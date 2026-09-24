// Run with `node scripts/measure_graph_layout.mjs GRAPH_JSON 20`.
// This times production ELK inputs; it does not time WebView painting.
import { readFileSync } from "node:fs";
import ELK from "elkjs/lib/elk.bundled.js";

const [path, countText] = process.argv.slice(2);
const count = Number(countText);
if (!path || !Number.isInteger(count) || count < 20) {
  throw new Error("用法：node scripts/measure_graph_layout.mjs GRAPH_JSON RUNS（至少 20 次）");
}
const graph = JSON.parse(readFileSync(path, "utf8"));
const relations = [...graph.relations, ...(graph.derivedRelations ?? []), ...(graph.inferredRelations ?? [])];
if (graph.nodes.length !== 500 || relations.length > 3000 || relations.length < 2500) {
  throw new Error(`验收图规模不符：${graph.nodes.length} 个节点，${relations.length} 条关系`);
}
const input = {
  id: "project",
  layoutOptions: {
    "elk.algorithm": "layered",
    "elk.direction": "RIGHT",
    "elk.spacing.nodeNode": "58",
    "elk.layered.spacing.nodeNodeBetweenLayers": "105",
  },
  children: graph.nodes.map((node) => ({ id: node.id, width: 224, height: 82 })),
  edges: [],
};
const parent = new Map(graph.nodes.map((node) => [node.id, node.id]));
const root = (id) => {
  let current = id;
  while (parent.get(current) !== current) current = parent.get(current);
  return current;
};
for (const relation of relations) {
  const left = root(relation.fromThreadId);
  const right = root(relation.toThreadId);
  if (left === right) continue;
  parent.set(right, left);
  input.edges.push({ id: relation.id, sources: [relation.fromThreadId], targets: [relation.toThreadId] });
}
const elk = new ELK();
const values = [];
for (let run = 0; run < count; run++) {
  const start = performance.now();
  const result = await elk.layout(input);
  const ms = performance.now() - start;
  if (result.children?.length !== graph.nodes.length) throw new Error("布局丢失节点");
  values.push(ms);
  console.log(`run=${run} nodes=${result.children.length} visible_edges=${relations.length} layout_edges=${input.edges.length} layout_ms=${ms.toFixed(3)}`);
}
values.sort((a, b) => a - b);
console.log(`layout: min=${values[0].toFixed(3)} median=${values[Math.floor(count / 2)].toFixed(3)} p95=${values[Math.ceil(count * 0.95) - 1].toFixed(3)} max=${values[count - 1].toFixed(3)}`);
