import { useEffect, useMemo, useState } from "react";
import {
  ReactFlow, Background, Controls, Handle, MarkerType, Position, type Edge, type Node,
  type NodeProps,
} from "@xyflow/react";
import { invoke } from "@tauri-apps/api/core";
import ELK from "elkjs/lib/elk.bundled.js";
import "@xyflow/react/dist/style.css";

type GraphNode = { id: string; title: string | null; referenceOnly: boolean };
type Relation = {
  id: string; projectId: string; fromThreadId: string; toThreadId: string;
  kind: "FORKED_FROM" | "SUBAGENT_OF"; source: "observed"; sourceField: string;
  confidence: number; parentEndpoint: "inProject" | "missing" | "outsideProject" | "unassigned";
};
type Evidence = { id: string; threadId: string; turnId: string; itemId: string; excerpt: string; contentVersion: string };
type DerivedRelation = { id: string; projectId: string; fromThreadId: string; toThreadId: string;
  kind: "SHARED_FILE" | "SHARED_ARTIFACT" | "EXPLICIT_REFERENCE"; source: "derived"; basis: string; evidence: Evidence[] };
type InferredRelation = { id: string; projectId: string; candidateId: string; fromThreadId: string; toThreadId: string;
  kind: "CONTINUES" | "IMPLEMENTS" | "FIXES" | "VALIDATES" | "INVESTIGATES" | "ALTERNATIVE_TO" | "SUPERSEDES" | "MOTIVATED_BY" | "RELATED";
  source: "jev"; requestedModel: string; actualModel: string; confidence: number; probabilities: Record<string, number>;
  evidenceConfidence: number; evidenceProbabilities: Record<string, number>; evidence: { id: string; left: Evidence; right: Evidence };
  timeCheck: "verified" | "unverifiable"; explanation: string; inputVersion: string };
type InferenceOutcome = { candidateId: string; status: string; unknownCount: number };
type Diagnostic = { threadId: string; sourceField: string; referencedThreadId: string; message: string };
type ProjectGraph = { project: { id: string; name: string }; nodes: GraphNode[]; relations: Relation[]; derivedRelations?: DerivedRelation[];
  inferredRelations?: InferredRelation[]; inferenceOutcomes?: InferenceOutcome[]; diagnostics: Diagnostic[] };
type GraphNodeData = { title: string; id: string; referenceOnly: boolean };
type Selection = { type: "node" | "edge"; id: string } | null;

const elk = new ELK();
const nodeWidth = 224;
const nodeHeight = 82;
const endpointText: Record<Relation["parentEndpoint"], string> = {
  inProject: "父会话属于当前项目",
  missing: "父会话尚未出现在本机缓存；仅保留来源引用。",
  outsideProject: "父会话属于其他项目；不展开跨项目内容。",
  unassigned: "父会话的项目归属尚未确认；仅保留来源引用。",
};
const kindText: Record<Relation["kind"], string> = {
  FORKED_FROM: "派生",
  SUBAGENT_OF: "子代理",
};
const sourceText: Record<Relation["source"], string> = { observed: "观察" };
const derivedText: Record<DerivedRelation["kind"], string> = { SHARED_FILE: "共同文件", SHARED_ARTIFACT: "共同产物", EXPLICIT_REFERENCE: "明确引用" };
const inferredText: Record<InferredRelation["kind"], string> = {
  CONTINUES: "延续", IMPLEMENTS: "落实", FIXES: "修复", VALIDATES: "验证", INVESTIGATES: "调查",
  ALTERNATIVE_TO: "替代方案", SUPERSEDES: "取代", MOTIVATED_BY: "促成", RELATED: "相关",
};

function ThreadNode({ data }: NodeProps<Node<GraphNodeData>>) {
  return <div className={`graph-node ${data.referenceOnly ? "graph-node-reference" : ""}`}>
    <Handle type="target" position={Position.Left} id="fork" style={{ top: "35%" }} />
    <Handle type="target" position={Position.Left} id="subagent" style={{ top: "65%" }} />
    <strong>{data.referenceOnly ? "仅有父会话引用" : data.title}</strong>
    <small>{data.id}</small>
    <Handle type="source" position={Position.Right} id="fork" style={{ top: "35%" }} />
    <Handle type="source" position={Position.Right} id="subagent" style={{ top: "65%" }} />
  </div>;
}

const nodeTypes = { thread: ThreadNode };

async function layoutGraph(graph: ProjectGraph, showLowConfidence: boolean): Promise<{ nodes: Node<GraphNodeData>[]; edges: Edge[]; warning: string }> {
  const visibleInferred = (graph.inferredRelations ?? []).filter((relation) => showLowConfidence || relation.confidence >= 0.70);
  let coordinates = new Map<string, { x?: number; y?: number }>();
  let warning = "";
  try {
    const result = await elk.layout({
      id: "project",
      layoutOptions: {
        "elk.algorithm": "layered",
        "elk.direction": "RIGHT",
        "elk.spacing.nodeNode": "58",
        "elk.layered.spacing.nodeNodeBetweenLayers": "105",
      },
      children: graph.nodes.map((node) => ({ id: node.id, width: nodeWidth, height: nodeHeight })),
      edges: [...graph.relations, ...(graph.derivedRelations ?? []), ...visibleInferred].map((relation) => ({
        id: relation.id, sources: [relation.fromThreadId], targets: [relation.toThreadId],
      })),
    });
    coordinates = new Map(result.children?.map((node) => [node.id, node]));
  } catch {
    warning = "自动布局未完成，已按固定顺序展示全部会话和关系。";
  }
  const nodes: Node<GraphNodeData>[] = graph.nodes.map((node, index) => ({
    id: node.id,
    type: "thread",
    position: { x: coordinates.get(node.id)?.x ?? (index % 4) * 320, y: coordinates.get(node.id)?.y ?? Math.floor(index / 4) * 150 },
    data: { title: node.title || node.id, id: node.id, referenceOnly: node.referenceOnly },
    draggable: false,
  }));
  const edges: Edge[] = graph.relations.map((relation) => {
    const fork = relation.kind === "FORKED_FROM";
    const color = fork ? "#287047" : "#456d9a";
    return {
      id: relation.id,
      source: relation.fromThreadId,
      target: relation.toThreadId,
      sourceHandle: fork ? "fork" : "subagent",
      targetHandle: fork ? "fork" : "subagent",
      type: "smoothstep",
      label: `${sourceText[relation.source]} · ${kindText[relation.kind]} · ${relation.confidence.toFixed(1)}`,
      labelStyle: { fill: color, fontSize: 11, fontWeight: 650 },
      labelBgStyle: { fill: "var(--paper)", fillOpacity: 0.95 },
      labelBgPadding: [7, 4],
      style: { stroke: color, strokeWidth: 2 },
      markerEnd: { type: MarkerType.ArrowClosed, color },
    };
  });
  for (const relation of graph.derivedRelations ?? []) {
    edges.push({
      id: relation.id, source: relation.fromThreadId, target: relation.toThreadId,
      type: "smoothstep", label: `规则 · ${derivedText[relation.kind]}`,
      labelStyle: { fill: "#777f81", fontSize: 11, fontWeight: 650 },
      labelBgStyle: { fill: "var(--paper)", fillOpacity: 0.95 }, labelBgPadding: [7, 4],
      style: { stroke: "#8c9597", strokeWidth: 2, strokeDasharray: "5 4" },
    });
  }
  for (const relation of visibleInferred) {
    const color = "#8b5ea8";
    edges.push({
      id: relation.id, source: relation.fromThreadId, target: relation.toThreadId,
      type: "smoothstep", label: `推断 · ${inferredText[relation.kind]} · ${relation.confidence.toFixed(2)}`,
      labelStyle: { fill: color, fontSize: 11, fontWeight: 650 },
      labelBgStyle: { fill: "var(--paper)", fillOpacity: 0.95 }, labelBgPadding: [7, 4],
      style: { stroke: color, strokeWidth: 2 },
      ...(["RELATED", "ALTERNATIVE_TO"].includes(relation.kind) ? {} : { markerEnd: { type: MarkerType.ArrowClosed, color } }),
    });
  }
  return { nodes, edges, warning };
}

export function ProjectGraphView({ projectId, refreshVersion, onSelectEvidence }: { projectId: string; refreshVersion: number;
  onSelectEvidence?: (evidence: Evidence) => void }) {
  const [graph, setGraph] = useState<ProjectGraph | null>(null);
  const [layout, setLayout] = useState<{ nodes: Node<GraphNodeData>[]; edges: Edge[]; warning: string }>({ nodes: [], edges: [], warning: "" });
  const [selection, setSelection] = useState<Selection>(null);
  const [error, setError] = useState("");
  const [showLowConfidence, setShowLowConfidence] = useState(false);
  useEffect(() => {
    let active = true;
    setGraph(null);
    setError("");
    invoke<ProjectGraph>("get_project_graph", { projectId })
      .then(async (next) => {
        const positioned = await layoutGraph(next, showLowConfidence);
        if (active) {
          setGraph(next);
          setLayout(positioned);
          setSelection((current) => current &&
            (current.type === "edge" ? [...next.relations, ...(next.derivedRelations ?? []), ...(next.inferredRelations ?? [])].some((relation) => relation.id === current.id) : next.nodes.some((node) => node.id === current.id))
            ? current : null);
        }
      })
      .catch((cause) => { if (active) setError(typeof cause?.message === "string" ? cause.message : "关系图读取失败。请重试刷新。"); });
    return () => { active = false; };
  }, [projectId, refreshVersion, showLowConfidence]);

  const detail = useMemo(() => {
    if (!graph || !selection) return null;
    if (selection.type === "edge") {
      const relation = graph.relations.find((item) => item.id === selection.id);
      if (relation) return { type: "edge" as const, relation };
      const derived = graph.derivedRelations?.find((item) => item.id === selection.id);
      if (derived) return { type: "derived" as const, relation: derived };
      const inferred = graph.inferredRelations?.find((item) => item.id === selection.id);
      return inferred ? { type: "inferred" as const, relation: inferred } : null;
    }
    const node = graph.nodes.find((item) => item.id === selection.id);
    return node ? { type: "node" as const, node } : null;
  }, [graph, selection]);

  return <section id="relations" className="panel graph-panel">
    <div className="panel-kicker">04 / 项目关系图</div>
    <h2>{graph?.project.name ?? "项目关系"}</h2>
    <p className="panel-intro">观察关系箭头从来源父会话指向后续会话；灰色虚线规则关系按稳定端点展示事实交集。紫色推断关系由 Jev 判断并附双方证据；因果箭头从前序指向后续。</p>
    {error && <div className="page-error" role="alert">{error}</div>}
    {!graph && !error && <p className="empty-list">正在读取项目关系图…</p>}
    {graph && <>
      <div className="graph-summary"><strong>{graph.nodes.filter((node) => !node.referenceOnly).length} 条会话</strong><span>{graph.relations.length} 条观察关系</span><span>{graph.derivedRelations?.length ?? 0} 条规则关系</span><span>{(graph.inferredRelations ?? []).filter((item) => item.confidence >= 0.70).length} 条默认显示的推断关系</span><span>{graph.nodes.filter((node) => node.referenceOnly).length} 个仅有引用的端点</span></div>
      <label className="graph-confidence-toggle"><input type="checkbox" checked={showLowConfidence} onChange={(event) => setShowLowConfidence(event.target.checked)} />查看低于 0.70 的推断关系（{(graph.inferredRelations ?? []).filter((item) => item.confidence < 0.70).length}）</label>
      {(graph.inferenceOutcomes ?? []).length > 0 && <p className="analysis-note">候选判断：无关系 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "none").length}，无法判断 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "undetermined").length}，证据不足 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "insufficientEvidence").length}；这些结果不生成图边。</p>}
      {layout.warning && <div className="graph-layout-warning" role="status">{layout.warning}</div>}
      {graph.nodes.length ? <div className="graph-canvas" aria-label="项目结构关系图">
        <ReactFlow nodes={layout.nodes} edges={layout.edges} nodeTypes={nodeTypes} fitView fitViewOptions={{ padding: 0.18 }}
          nodesDraggable={false} onNodeClick={(_, node) => setSelection({ type: "node", id: node.id })}
          onEdgeClick={(_, edge) => setSelection({ type: "edge", id: edge.id })}
          onPaneClick={() => setSelection(null)} minZoom={0.15} maxZoom={2}>
          <Background gap={22} size={1} />
          <Controls showInteractive={false} />
        </ReactFlow>
      </div> : <p className="empty-list">当前项目没有会话。</p>}
      <div className="graph-details" aria-live="polite">
        {!detail && <p>选择节点或关系以查看来源详情。</p>}
        {detail?.type === "node" && <><h3>{detail.node.referenceOnly ? "仅有引用的端点" : "会话"}</h3><strong>{detail.node.title ?? detail.node.id}</strong><code>{detail.node.id}</code>{detail.node.referenceOnly && <p>来源记录了此会话 ID，当前项目图没有可展示的会话内容。</p>}</>}
        {detail?.type === "derived" && <><h3>规则关系 · {derivedText[detail.relation.kind]}</h3>
          <p><code>{detail.relation.fromThreadId}</code> ↔ <code>{detail.relation.toThreadId}</code></p>
          <p>{detail.relation.basis}</p><p>这是来源事实计算结果，不是模型判断或概率。</p>
          {detail.relation.evidence.map((evidence) => <div key={evidence.id} className="graph-proof">
            <blockquote>{evidence.excerpt}</blockquote><small>会话 {evidence.threadId} · 回合 {evidence.turnId} · 条目 {evidence.itemId} · 内容版本 {evidence.contentVersion.slice(0, 12)}</small>
            <button className="browse-button" onClick={() => onSelectEvidence?.(evidence)}>定位来源条目</button>
          </div>)}
        </>}
        {detail?.type === "inferred" && <><h3>推断关系 · {inferredText[detail.relation.kind]}</h3>
          <p><code>{detail.relation.fromThreadId}</code> {["RELATED", "ALTERNATIVE_TO"].includes(detail.relation.kind) ? "↔" : "→"} <code>{detail.relation.toThreadId}</code></p>
          <p>{detail.relation.explanation}</p>
          <dl><dt>来源</dt><dd>Jev · {detail.relation.actualModel}（请求 {detail.relation.requestedModel}）</dd><dt>关系判断置信度</dt><dd>{detail.relation.confidence.toFixed(2)}</dd>
            <dt>关系选项概率</dt><dd>{Object.entries(detail.relation.probabilities).map(([key, value]) => `${key} ${value.toFixed(2)}`).join("、")}</dd>
            <dt>证据选择置信度</dt><dd>{detail.relation.evidenceConfidence.toFixed(2)}</dd>
            <dt>证据选项概率</dt><dd>{Object.entries(detail.relation.evidenceProbabilities).map(([key, value]) => `${key} ${value.toFixed(2)}`).join("、")}</dd>
            <dt>时间核对</dt><dd>{["RELATED", "ALTERNATIVE_TO"].includes(detail.relation.kind) ? "无向关系不检查顺序" : detail.relation.timeCheck === "verified" ? "前后顺序已由证据回合验证" : "来源时间不足，无法验证顺序"}</dd>
            <dt>覆盖状态</dt><dd>自动推断；尚无用户裁决</dd></dl>
          <small>Choice 分数表示本次判定的确定性，不等于关系正确率。</small>
          {[detail.relation.evidence.left, detail.relation.evidence.right].map((evidence) => <div key={evidence.id} className="graph-proof">
            <blockquote>{evidence.excerpt}</blockquote><small>会话 {evidence.threadId} · 回合 {evidence.turnId} · 条目 {evidence.itemId} · 内容版本 {evidence.contentVersion.slice(0, 12)}</small>
            <button className="browse-button" onClick={() => onSelectEvidence?.(evidence)}>定位来源条目</button>
          </div>)}
        </>}
        {detail?.type === "edge" && <><h3>{sourceText[detail.relation.source]}关系 · {kindText[detail.relation.kind]}</h3>
          <p><code>{detail.relation.fromThreadId}</code> → <code>{detail.relation.toThreadId}</code></p>
          <dl><dt>含义</dt><dd>{detail.relation.kind === "FORKED_FROM" ? "后续会话从来源父会话派生。" : "后续会话是来源父会话的子代理。"}</dd><dt>关系类型</dt><dd><code>{detail.relation.kind}</code></dd><dt>来源</dt><dd>{sourceText[detail.relation.source]} · 来源字段 <code>{detail.relation.sourceField}</code></dd><dt>置信度</dt><dd>{detail.relation.confidence.toFixed(1)}</dd><dt>端点</dt><dd>{endpointText[detail.relation.parentEndpoint]}</dd></dl>
        </>}
      </div>
      {(graph.diagnostics.length > 0 || graph.relations.some((item) => item.parentEndpoint !== "inProject")) && <div className="graph-diagnostics"><strong>来源诊断</strong>
        {graph.relations.filter((item) => item.parentEndpoint !== "inProject").map((item) => <p key={item.id}><code>{item.toThreadId}</code> 的 <code>{item.sourceField}</code> 指向 <code>{item.fromThreadId}</code>：{endpointText[item.parentEndpoint]}</p>)}
        {graph.diagnostics.map((item) => <p key={`${item.threadId}:${item.sourceField}`}><code>{item.threadId}</code> 的 <code>{item.sourceField}</code>：{item.message}</p>)}
      </div>}
    </>}
  </section>;
}
