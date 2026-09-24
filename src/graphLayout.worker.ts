import ELK from "elkjs/lib/elk.bundled.js";

const elk = new ELK();

self.onmessage = async (event: MessageEvent<{ nodes: string[]; edges: { id: string; source: string; target: string }[] }>) => {
  const { nodes, edges } = event.data;
  try {
    const result = await elk.layout({
      id: "project",
      layoutOptions: {
        "elk.algorithm": "layered",
        "elk.direction": "RIGHT",
        "elk.spacing.nodeNode": "58",
        "elk.layered.spacing.nodeNodeBetweenLayers": "105",
      },
      children: nodes.map((id) => ({ id, width: 224, height: 82 })),
      edges: edges.map(({ id, source, target }) => ({ id, sources: [source], targets: [target] })),
    });
    self.postMessage({ positions: result.children?.map(({ id, x, y }) => ({ id, x, y })) ?? [] });
  } catch {
    self.postMessage({ error: true });
  }
};
