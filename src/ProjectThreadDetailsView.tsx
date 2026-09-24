import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";

type Evidence = { threadId: string; turnId: string; itemId: string; excerpt: string };
type Relation = { id: string; source: string; kind: string; fromThreadId: string; toThreadId: string;
  confidence?: number; basis?: string; explanation?: string; evidence?: Evidence[] | { left: Evidence; right: Evidence } };
type Graph = { nodes: { id: string; referenceOnly: boolean }[]; relations: Relation[]; derivedRelations: Relation[]; inferredRelations: Relation[];
  reviewedRelations?: { id: string; evidenceValid: boolean; evidenceVersion: string;
    review: { decision: string; confirmedEvidenceVersion: string | null } }[] };
type Thread = { id: string; title: string | null; preview: string; sessionId: string; cwd: string; sourceKind: string;
  sourceDetail: string | null; threadSource: string | null; parentThreadId: string | null; forkedFromId: string | null;
  createdAt: number; updatedAt: number; archived: boolean;
  metadataComplete: boolean; turnsComplete: boolean; itemsComplete: boolean; contentComplete: boolean;
  missingFromSource: boolean; readError: string | null;
  git: { branch: string | null; sha: string | null; originUrl: string | null } | null };
type Attribution = { workspaceRoot: string | null; detail: string; diagnostic: string | null; sourceProjectId: string | null };

export function ProjectThreadDetailsView({ projectId, thread, attribution, refreshVersion, hidden, onSelectThread, onSelectEvidence, relationSource = "all", relationKind = "all", minimumConfidence = 0.7 }: {
  projectId: string; thread: Thread; attribution: Attribution; refreshVersion: number; hidden: boolean;
  onSelectThread: (id: string) => void; onSelectEvidence: (evidence: Evidence) => void;
  relationSource?: string; relationKind?: string; minimumConfidence?: number;
}) {
  const loadedProjectId = useRef(projectId);
  const [graph, setGraph] = useState<Graph | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let active = true;
    if (loadedProjectId.current !== projectId) {
      loadedProjectId.current = projectId;
      setGraph(null);
    }
    setError("");
    invoke<Graph>("get_project_graph", { projectId }).then((value) => { if (active) { setGraph(value); setError(""); } })
      .catch((cause) => { if (active) setError(formatAppError(cause, "关系暂不可用；来源事实与回合仍可查看。")); });
    return () => { active = false; };
  }, [projectId, refreshVersion]);
  const relations = [...(graph?.relations ?? []), ...(graph?.derivedRelations ?? []), ...(graph?.inferredRelations ?? [])]
    .filter((relation) => (relation.fromThreadId === thread.id || relation.toThreadId === thread.id)
      && (relationSource === "all" || relationSource === (relation.source === "jev" ? "inferred" : relation.source))
      && (relationKind === "all" || relationKind === relation.kind)
      && (relation.source !== "jev" || (relation.confidence ?? 0) >= minimumConfidence || (minimumConfidence === 0.7 && graph?.reviewedRelations?.some((item) => item.id === relation.id && item.evidenceValid && item.review.decision === "confirmed" && item.review.confirmedEvidenceVersion === item.evidenceVersion))));
  const proof = (relation: Relation): Evidence[] => Array.isArray(relation.evidence) ? relation.evidence
    : relation.evidence ? [relation.evidence.left, relation.evidence.right] : [];
  const related = (relation: Relation) => relation.fromThreadId === thread.id ? relation.toThreadId : relation.fromThreadId;
  return <section id="thread-detail" className="panel thread-detail" aria-label="会话详情">
    <div className="panel-kicker">会话详情</div><h2>{thread.title || thread.preview || thread.id}</h2>
    {hidden && <p className="selection-hidden">此会话被当前过滤条件隐藏，详情仍可检查。</p>}
    <p>{thread.preview || "来源没有提供预览。"}</p>
    <dl><dt>会话标识</dt><dd><code>{thread.id}</code></dd><dt>Session 标识</dt><dd><code>{thread.sessionId}</code></dd>
      <dt>来源</dt><dd>{thread.sourceKind}{thread.sourceDetail ? ` / ${thread.sourceDetail}` : ""}{thread.threadSource ? ` · ${thread.threadSource}` : ""}</dd>
      <dt>来源项目标识</dt><dd><code>{attribution.sourceProjectId ?? "未提供"}</code></dd>
      <dt>父会话 / 派生自</dt><dd><code>{thread.parentThreadId ?? thread.forkedFromId ?? "无"}</code></dd>
      <dt>工作区</dt><dd><code>{attribution.workspaceRoot ?? thread.cwd}</code></dd><dt>工作目录</dt><dd><code>{thread.cwd}</code></dd>
      <dt>归属依据</dt><dd>{attribution.detail}{attribution.diagnostic && <span className="thread-warning"> · {attribution.diagnostic}</span>}</dd>
      <dt>Git</dt><dd>{thread.git?.branch ?? "无分支信息"}{thread.git?.sha && <code> · {thread.git.sha}</code>}{thread.git?.originUrl && <code> · {thread.git.originUrl}</code>}</dd>
      <dt>创建 / 更新</dt><dd>{new Date(thread.createdAt * 1000).toLocaleString("zh-CN")} / {new Date(thread.updatedAt * 1000).toLocaleString("zh-CN")}</dd>
      <dt>状态</dt><dd>{thread.archived ? "已归档" : "未归档"} · 元数据{thread.metadataComplete ? "完整" : "不完整"} · 回合{thread.turnsComplete ? "完整" : "不完整"} · 条目{thread.itemsComplete ? "完整" : "不完整"} · 内容{thread.contentComplete ? "完整" : "不完整"}{thread.missingFromSource && " · 最近完整列表未再次出现"}{thread.readError && ` · ${thread.readError}`}</dd></dl>
    <h3>相关关系 · {relations.length}</h3>
    {error && <p role="status">{error}</p>}
    {!graph && !error && <p>正在读取关系…</p>}
    {graph && relations.length === 0 && <p>暂无已保存关系；下方仍可检查回合、事实和总结。</p>}
    {relations.map((relation) => <div className="thread-detail-relation" key={relation.id}><strong>{relation.source === "observed" ? "观察" : relation.source === "derived" ? "规则" : "推断"} · {relation.kind}</strong>
      {graph?.nodes.some((node) => node.id === related(relation) && !node.referenceOnly)
        ? <button className="plain-button" onClick={() => onSelectThread(related(relation))}>{related(relation)}</button>
        : <code>{related(relation)}（仅有引用）</code>}
      {relation.confidence !== undefined && <span>置信度 {relation.confidence.toFixed(2)}</span>}
      {(relation.basis || relation.explanation) && <p>{relation.basis || relation.explanation}</p>}
      {proof(relation).map((evidence) => <button className="browse-button" key={`${evidence.threadId}:${evidence.turnId}:${evidence.itemId}`} onClick={() => onSelectEvidence(evidence)}>定位来源条目 · {evidence.threadId} / {evidence.turnId} / {evidence.itemId}</button>)}
    </div>)}
    <p className="analysis-note">下方可查看结构化事实、已生成总结及回合条目。未分析会话也可直接检查来源事实。</p>
  </section>;
}
