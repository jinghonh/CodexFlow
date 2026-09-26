import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  ReactFlow, Background, Controls, Handle, MarkerType, Position, type Edge, type Node,
  type NodeProps,
} from "@xyflow/react";
import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";
import { threadDisplayTitle } from "./threadDisplay";
import { loadProjectQuery, useProjectQueryCache } from "./projectQueryCache";
import "@xyflow/react/dist/style.css";

type GraphNode = { id: string; title: string | null; sourceKind?: string | null; referenceOnly: boolean };
type Relation = {
  id: string; projectId: string; fromThreadId: string; toThreadId: string;
  kind: "FORKED_FROM" | "SUBAGENT_OF"; source: "observed"; sourceField: string;
  confidence: number; parentEndpoint: "inProject" | "missing" | "outsideProject" | "unassigned";
};
type Evidence = { id: string; threadId: string; turnId: string; itemId: string; excerpt: string; contentVersion: string };
type EvidenceReference = Pick<Evidence, "threadId" | "turnId" | "itemId">;
type EvidenceOption = { key: string; pair: { id: string; left: Evidence; right: Evidence } };
type DerivedRelation = { id: string; projectId: string; fromThreadId: string; toThreadId: string;
  kind: "SHARED_FILE" | "SHARED_ARTIFACT" | "EXPLICIT_REFERENCE"; source: "derived"; basis: string; evidence: Evidence[] };
type InferredRelation = { id: string; projectId: string; candidateId: string; fromThreadId: string; toThreadId: string;
  kind: "CONTINUES" | "IMPLEMENTS" | "FIXES" | "VALIDATES" | "INVESTIGATES" | "ALTERNATIVE_TO" | "SUPERSEDES" | "MOTIVATED_BY" | "RELATED";
  source: "jev"; requestedModel: string; actualModel: string; confidence: number; probabilities: Record<string, number>;
  evidenceConfidence: number; evidenceProbabilities: Record<string, number>; evidenceOptions?: EvidenceOption[];
  selectedEvidenceOption?: string | null; evidence: { id: string; left: Evidence; right: Evidence };
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
type GraphLayout = { nodes: Node<GraphNodeData>[]; edges: Edge[]; warning: string };

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

function probabilityLabel(key: string, evidence = false): string {
  if (key === "INSUFFICIENT") return "证据不足";
  if (key === "SUPPORTS") return "支持关系";
  if (key === "REJECTS") return "不支持关系";
  if (key === "UNKNOWN") return "无法判断";
  if (evidence && /^p\d+$/.test(key)) return `选项 ${key}`;
  return inferredText[key as InferredRelation["kind"]] ?? key;
}

function ProbabilityTable({ title, probabilities, evidence = false, selectedOption, options = [], onSelectEvidence }: {
  title: string; probabilities: Record<string, number>; evidence?: boolean; selectedOption?: string | null;
  options?: EvidenceOption[]; onSelectEvidence: (item: EvidenceReference) => void;
}) {
  const [expanded, setExpanded] = useState<string | null>(null);
  const rows = Object.entries(probabilities)
    .filter(([, probability]) => probability > 0)
    .sort(([leftKey, leftValue], [rightKey, rightValue]) => rightValue - leftValue || leftKey.localeCompare(rightKey));
  const hiddenZeros = Object.values(probabilities).filter((probability) => probability <= 0).length;
  const optionPairs = new Map(options.map((option) => [option.key, option.pair]));
  const highest = rows[0]?.[0] ?? null;

  return <section className="probability-section" aria-label={title}>
    <div className="probability-heading"><h4>{title}</h4><span>{hiddenZeros ? `已收起 ${hiddenZeros} 个零值` : "按概率排序"}</span></div>
    {evidence && options.length === 0 && rows.some(([key]) => /^p\d+$/.test(key)) &&
      <p className="probability-note">历史结果未保存选项映射；保留原始编号，不关联当前候选。</p>}
    <table className="probability-table">
      <thead><tr><th scope="col">选项</th><th scope="col">概率</th>{evidence && <th scope="col">证据</th>}</tr></thead>
      <tbody>{rows.length ? rows.map(([key, probability]) => {
        const pair = optionPairs.get(key);
        return <Fragment key={key}>
          <tr className={`${key === highest ? "probability-highest" : ""} ${key === selectedOption ? "probability-selected" : ""}`}>
            <th scope="row"><span>{probabilityLabel(key, evidence)}</span>{key === selectedOption && <small>模型选择</small>}</th>
            <td><div className="probability-value"><span className="probability-track"><span style={{ width: `${Math.max(2, probability * 100)}%` }} /></span><strong>{(probability * 100).toFixed(1)}%</strong></div></td>
            {evidence && <td>{pair ? <button className="probability-expand" aria-expanded={expanded === key} onClick={() => setExpanded(expanded === key ? null : key)}>{expanded === key ? "收起" : "查看"}<span>{expanded === key ? "−" : "+"}</span></button> : <span className="probability-source-muted">{key === "INSUFFICIENT" ? "无证据对" : "—"}</span>}</td>}
          </tr>
          {expanded === key && pair && <tr className="probability-evidence-row"><td colSpan={3}>
            <div className="probability-pair">
              {[pair.left, pair.right].map((item, index) => <article key={item.id}>
                <div><strong>{index === 0 ? "左侧证据" : "右侧证据"}</strong><button className="plain-button" onClick={() => onSelectEvidence(item)}>在会话中查看</button></div>
                <blockquote>{item.excerpt}</blockquote>
                <small>会话 {item.threadId} · 回合 {item.turnId} · 条目 {item.itemId}</small>
              </article>)}
            </div>
          </td></tr>}
        </Fragment>;
      }) : <tr><td className="probability-empty" colSpan={evidence ? 3 : 2}>没有非零选项</td></tr>}</tbody>
    </table>
  </section>;
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

function graphEdges(graph: ProjectGraph): Edge[] {
  const visibleRelations = graph.inferredRelations ?? [];
  const edges: Edge[] = graph.relations.map((relation) => {
    const fork = relation.kind === "FORKED_FROM";
    const color = fork ? "var(--graph-observed)" : "var(--graph-child)";
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
      labelStyle: { fill: "var(--graph-derived)", fontSize: 11, fontWeight: 650 },
      labelBgStyle: { fill: "var(--paper)", fillOpacity: 0.95 }, labelBgPadding: [7, 4],
      style: { stroke: "var(--graph-derived)", strokeWidth: 2, strokeDasharray: "5 4" },
    });
  }
  for (const relation of visibleRelations) {
    const color = "var(--graph-inferred)";
    edges.push({
      id: relation.id, source: relation.fromThreadId, target: relation.toThreadId,
      type: "smoothstep", label: `推断 · ${inferredText[relation.kind]} · ${relation.confidence.toFixed(2)}`,
      labelStyle: { fill: color, fontSize: 11, fontWeight: 650 },
      labelBgStyle: { fill: "var(--paper)", fillOpacity: 0.95 }, labelBgPadding: [7, 4],
      style: { stroke: color, strokeWidth: 2 },
      ...(["RELATED", "ALTERNATIVE_TO"].includes(relation.kind) ? {} : { markerEnd: { type: MarkerType.ArrowClosed, color } }),
    });
  }
  return edges;
}

async function layoutGraph(graph: ProjectGraph, signal?: AbortSignal): Promise<{ nodes: Node<GraphNodeData>[]; edges: Edge[]; warning: string }> {
  const visibleRelations = graph.inferredRelations ?? [];
  let coordinates = new Map<string, { x?: number; y?: number }>();
  let warning = "";
  try {
    const nodes = graph.nodes.map((node) => node.id);
    const allEdges = [...graph.relations, ...(graph.derivedRelations ?? []), ...visibleRelations].map((relation) => ({
      id: relation.id, source: relation.fromThreadId, target: relation.toThreadId,
    }));
    // Dense cyclic graphs can make layered layout take tens of seconds. Use a
    // source-prioritized spanning forest for positions and paint every edge.
    const parent = new Map(nodes.map((id) => [id, id]));
    const root = (id: string): string => {
      let current = id;
      while (parent.get(current) !== current) current = parent.get(current)!;
      return current;
    };
    const edges = allEdges.filter((edge) => {
      if (!parent.has(edge.source) || !parent.has(edge.target)) return false;
      const left = root(edge.source);
      const right = root(edge.target);
      if (left === right) return false;
      parent.set(right, left);
      return true;
    });
    const layoutOnMain = async () => {
      if (signal?.aborted) throw new Error("布局已取消");
      const { default: ELK } = await import("elkjs/lib/elk.bundled.js");
      if (signal?.aborted) throw new Error("布局已取消");
      const result = await new ELK().layout({
        id: "project",
        layoutOptions: { "elk.algorithm": "layered", "elk.direction": "RIGHT",
          "elk.spacing.nodeNode": "58", "elk.layered.spacing.nodeNodeBetweenLayers": "105" },
        children: nodes.map((id) => ({ id, width: nodeWidth, height: nodeHeight })),
        edges: edges.map(({ id, source, target }) => ({ id, sources: [source], targets: [target] })),
      });
      return result.children?.map((node) => ({ id: node.id, x: node.x, y: node.y })) ?? [];
    };
    if (typeof Worker === "undefined") {
      coordinates = new Map((await layoutOnMain()).map((node) => [node.id, node]));
    } else {
      const positions = await new Promise<{ id: string; x?: number; y?: number }[]>((resolve, reject) => {
        const worker = new Worker(new URL("./graphLayout.worker.ts", import.meta.url), { type: "module" });
        const abort = () => { worker.terminate(); reject(new Error("布局已取消")); };
        if (signal?.aborted) { abort(); return; }
        signal?.addEventListener("abort", abort, { once: true });
        worker.onmessage = (event: MessageEvent<{ positions?: { id: string; x?: number; y?: number }[]; error?: string }>) => {
          signal?.removeEventListener("abort", abort);
          worker.terminate();
          if (event.data.error) reject(new Error(event.data.error));
          else resolve(event.data.positions ?? []);
        };
        worker.onerror = (event) => { signal?.removeEventListener("abort", abort); worker.terminate(); reject(new Error(event.message || "布局线程失败")); };
        worker.postMessage({ nodes, edges });
      }).catch((cause) => {
        if (signal?.aborted) throw cause;
        // Some WebKit builds cannot construct ELK inside a module Worker.
        // The spanning forest keeps the fallback layout short enough to run
        // after yielding the UI for a frame.
        return new Promise<{ id: string; x?: number; y?: number }[]>((resolve, reject) => {
          if (signal?.aborted) { reject(new Error("布局已取消")); return; }
          const abort = () => { cancelAnimationFrame(frame); reject(new Error("布局已取消")); };
          signal?.addEventListener("abort", abort, { once: true });
          const frame = requestAnimationFrame(() => {
            signal?.removeEventListener("abort", abort);
            if (signal?.aborted) { reject(new Error("布局已取消")); return; }
            void layoutOnMain().then(resolve, reject);
          });
        });
      });
      coordinates = new Map(positions.map((node) => [node.id, node]));
    }
  } catch (cause) {
    warning = `自动布局未完成，已按固定顺序展示全部会话和关系。原因：${cause instanceof Error ? cause.message : String(cause)}`;
  }
  const nodes: Node<GraphNodeData>[] = graph.nodes.map((node, index) => ({
    id: node.id,
    type: "thread",
    position: { x: coordinates.get(node.id)?.x ?? (index % 4) * 320, y: coordinates.get(node.id)?.y ?? Math.floor(index / 4) * 150 },
    data: { title: node.referenceOnly ? node.title || node.id : threadDisplayTitle({ title: node.title, sourceKind: node.sourceKind ?? "" }), id: node.id, referenceOnly: node.referenceOnly },
    draggable: false,
  }));
  return { nodes, edges: graphEdges(graph), warning };
}

export function ProjectGraphView({ projectId, refreshVersion, onSelectEvidence, selectedThreadId = null, onSelectThread, visibleThreadIds, relationSource = "all", onRelationSourceChange, relationKind = "all", onRelationKindChange, minimumConfidence = 0.7, onMinimumConfidenceChange, renderSessionInspector, onOpenFullHistory, onGraphChanged }: { projectId: string; refreshVersion: number;
  onSelectEvidence?: (evidence: EvidenceReference) => void; selectedThreadId?: string | null; onSelectThread?: (id: string) => void;
  visibleThreadIds?: Set<string>; relationSource?: string; onRelationSourceChange?: (value: string) => void;
  relationKind?: string; onRelationKindChange?: (value: string) => void; minimumConfidence?: number; onMinimumConfidenceChange?: (value: number) => void;
  renderSessionInspector?: (handlers: { onSelectEvidence: (evidence: EvidenceReference) => void; onSelectThread: (id: string) => void }) => ReactNode;
  onOpenFullHistory?: () => void; onGraphChanged?: () => void }) {
  const queryCache = useProjectQueryCache();
  const loadedProjectId = useRef(projectId);
  const [graph, setGraph] = useState<ProjectGraph | null>(null);
  const [layout, setLayout] = useState<{ nodes: Node<GraphNodeData>[]; edges: Edge[]; warning: string }>({ nodes: [], edges: [], warning: "" });
  const [layoutPending, setLayoutPending] = useState(false);
  const [selection, setSelection] = useState<Selection>(null);
  const [error, setError] = useState("");
  const [decisionError, setDecisionError] = useState("");
  const [savingDecision, setSavingDecision] = useState(false);
  const [inspectorPage, setInspectorPage] = useState<"relation" | "session">(selectedThreadId ? "session" : "relation");
  const pendingEvidenceThread = useRef<string | null>(null);
  const [localMinimumConfidence, setLocalMinimumConfidence] = useState(minimumConfidence);
  const threshold = onMinimumConfidenceChange ? minimumConfidence : localMinimumConfidence;
  const changeThreshold = (value: number) => onMinimumConfidenceChange ? onMinimumConfidenceChange(value) : setLocalMinimumConfidence(value);
  useEffect(() => {
    if (pendingEvidenceThread.current !== null && pendingEvidenceThread.current === selectedThreadId) {
      pendingEvidenceThread.current = null;
      setInspectorPage("session");
      return;
    }
    setSelection(selectedThreadId ? { type: "node", id: selectedThreadId } : null);
    setInspectorPage(selectedThreadId ? "session" : "relation");
  }, [selectedThreadId]);
  useEffect(() => {
    let active = true;
    if (loadedProjectId.current !== projectId) {
      loadedProjectId.current = projectId;
      setGraph(null);
    }
    setError("");
    loadProjectQuery(queryCache, `project-graph:${projectId}`, refreshVersion,
      () => invoke<ProjectGraph>("get_project_graph", { projectId }))
      .then((next) => {
        if (!active) return;
        setLayoutPending(true);
        setGraph(next);
        setSelection((current) => current &&
          (current.type === "edge" ? [...next.relations, ...(next.derivedRelations ?? []), ...reviewedRelations(next)].some((relation) => relation.id === current.id) : next.nodes.some((node) => node.id === current.id))
          ? current : null);
      })
      .catch((cause) => { if (active) setError(formatAppError(cause, "关系图读取失败。请重试刷新。")); });
    return () => { active = false; };
  }, [projectId, refreshVersion, queryCache]);

  const visibleGraph = useMemo(() => graph
    ? filteredGraph(graph, visibleThreadIds, relationSource, relationKind, threshold)
    : null, [graph, visibleThreadIds, relationSource, relationKind, threshold]);

  useEffect(() => {
    if (!visibleGraph) return;
    const layoutSignature = JSON.stringify([
      visibleGraph.nodes.map((node) => [node.id, node.title, node.sourceKind, node.referenceOnly])
        .sort((left, right) => String(left[0]).localeCompare(String(right[0]))),
      visibleGraph.relations.map((relation) => [relation.id, relation.fromThreadId, relation.toThreadId, relation.kind, relation.source, relation.confidence])
        .sort((left, right) => String(left[0]).localeCompare(String(right[0]))),
      (visibleGraph.derivedRelations ?? []).map((relation) => [relation.id, relation.fromThreadId, relation.toThreadId, relation.kind])
        .sort((left, right) => String(left[0]).localeCompare(String(right[0]))),
      (visibleGraph.inferredRelations ?? []).map((relation) => [relation.id, relation.fromThreadId, relation.toThreadId, relation.kind, relation.confidence])
        .sort((left, right) => String(left[0]).localeCompare(String(right[0]))),
    ]);
    const layoutKey = `project-graph-layout:${projectId}`;
    const cachedLayout = queryCache?.get<GraphLayout>(layoutKey, layoutSignature);
    if (cachedLayout) {
      setLayout(cachedLayout);
      setLayoutPending(false);
      return;
    }
    let active = true;
    const controller = new AbortController();
    setLayoutPending(true);
    setLayout((current) => ({ ...current, edges: graphEdges(visibleGraph) }));
    layoutGraph(visibleGraph, controller.signal)
      .then((positioned) => {
        if (!active) return;
        if (!positioned.warning) queryCache?.set(layoutKey, layoutSignature, positioned);
        setLayout(positioned);
        setLayoutPending(false);
      });
    return () => { active = false; controller.abort(); };
  }, [visibleGraph, projectId, queryCache]);

  const visibleNodeIds = new Set(visibleGraph?.nodes.map((node) => node.id) ?? []);
  const visibleRelationIds = new Set([
    ...(visibleGraph?.relations ?? []), ...(visibleGraph?.derivedRelations ?? []), ...(visibleGraph?.inferredRelations ?? []),
  ].map((relation) => relation.id));
  const visibleNodes = layout.nodes.filter((node) => visibleNodeIds.has(node.id));
  const visibleEdges = layout.edges.filter((edge) => visibleRelationIds.has(edge.id));

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
  const selectedEvidenceIsMapped = detail?.type === "inferred"
    && (detail.relation.evidenceOptions ?? []).some((option) => option.pair.id === detail.relation.evidence.id);

  function selectSource(evidence: EvidenceReference) {
    selectSession(evidence.threadId);
    onSelectEvidence?.(evidence);
  }

  function selectSession(threadId: string) {
    if (threadId !== selectedThreadId) pendingEvidenceThread.current = threadId;
    setInspectorPage("session");
    onSelectThread?.(threadId);
  }

  const sessionInspectorContent = renderSessionInspector?.({ onSelectEvidence: selectSource, onSelectThread: selectSession });

  async function decide(relation: ReviewedRelation, decision: RelationReview["decision"]) {
    setSavingDecision(true);
    setDecisionError("");
    try {
      await invoke<RelationReview>("decide_inferred_relation", {
        projectId, relationId: relation.id, decision, expectedRevision: relation.review.revision,
        expectedEvidenceVersion: relation.evidenceVersion,
      });
      if (onGraphChanged) onGraphChanged();
      else {
        const next = await loadProjectQuery(queryCache, `project-graph:${projectId}`, refreshVersion,
          () => invoke<ProjectGraph>("get_project_graph", { projectId }), true);
        setGraph(next);
      }
    } catch (cause) {
      const failure = cause as { code?: string; message?: string };
      setDecisionError(failure?.code === "CONCURRENT_MODIFICATION"
        ? "关系裁决已被更新，请刷新关系图后重试。"
        : formatAppError(cause, "保存关系裁决失败，请刷新后重试。"));
    } finally {
      setSavingDecision(false);
    }
  }

  async function refreshGraph() {
    setDecisionError("");
    setLayoutPending(true);
    try {
      const next = await loadProjectQuery(queryCache, `project-graph:${projectId}`, refreshVersion,
        () => invoke<ProjectGraph>("get_project_graph", { projectId }), true);
      setGraph(next);
    } catch (cause) {
      setLayoutPending(false);
      setDecisionError(formatAppError(cause, "刷新关系图失败。"));
    }
  }

  return <section id="relations" className="panel graph-panel">
    <div className="panel-kicker">审查 / 项目关系</div>
    <h2>{graph?.project.name ?? "项目关系"}</h2>
    <p className="panel-intro">沿关系查看项目中的会话脉络。选择一条关系，可依次核对模型判断、双方证据和会话来源。</p>
    {error && <div className="page-error" role="alert">{error}</div>}
    {!graph && !error && <p className="empty-list">正在读取项目关系图…</p>}
    {graph && <>
      <div className="graph-summary"><strong>{visibleNodes.filter((node) => !node.data.referenceOnly).length} 条可见会话</strong><span>{visibleEdges.length} 条可见关系</span><span>{graph.relations.length} 条观察关系</span><span>{graph.derivedRelations?.length ?? 0} 条规则关系</span><span>{visibleInferred(graph, 0.7).length} 条默认显示的推断关系</span></div>
      <div className="explorer-filters" aria-label="关系过滤"><label>来源类别<select aria-label="按来源类别过滤关系" value={relationSource} onChange={(event) => onRelationSourceChange?.(event.target.value)}><option value="all">全部</option><option value="observed">观察</option><option value="derived">规则</option><option value="inferred">推断</option></select></label><label>关系类型<select aria-label="按关系类型过滤" value={relationKind} onChange={(event) => onRelationKindChange?.(event.target.value)}><option value="all">全部类型</option>{[...new Set([...graph.relations, ...(graph.derivedRelations ?? []), ...(graph.inferredRelations ?? [])].map((item) => item.kind))].sort().map((kind) => <option value={kind} key={kind}>{kind}</option>)}</select></label><label>最低置信度<select aria-label="最低关系置信度" value={threshold} onChange={(event) => changeThreshold(Number(event.target.value))}><option value={0}>全部</option><option value={0.5}>0.50</option><option value={0.7}>0.70（默认）</option><option value={0.9}>0.90</option></select></label></div>
      <label className="graph-confidence-toggle"><input type="checkbox" checked={threshold < 0.7} onChange={(event) => changeThreshold(event.target.checked ? 0 : 0.7)} />查看低于 0.70 的推断关系（{(graph.inferredRelations ?? []).filter((item) => item.confidence < 0.70).length}）</label>
      {reviewedRelations(graph).filter((relation) => relation.review.decision === "rejected" || !relation.evidenceValid).length > 0 &&
        <div className="graph-review-list"><strong>已拒绝或过期的推断关系</strong>
          {reviewedRelations(graph).filter((relation) => relation.review.decision === "rejected" || !relation.evidenceValid)
            .map((relation) => <button className="browse-button" key={relation.id} onClick={() => { setSelection({ type: "edge", id: relation.id }); setInspectorPage("relation"); }}>
              {inferredText[relation.kind]} · {reviewStatus(relation)} · {relation.staleReason ?? "可查看旧证据"} · {relation.fromThreadId} ↔ {relation.toThreadId}
            </button>)}
        </div>}
      {(graph.inferenceOutcomes ?? []).length > 0 && <p className="analysis-note">候选判断：无关系 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "none").length}，无法判断 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "undetermined").length}，证据不足 {(graph.inferenceOutcomes ?? []).filter((item) => item.status === "insufficientEvidence").length}；这些结果不生成图边。</p>}
      <button className="browse-button" onClick={() => void refreshGraph()}>自动布局</button>
      <p className="graph-layout-status" role="status">{layoutPending ? "正在布局关系图…" : "关系图布局完成"}</p>
      {layout.warning && <div className="graph-layout-warning" role="status">{layout.warning}</div>}
      {visibleNodes.length > 80 && <p className="analysis-note">大图先显示当前视口；平移或缩放可浏览其余会话和关系。图中保留全部 {visibleNodes.length} 个节点、{visibleEdges.length} 条关系。</p>}
      <div className="graph-inspection-layout">
      {visibleNodes.length ? <div className="graph-canvas" aria-label="项目结构关系图">
        <ReactFlow nodes={visibleNodes.map((node) => ({ ...node, selected: node.id === selectedThreadId }))} edges={visibleEdges} nodeTypes={nodeTypes} fitView={visibleNodes.length <= 80} onlyRenderVisibleElements fitViewOptions={{ padding: 0.18 }}
          nodesDraggable={false} onNodeClick={(_, node) => {
            setSelection({ type: "node", id: node.id });
            setInspectorPage(node.data.referenceOnly ? "relation" : "session");
            if (!node.data.referenceOnly) onSelectThread?.(node.id);
          }}
          onEdgeClick={(_, edge) => { setSelection({ type: "edge", id: edge.id }); setInspectorPage("relation"); }}
          onPaneClick={() => { setSelection(null); setInspectorPage("relation"); }} minZoom={0.15} maxZoom={2}>
          <Background gap={22} size={1} />
          <Controls showInteractive={false} />
        </ReactFlow>
      </div> : <p className="empty-list">{graph.nodes.length ? "当前条件没有可见会话；已选会话详情仍会保留。" : "当前项目没有会话。"}</p>}
      <aside className="graph-details" aria-live="polite" aria-label="关系与会话检查">
        {inspectorPage === "session" && sessionInspectorContent ? <>
          <button className="inspector-back" onClick={() => setInspectorPage("relation")}>← 返回关系检查</button>
          <div className="graph-session-inspector">{sessionInspectorContent}</div>
          {onOpenFullHistory && <button className="history-open-button" onClick={onOpenFullHistory}>打开完整会话历史 <span>↗</span></button>}
        </> : <>
        {!detail && <div className="inspector-empty"><span>选择一个节点或关系</span><p>关系判断、证据和会话详情会在这里连续展开。</p></div>}
        {detail?.type === "node" && <><h3>{detail.node.referenceOnly ? "仅有引用的端点" : "会话"}</h3><strong>{detail.node.referenceOnly ? detail.node.title ?? detail.node.id : threadDisplayTitle({ title: detail.node.title, sourceKind: detail.node.sourceKind ?? "" })}</strong><code>{detail.node.id}</code>{detail.node.referenceOnly && <p>来源记录了此会话 ID，当前项目图没有可展示的会话内容。</p>}</>}
        {detail?.type === "derived" && <><h3>规则关系 · {derivedText[detail.relation.kind]}</h3>
          <p><code>{detail.relation.fromThreadId}</code> ↔ <code>{detail.relation.toThreadId}</code></p>
          <p>{detail.relation.basis}</p><p>这是来源事实计算结果，不是模型判断或概率。</p>
          {detail.relation.evidence.map((evidence) => <div key={evidence.id} className="graph-proof">
            <blockquote>{evidence.excerpt}</blockquote><small>会话 {evidence.threadId} · 回合 {evidence.turnId} · 条目 {evidence.itemId} · 内容版本 {evidence.contentVersion.slice(0, 12)}</small>
            <button className="browse-button" onClick={() => selectSource(evidence)}>在会话中查看</button>
          </div>)}
        </>}
        {detail?.type === "inferred" && <><div className="inspector-kicker">模型推断 · {detail.relation.actualModel}</div><h3>{inferredText[detail.relation.kind]}</h3>
          <div className="inspector-status-row"><span className={detail.relation.evidenceValid ? "state-pill valid" : "state-pill stale"}>{detail.relation.evidenceValid ? "当前分析有效" : "历史结果 · 证据已变化"}</span><span className="state-pill neutral">{reviewStatus(detail.relation)}</span></div>
          {!detail.relation.evidenceValid && <><p>{detail.relation.staleReason ?? "来源证据已变化；下方保留当时的分析与证据快照。"}</p><button className="browse-button" onClick={() => document.getElementById("project-analysis")?.scrollIntoView({ behavior: "smooth" })}>前往重新分析</button></>}
          <p className="inspector-endpoints"><code>{detail.relation.fromThreadId}</code><span>{["RELATED", "ALTERNATIVE_TO"].includes(detail.relation.kind) ? "↔" : "→"}</span><code>{detail.relation.toThreadId}</code></p>
          <p className="inspector-explanation">{detail.relation.explanation}</p>
          <div className="judgment-metrics"><div><span>关系置信度</span><strong>{(detail.relation.confidence * 100).toFixed(1)}%</strong></div><div><span>证据选择置信度</span><strong>{(detail.relation.evidenceConfidence * 100).toFixed(1)}%</strong></div></div>
          <ProbabilityTable title="关系选项概率" probabilities={detail.relation.probabilities} onSelectEvidence={selectSource} />
          <ProbabilityTable title="证据选项概率" evidence probabilities={detail.relation.evidenceProbabilities}
            options={detail.relation.evidenceOptions} selectedOption={detail.relation.selectedEvidenceOption} onSelectEvidence={selectSource} />
          {!selectedEvidenceIsMapped && <div className="selected-evidence-snapshot">
            <div><strong>已选证据快照</strong><span>{detail.relation.selectedEvidenceOption ? `选项 ${detail.relation.selectedEvidenceOption}` : "历史选项映射缺失"}</span></div>
            {[detail.relation.evidence.left, detail.relation.evidence.right].map((item, index) => <article key={item.id}>
              <strong>{index === 0 ? "左侧证据" : "右侧证据"}</strong><blockquote>{item.excerpt}</blockquote>
              <small>会话 {item.threadId} · 回合 {item.turnId} · 条目 {item.itemId}</small>
              <button className="plain-button" onClick={() => selectSource(item)}>在会话中查看</button>
            </article>)}
          </div>}
          <div className="inspector-facts"><span>时间核对</span><strong>{["RELATED", "ALTERNATIVE_TO"].includes(detail.relation.kind) ? "无向关系不检查顺序" : detail.relation.timeCheck === "verified" ? "前后顺序已由证据回合验证" : "来源时间不足，无法验证顺序"}</strong><span>修订号</span><strong>{detail.relation.review.revision}</strong></div>
          {decisionError && <div className="page-error" role="alert">{decisionError}<button className="browse-button" onClick={refreshGraph}>刷新关系图</button></div>}
          <div className="graph-review-actions">
            <button className="primary-button" disabled={savingDecision || !detail.relation.evidenceValid || confirmedWithCurrentEvidence(detail.relation)} onClick={() => decide(detail.relation, "confirmed")}>确认关系</button>
            <button className="browse-button" disabled={savingDecision || detail.relation.review.decision === "rejected"} onClick={() => decide(detail.relation, "rejected")}>拒绝关系</button>
            <button className="browse-button" disabled={savingDecision || detail.relation.review.decision === "pending"} onClick={() => decide(detail.relation, "pending")}>恢复待裁决</button>
          </div>
          <small>模型输出分值表示判断的确定性，不等于关系正确率。</small>
        </>}
        {detail?.type === "edge" && <><h3>{sourceText[detail.relation.source]}关系 · {kindText[detail.relation.kind]}</h3>
          <p className="inspector-endpoints"><code>{detail.relation.fromThreadId}</code><span>→</span><code>{detail.relation.toThreadId}</code></p>
          <dl><dt>含义</dt><dd>{detail.relation.kind === "FORKED_FROM" ? "后续会话从来源父会话派生。" : "后续会话是来源父会话的子代理。"}</dd><dt>关系类型</dt><dd><code>{detail.relation.kind}</code></dd><dt>来源</dt><dd>{sourceText[detail.relation.source]} · 来源字段 <code>{detail.relation.sourceField}</code></dd><dt>置信度</dt><dd>{detail.relation.confidence.toFixed(1)}</dd><dt>端点</dt><dd>{endpointText[detail.relation.parentEndpoint]}</dd></dl>
        </>}
        </>}
      </aside>
      </div>
      {(graph.diagnostics.length > 0 || graph.relations.some((item) => item.parentEndpoint !== "inProject")) && <div className="graph-diagnostics"><strong>来源诊断</strong>
        {graph.relations.filter((item) => item.parentEndpoint !== "inProject").map((item) => <p key={item.id}><code>{item.toThreadId}</code> 的 <code>{item.sourceField}</code> 指向 <code>{item.fromThreadId}</code>：{endpointText[item.parentEndpoint]}</p>)}
        {graph.diagnostics.map((item) => <p key={`${item.threadId}:${item.sourceField}`}><code>{item.threadId}</code> 的 <code>{item.sourceField}</code>：{item.message}</p>)}
      </div>}
    </>}
  </section>;
}
