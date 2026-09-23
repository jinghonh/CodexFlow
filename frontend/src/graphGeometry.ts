import type { GraphEdge, GraphNode, NodeLayout } from "./types";
export const NODE_WIDTH = 220;
export const NODE_HEIGHT = 116;
export const relationNames: Record<string, string> = { continues: "延续", depends_on: "依赖", implements: "实现", reviewed_by: "由其审查", fixes: "修复", references: "引用", related_to: "相关" };
export const statusNames: Record<string, string> = { none: "未设置", active: "进行中", done: "已完成", blocked: "阻塞" };

// 按连通分量成组；输入排序固定，筛选不参与布局，保持空间位置稳定。
export function automaticPositions(nodes: GraphNode[], edges: GraphEdge[]): Map<string, NodeLayout> {
  const ids = new Set(nodes.filter(node => !node.hidden).map(node => node.id));
  const neighbors = new Map([...ids].map(id => [id, new Set<string>()]));
  for (const edge of edges) if (ids.has(edge.source) && ids.has(edge.target)) { neighbors.get(edge.source)!.add(edge.target); neighbors.get(edge.target)!.add(edge.source); }
  const seen = new Set<string>(); const positions = new Map<string, NodeLayout>(); let top = 40;
  for (const id of [...ids].sort()) {
    if (seen.has(id)) continue;
    const queue = [id]; seen.add(id); const group: string[] = [];
    for (let i = 0; i < queue.length; i++) { const current = queue[i]; group.push(current); for (const next of [...neighbors.get(current)!].sort()) if (!seen.has(next)) { seen.add(next); queue.push(next); } }
    group.forEach((member, index) => positions.set(member, { x: 40 + index % 3 * 320, y: top + Math.floor(index / 3) * 180 }));
    top += Math.ceil(group.length / 3) * 180 + (group.length > 1 ? 60 : 0);
  }
  return positions;
}
export function edgeGeometry(source: NodeLayout, target: NodeLayout, offset = 0) {
  const a = { x: source.x + NODE_WIDTH / 2, y: source.y + NODE_HEIGHT / 2 };
  const b = { x: target.x + NODE_WIDTH / 2, y: target.y + NODE_HEIGHT / 2 };
  const dx = b.x - a.x, dy = b.y - a.y, distance = Math.hypot(dx, dy) || 1;
  const control = { x: (a.x + b.x) / 2 - dy / distance * offset, y: (a.y + b.y) / 2 + dx / distance * offset };
  function boundary(center: NodeLayout, toward: NodeLayout) { const x = toward.x - center.x, y = toward.y - center.y; const scale = Math.min((NODE_WIDTH / 2 + 7) / (Math.abs(x) || .001), (NODE_HEIGHT / 2 + 7) / (Math.abs(y) || .001)); return { x: center.x + x * scale, y: center.y + y * scale }; }
  const start = boundary(a, control), end = boundary(b, control);
  return { path: `M ${start.x} ${start.y} Q ${control.x} ${control.y} ${end.x} ${end.y}`, label: { x: (start.x + 2 * control.x + end.x) / 4, y: (start.y + 2 * control.y + end.y) / 4 } };
}
