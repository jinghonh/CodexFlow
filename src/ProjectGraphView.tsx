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
type RelationReview = { relationId: string; projectId: string; decision: "pending" | "confirmed" | "rejected";
  revision: number; confirmedEvidenceVersion: string | null };
type ReviewedRelation = InferredRelation & { evidenceVersion: string; evidenceValid: boolean; staleReason?: string | null; review: RelationReview };
type InferenceOutcome = { candidateId: string; status: string; unknownCount: number };
type Diagnostic = { threadId: string; sourceField: string; referencedThreadId: string; message: string };
type ProjectGraph = { project: { id: string; name: string }; nodes: GraphNode[]; relations: Relation[]; derivedRelations?: DerivedRelation[];
  inferredRelations?: InferredRelation[]; reviewedRelations?: ReviewedRelation[]; inferenceOutcomes?: InferenceOutcome[]; diagnostics: Diagnostic[] };
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

function reviewedRelations(graph: ProjectGraph): ReviewedRelation[] {
  return graph.reviewedRelations ?? (graph.inferredRelations ?? []).map((relation) => ({
    ...relation, evidenceVersion: relation.inputVersion, evidenceValid: true, staleReason: null,
    review: { relationId: relation.id, projectId: relation.projectId, decision: "pending", revision: 0, confirmedEvidenceVersion: null },
  }));
}

function confirmedWithCurrentEvidence(relation: ReviewedRelation): boolean {
  return relation.evidenceValid && relation.review.decision === "confirmed"
    && relation.review.confirmedEvidenceVersion === relation.evidenceVersion;
}

function visibleInferred(graph: ProjectGraph, minimumConfidence: number): InferredRelation[] {
  const byId = new Map(reviewedRelations(graph).map((relation) => [relation.id, relation]));
  return (graph.inferredRelations ?? []).filter((relation) => {
    const reviewed = byId.get(relation.id);
    return reviewed?.review.decision !== "rejected" && (!reviewed || reviewed.evidenceValid)
      && (relation.confidence >= minimumConfidence || (minimumConfidence === 0.7 && reviewed && confirmedWithCurrentEvidence(reviewed)));
  });
}

function reviewStatus(relation: ReviewedRelation): string {
  if (relation.review.decision === "rejected") return "已拒绝";
  if (relation.review.decision === "confirmed") {
    if (!relation.evidenceValid) return "已确认；当前关系未验证或证据已过期";
    return confirmedWithCurrentEvidence(relation) ? "已确认" : "已确认；当前证据尚未确认";
  }
  return relation.evidenceValid ? "待裁决" : "当前关系未验证或证据已过期";
}

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

function filteredGraph(graph: ProjectGraph, visibleThreadIds: Set<string> | undefined, source: string, kind: string, minimumConfidence: number): ProjectGraph {
  const visible = (id: string) => !visibleThreadIds || visibleThreadIds.has(id);
  const observed = source === "all" || source === "observed";
  const derived = source === "all" || source === "derived";
  const inferred = source === "all" || source === "inferred";
  const relations = observed ? graph.relations.filter((item) => (kind === "all" || kind === item.kind) && visible(item.toThreadId) && (visible(item.fromThreadId) || graph.nodes.some((node) => node.id === item.fromThreadId && node.referenceOnly))) : [];
  const referenceIds = new Set(relations.map((item) => item.fromThreadId));
  return {
    ...graph,
    nodes: graph.nodes.filter((node) => node.referenceOnly ? referenceIds.has(node.id) : visible(node.id)),
    relations,
    derivedRelations: derived ? (graph.derivedRelations ?? []).filter((item) => (kind === "all" || kind === item.kind) && visible(item.fromThreadId) && visible(item.toThreadId)) : [],
    inferredRelations: inferred ? visibleInferred(graph, minimumConfidence).filter((item) => (kind === "all" || kind === item.kind) && visible(item.fromThreadId) && visible(item.toThreadId)) : [],
  };
}

async function layoutGraph(graph: ProjectGraph): Promise<{ nodes: Node<GraphNodeData>[]; edges: Edge[]; warning: string }> {
  const visibleRelations = graph.inferredRelations ?? [];
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
      edges: [...graph.relations, ...(graph.derivedRelations ?? []), ...visibleRelations].map((relation) => ({
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
  for (const relation of visibleRelations) {
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

export function ProjectGraphView({ projectId, refreshVersion, onSelectEvidence, selectedThreadId = null, onSelectThread, visibleThreadIds, relationSource = "all", onRelationSourceChange, relationKind = "all", onRelationKindChange, minimumConfidence = 0.7, onMinimumConfidenceChange }: { projectId: string; refreshVersion: number;
  onSelectEvidence?: (evidence: Evidence) => void; selectedThreadId?: string | null; onSelectThread?: (id: string) => void;
  visibleThreadIds?: Set<string>; relationSource?: string; onRelationSourceChange?: (value: string) => void;
  relationKind?: string; onRelationKindChange?: (value: string) => void; minimumConfidence?: number; onMinimumConfidenceChange?: (value: number) => void }) {
  const [graph, setGraph] = useState<ProjectGraph | null>(null);
  const [layout, setLayout] = useState<{ nodes: Node<GraphNodeData>[]; edges: Edge[]; warning: string }>({ nodes: [], edges: [], warning: "" });
  const [selection, setSelection] = useState<Selection>(null);
  const [error, setError] = useState("");
  const [decisionError, setDecisionError] = useState("");
  const [savingDecision, setSavingDecision] = useState(false);
  const [localMinimumConfidence, setLocalMinimumConfidence] = useState(minimumConfidence);
  const threshold = onMinimumConfidenceChange ? minimumConfidence : localMinimumConfidence;
  const changeThreshold = (value: number) => onMinimumConfidenceChange ? onMinimumConfidenceChange(value) : setLocalMinimumConfidence(value);
  useEffect(() => { setSelection(null); }, [selectedThreadId]);
  useEffect(() => {
    let active = true;
    setGraph(null);
    setError("");
    invoke<ProjectGraph>("get_project_graph", { projectId })
      .then((next) => {
        if (!active) return;
        setGraph(next);
        setSelection((current) => current &&
          (current.type === "edge" ? [...next.relations, ...(next.derivedRelations ?? []), ...reviewedRelations(next)].some((relation) => relation.id === current.id) : next.nodes.some((node) => node.id === current.id))
          ? current : null);
      })
      .catch((cause) => { if (active) setError(typeof cause?.message === "string" ? cause.message : "关系图读取失败。请重试刷新。"); });
    return () => { active = false; };
  }, [projectId, refreshVersion]);

  useEffect(() => {
    if (!graph) return;
    let active = true;
    layoutGraph(filteredGraph(graph, visibleThreadIds, relationSource, relationKind, threshold))
      .then((positioned) => { if (active) setLayout(positioned); });
    return () => { active = false; };
  }, [graph, visibleThreadIds, relationSource, relationKind, threshold]);

  const detail = useMemo(() => {
    const current = selection ?? (selectedThreadId ? { type: "node" as const, id: selectedThreadId } : null);
    if (!graph || !current) return null;
    if (current.type === "edge") {
      const relation = graph.relations.find((item) => item.id === current.id);
      if (relation) return { type: "edge" as const, relation };
      const derived = graph.derivedRelations?.find((item) => item.id === current.id);
      if (derived) return { type: "derived" as const, relation: derived };
      const inferred = reviewedRelations(graph).find((item) => item.id === current.id);
      return inferred ? { type: "inferred" as const, relation: inferred } : null;
    }
    const node = graph.nodes.find((item) => item.id === current.id);
    return node ? { type: "node" as const, node } : null;
  }, [graph, selection, selectedThreadId]);

  async function decide(relation: ReviewedRelation, decision: RelationReview["decision"]) {
    setSavingDecision(true);
    setDecisionError("");
    try {
      await invoke<RelationReview>("decide_inferred_relation", {
        projectId, relationId: relation.id, decision, expectedRevision: relation.review.revision,
        expectedEvidenceVersion: relation.evidenceVersion,
      });
      const next = await invoke<ProjectGraph>("get_project_graph", { projectId });
      setGraph(next);
    } catch (cause) {
      const failure = cause as { code?: string; message?: string };
      setDecisionError(failure?.code === "CONCURRENT_MODIFICATION"
        ? "关系裁决已被更新，请刷新关系图后重试。"
        : failure?.message || "保存关系裁决失败，请刷新后重试。");
    } finally {
      setSavingDecision(false);
    }
  }

  async function refreshGraph() {
    setDecisionError("");
    try {
      const next = await invoke<ProjectGraph>("get_project_graph", { projectId });
      setGraph(next);
    } catch (cause) {
      setDecisionError((cause as { message?: string })?.message || "刷新关系图失败。");
    }
  }

  return <section id="relations" className="panel graph-panel">
    <div className="panel-kicker">04 / 项目关系图</div>
    <h2>{graph?.project.name ?? "项目关系"}</h2>
    <p className="panel-intro">观察关系箭头从来源父会话指向后续会话；灰色虚线规则关系按稳定端点展示事实交集。紫色推断关系由 Jev 判断并附双方证据；因果箭头从前序指向后续。</p>
    {error && <div className="page-error" role="alert">{error}</div>}
    {!graph && !error && <p className="empty-list">正在读取项目关系图…</p>}
    {graph && <>
      <div className="graph-summary"><strong>{layout.nodes.filter((node) => !node.data.referenceOnly).length} 条可见会话</strong><span>{layout.edges.length} 条可见关系</span><span>{graph.relations.length} 条观察关系</span><span>{graph.derivedRelations?.length ?? 0} 条规则关系</span><span>{visibleInferred(graph, 0.7).length} 条默认显示的推断关系</span></div>
      <div className="explorer-filters" aria-label="关系过滤"><label>来源类别<select aria-label="按来源类别过滤关系" value={relationSource} onChange={(event) => onRelationSourceChange?.(event.target.value)}><option value="all">全部</option><option value="observed">观察</option><option value="derived">规则</option><option value="inferred">推断</option></select></label><label>关系类型<select aria-label="按关系类型过滤" value={relationKind} onChange={(event) => onRelationKindChange?.(event.target.value)}><option value="all">全部类型</option>{[...new Set([...graph.relations, ...(graph.derivedRelations ?? []), ...(graph.inferredRelations ?? [])].map((item) => item.kind))].sort().map((kind) => <option value={kind} key={kind}>{kind}</option>)}</select></label><label>最低置信度<select aria-label="最低关系置信度" value={threshold} onChange={(event) => changeThreshold(Number(event.target.value))}><option value={0}>全部</option><option value={0.5}>0.50</option><option value={0.7}>0.70（默认）</option><option value={0.9}>0.90</option></select></label></div>
      <label className="graph-confidence-toggle"><input type="checkbox" checked={threshold < 0.7} onChange={(event) => changeThreshold(event.target.checked ? 0 : 0.7)} />查看低于 0.70 的推断关系（{(graph.inferredRelations ?? []).filter((item) => item.confidence < 0.70).length}）</label>
      {reviewedRelations(graph).filter((relation) => relation.review.decision === "rejected" || !relation.evidenceValid).length > 0 &&
        <div className="graph-review-list"><strong>已拒绝或过期的推断关系</strong>
          {reviewedRelations(graph).filter((relation) => relation.review.decision === "rejected" || !relation.evidenceValid)
            .map((relation) => <button className="browse-button" key={relation.id} onClick={() => setSelection({ type: "edge", id: relation.id })}>
              {inferredText[relation.kind]} · {reviewStatus(relation)} · {relation.staleReason ?? "可查看旧证据"} · {relation.fromThreadId} ↔ {relation.toThreadId}
            </button>)}
        </div>}
      {(graph.inferenceOutcomes ?? []).length > 0 && <p className="analysis-note">候选判断：无关系 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "none").length}，无法判断 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "undetermined").length}，证据不足 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "insufficientEvidence").length}；这些结果不生成图边。</p>}
      <button className="browse-button" onClick={() => void refreshGraph()}>自动布局</button>
      {layout.warning && <div className="graph-layout-warning" role="status">{layout.warning}</div>}
      {layout.nodes.length ? <div className="graph-canvas" aria-label="项目结构关系图">
        <ReactFlow nodes={layout.nodes.map((node) => ({ ...node, selected: node.id === selectedThreadId }))} edges={layout.edges} nodeTypes={nodeTypes} fitView fitViewOptions={{ padding: 0.18 }}
          nodesDraggable={false} onNodeClick={(_, node) => { setSelection({ type: "node", id: node.id }); if (!node.data.referenceOnly) onSelectThread?.(node.id); }}
          onEdgeClick={(_, edge) => setSelection({ type: "edge", id: edge.id })}
          onPaneClick={() => setSelection(null)} minZoom={0.15} maxZoom={2}>
          <Background gap={22} size={1} />
          <Controls showInteractive={false} />
        </ReactFlow>
      </div> : <p className="empty-list">{graph.nodes.length ? "当前条件没有可见会话；已选会话详情仍会保留。" : "当前项目没有会话。"}</p>}
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
          <p role="status">{detail.relation.evidenceValid ? "当前分析有效" : `旧分析过期：${detail.relation.staleReason ?? "来源证据已变化"}；以下保留旧证据供检查。`}</p>
          {!detail.relation.evidenceValid && <button className="browse-button" onClick={() => document.getElementById("project-analysis")?.scrollIntoView({ behavior: "smooth" })}>前往重新分析</button>}
          <p><code>{detail.relation.fromThreadId}</code> {["RELATED", "ALTERNATIVE_TO"].includes(detail.relation.kind) ? "↔" : "→"} <code>{detail.relation.toThreadId}</code></p>
          <p>{detail.relation.explanation}</p>
          <dl><dt>来源</dt><dd>Jev · {detail.relation.actualModel}（请求 {detail.relation.requestedModel}）</dd><dt>关系判断置信度</dt><dd>{detail.relation.confidence.toFixed(2)}</dd>
            <dt>关系选项概率</dt><dd>{Object.entries(detail.relation.probabilities).map(([key, value]) => `${key} ${value.toFixed(2)}`).join("、")}</dd>
            <dt>证据选择置信度</dt><dd>{detail.relation.evidenceConfidence.toFixed(2)}</dd>
            <dt>证据选项概率</dt><dd>{Object.entries(detail.relation.evidenceProbabilities).map(([key, value]) => `${key} ${value.toFixed(2)}`).join("、")}</dd>
            <dt>时间核对</dt><dd>{["RELATED", "ALTERNATIVE_TO"].includes(detail.relation.kind) ? "无向关系不检查顺序" : detail.relation.timeCheck === "verified" ? "前后顺序已由证据回合验证" : "来源时间不足，无法验证顺序"}</dd>
            <dt>裁决状态</dt><dd>{reviewStatus(detail.relation)}</dd><dt>修订号</dt><dd>{detail.relation.review.revision}</dd></dl>
          {decisionError && <div className="page-error" role="alert">{decisionError}<button className="browse-button" onClick={refreshGraph}>刷新关系图</button></div>}
          <div className="graph-review-actions">
            <button className="browse-button" disabled={savingDecision || !detail.relation.evidenceValid || confirmedWithCurrentEvidence(detail.relation)} onClick={() => decide(detail.relation, "confirmed")}>确认关系</button>
            <button className="browse-button" disabled={savingDecision || detail.relation.review.decision === "rejected"} onClick={() => decide(detail.relation, "rejected")}>拒绝关系</button>
            <button className="browse-button" disabled={savingDecision || detail.relation.review.decision === "pending"} onClick={() => decide(detail.relation, "pending")}>恢复待裁决</button>
          </div>
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
