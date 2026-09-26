import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";
import { threadDisplayTitle, threadPreviewExcerpt } from "./threadDisplay";
import { loadProjectQuery, useProjectQueryCache } from "./projectQueryCache";

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

const relationKindNames: Record<string, string> = {
  FORKED_FROM: "派生自", SUBAGENT_OF: "子代理", SHARED_FILE: "共同文件", SHARED_ARTIFACT: "共同产物",
  EXPLICIT_REFERENCE: "明确引用", CONTINUES: "延续", IMPLEMENTS: "落实", FIXES: "修复",
  VALIDATES: "验证", INVESTIGATES: "调查", ALTERNATIVE_TO: "替代方案", SUPERSEDES: "取代",
  MOTIVATED_BY: "促成", RELATED: "相关",
};

export function ProjectThreadDetailsView({ projectId, thread, attribution, refreshVersion, hidden, onSelectThread, onSelectEvidence, relationSource = "all", relationKind = "all", minimumConfidence = 0.7 }: {
  projectId: string; thread: Thread; attribution: Attribution; refreshVersion: number; hidden: boolean;
  onSelectThread: (id: string) => void; onSelectEvidence: (evidence: Evidence) => void;
  relationSource?: string; relationKind?: string; minimumConfidence?: number;
}) {
  const queryCache = useProjectQueryCache();
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
    loadProjectQuery(queryCache, `project-graph:${projectId}`, refreshVersion,
      () => invoke<Graph>("get_project_graph", { projectId }))
      .then((value) => { if (active) { setGraph(value); setError(""); } })
      .catch((cause) => { if (active) setError(formatAppError(cause, "关系暂不可用；来源事实与回合仍可查看。")); });
    return () => { active = false; };
  }, [projectId, refreshVersion, queryCache]);
  const relations = [...(graph?.relations ?? []), ...(graph?.derivedRelations ?? []), ...(graph?.inferredRelations ?? [])]
    .filter((relation) => (relation.fromThreadId === thread.id || relation.toThreadId === thread.id)
      && (relationSource === "all" || relationSource === (relation.source === "jev" ? "inferred" : relation.source))
      && (relationKind === "all" || relationKind === relation.kind)
      && (relation.source !== "jev" || (relation.confidence ?? 0) >= minimumConfidence || (minimumConfidence === 0.7 && graph?.reviewedRelations?.some((item) => item.id === relation.id && item.evidenceValid && item.review.decision === "confirmed" && item.review.confirmedEvidenceVersion === item.evidenceVersion))));
  const proof = (relation: Relation): Evidence[] => Array.isArray(relation.evidence) ? relation.evidence
    : relation.evidence ? [relation.evidence.left, relation.evidence.right] : [];
  const related = (relation: Relation) => relation.fromThreadId === thread.id ? relation.toThreadId : relation.fromThreadId;
  return <section id="thread-detail" className="thread-detail" aria-label="会话详情">
    <div className="thread-detail-heading">
      <div className="panel-kicker">会话详情</div>
      <span className={`state-pill ${thread.archived ? "neutral" : "valid"}`}>{thread.archived ? "已归档" : "未归档"}</span>
      <h2>{threadDisplayTitle(thread)}</h2>
    </div>
    {hidden && <p className="selection-hidden">此会话被当前过滤条件隐藏，详情仍可检查。</p>}
    <div className="thread-preview-card"><span>会话预览</span><p>{thread.preview ? threadPreviewExcerpt(thread.preview) : "来源没有提供预览。"}</p></div>
    <div className="thread-summary-grid">
      <div><span>来源</span><strong>{thread.sourceKind}{thread.sourceDetail ? ` / ${thread.sourceDetail}` : ""}{thread.threadSource ? ` · ${thread.threadSource}` : ""}</strong></div>
      <div><span>工作区</span><code>{attribution.workspaceRoot ?? thread.cwd}</code></div>
      <div><span>最近更新</span><time>{new Date(thread.updatedAt * 1000).toLocaleString("zh-CN")}</time></div>
      <div><span>状态</span><strong>{thread.missingFromSource ? "完整列表中未再次出现" : thread.readError ? "来源读取异常" : "来源记录可用"}</strong></div>
    </div>
    {thread.readError && <p className="thread-detail-warning">{thread.readError}</p>}
    <details className="thread-technical">
      <summary><span>技术信息</span><small>标识、路径、Git 与采集诊断</small></summary>
      <dl>
        <dt>会话标识</dt><dd><code>{thread.id}</code></dd>
        <dt>Session 标识</dt><dd><code>{thread.sessionId}</code></dd>
        <dt>来源项目标识</dt><dd><code>{attribution.sourceProjectId ?? "未提供"}</code></dd>
        <dt>父会话 / 派生自</dt><dd><code>{thread.parentThreadId ?? thread.forkedFromId ?? "无"}</code></dd>
        <dt>工作目录</dt><dd><code>{thread.cwd}</code></dd>
        <dt>归属依据</dt><dd>{attribution.detail}{attribution.diagnostic && <span className="thread-warning"> · {attribution.diagnostic}</span>}</dd>
        <dt>Git</dt><dd>{thread.git?.branch ?? "无分支信息"}{thread.git?.sha && <code> · {thread.git.sha}</code>}{thread.git?.originUrl && <code> · {thread.git.originUrl}</code>}</dd>
        <dt>创建时间</dt><dd>{new Date(thread.createdAt * 1000).toLocaleString("zh-CN")}</dd>
        <dt>采集完整性</dt><dd>元数据 {thread.metadataComplete ? "完整" : "不完整"} · 回合 {thread.turnsComplete ? "完整" : "不完整"} · 条目 {thread.itemsComplete ? "完整" : "不完整"} · 内容 {thread.contentComplete ? "完整" : "不完整"}</dd>
      </dl>
    </details>
    <div className="thread-related">
      <div className="thread-related-heading"><h3>相关关系</h3><span>{relations.length}</span></div>
      {error && <p role="status" className="thread-detail-note">{error}</p>}
      {!graph && !error && <p className="thread-detail-note">正在读取关系…</p>}
      {graph && relations.length === 0 && <p className="thread-detail-note">暂无已保存关系；可在“探索”中检查回合、事实和总结。</p>}
      {relations.map((relation) => <div className="thread-detail-relation" key={relation.id}>
        <div className="thread-relation-label"><span className="state-pill neutral">{relation.source === "observed" ? "观察" : relation.source === "derived" ? "规则" : "推断"}</span><strong>{relationKindNames[relation.kind] ?? relation.kind}</strong></div>
        {graph?.nodes.some((node) => node.id === related(relation) && !node.referenceOnly)
          ? <button className="plain-button" onClick={() => onSelectThread(related(relation))}>查看相关会话 · {related(relation)}</button>
          : <code>{related(relation)}（仅有引用）</code>}
        {relation.confidence !== undefined && <span className="relation-confidence">置信度 {(relation.confidence * 100).toFixed(0)}%</span>}
        {(relation.basis || relation.explanation) && <p>{relation.basis || relation.explanation}</p>}
        {proof(relation).map((evidence) => <button className="browse-button" key={`${evidence.threadId}:${evidence.turnId}:${evidence.itemId}`} onClick={() => onSelectEvidence(evidence)}>在会话中查看 · {evidence.turnId}</button>)}
      </div>)}
    </div>
    <p className="analysis-note">完整历史、总结与来源事实可在“探索”中按需查看。</p>
  </section>;
}
