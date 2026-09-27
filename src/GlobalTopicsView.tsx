import React, { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";
import { threadDisplayTitle } from "./threadDisplay";
import { ProgressMeter } from "./ProgressMeter";
import { projectStateNames, semanticProgress } from "./analysisProgress";
import type { SemanticRun } from "./analysisProgress";

type TopicLabel = { id: string; name: string; description: string };
type TopicAssignment = {
  threadId: string; projectId: string; topicId: string | null; suggestedTopicId: string | null;
  confidence: number; manual: boolean; model: string;
};
type TopicThread = {
  thread: { id: string; title: string | null; preview: string; sourceKind: string; updatedAt: number };
  project: { id: string; name: string };
  assignment: TopicAssignment | null;
  semanticNeighbors: { threadId: string; projectId: string; score: number }[];
  summaryAvailable: boolean;
  indexed: boolean;
};
type TopicRelation = { kind: string; fromThreadId: string; toThreadId: string; confidence: number };
type RelationOutcome = { leftThreadId: string; rightThreadId: string; status: string; unknownCount: number; relations: TopicRelation[] };
type TopicView = { labels: TopicLabel[]; threads: TopicThread[]; indexedCount: number; pendingIndexCount: number; pendingTopicAssignments: number; crossProjectCandidateCount: number; pendingCrossProjectPairs: number; crossProjectRelations: RelationOutcome[]; topicAssignmentThreshold: number; manualTopicSampleCount: number; manualTopicAgreement: number | null };
type IndexResult = { indexed: number; reused: number; unavailableSummaries: number; topicAssignments: number; pendingTopicAssignments: number; jevCalls: number; actualModel: string | null; crossProjectCandidates: number; crossProjectRelations: number; pendingCrossProjectPairs: number };

export function GlobalTopicsView() {
  const [view, setView] = useState<TopicView | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [indexStarting, setIndexStarting] = useState(false);
  const [error, setError] = useState("");
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [indexResult, setIndexResult] = useState<IndexResult | null>(null);
  const [indexRun, setIndexRun] = useState<SemanticRun | null>(null);
  const [search, setSearch] = useState("");
  const [topicFilter, setTopicFilter] = useState("");

  async function refresh() {
    setError("");
    try { setView(await invoke<TopicView>("get_global_topic_view")); }
    catch (cause) { setError(formatAppError(cause, "读取全局主题失败。")); }
    finally { setLoading(false); }
  }

  useEffect(() => { void refresh(); }, []);

  useEffect(() => {
    let active = true;
    invoke<SemanticRun | null>("get_latest_semantic_index_run")
      .then((value) => { if (active) setIndexRun((current) => current && (!value || (current.id !== value.id && current.startedAtUnixMs >= value.startedAtUnixMs)) ? current : value); })
      .catch((cause) => { if (active) setError(formatAppError(cause, "读取全局主题运行失败。")); });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    if (!indexRun || !["queued", "running", "cancelling"].includes(indexRun.state)) return;
    let active = true;
    const timer = window.setInterval(() => {
      invoke<SemanticRun | null>("get_semantic_index_run", { runId: indexRun.id })
        .then((value) => { if (active && value) setIndexRun((current) => current && current.id !== value.id && current.startedAtUnixMs > value.startedAtUnixMs ? current : value); })
        .catch((cause) => { if (active) setError(formatAppError(cause, "读取全局主题进度失败。")); });
    }, 1000);
    return () => { active = false; window.clearInterval(timer); };
  }, [indexRun?.id, indexRun?.state]);

  useEffect(() => {
    if (!indexRun?.finishedAtUnixMs) return;
    setIndexResult(indexRun.result);
    void refresh();
  }, [indexRun?.id, indexRun?.finishedAtUnixMs]);

  async function createTopic(event: React.FormEvent) {
    event.preventDefault();
    if (!name.trim()) return;
    setBusy(true); setError("");
    try {
      setView(await invoke<TopicView>("create_topic_label", { name: name.trim(), description: description.trim() }));
      setName(""); setDescription("");
    } catch (cause) { setError(formatAppError(cause, "创建主题失败。")); }
    finally { setBusy(false); }
  }

  async function updateTopic(topic: TopicLabel) {
    const nextName = window.prompt("主题名称", topic.name);
    if (nextName === null) return;
    const nextDescription = window.prompt("主题说明", topic.description);
    if (nextDescription === null) return;
    setBusy(true); setError("");
    try { setView(await invoke<TopicView>("update_topic_label", { id: topic.id, name: nextName, description: nextDescription })); }
    catch (cause) { setError(formatAppError(cause, "更新主题失败。")); }
    finally { setBusy(false); }
  }

  async function deleteTopic(topic: TopicLabel) {
    if (!window.confirm(`删除主题“${topic.name}”？会话归属会改为未分类。`)) return;
    setBusy(true); setError("");
    try { setView(await invoke<TopicView>("delete_topic_label", { id: topic.id })); }
    catch (cause) { setError(formatAppError(cause, "删除主题失败。")); }
    finally { setBusy(false); }
  }

  async function setThreadTopic(threadId: string, topicId: string | null) {
    setBusy(true); setError("");
    try { setView(await invoke<TopicView>("set_thread_topic", { threadId, topicId })); }
    catch (cause) { setError(formatAppError(cause, "保存会话主题失败。")); }
    finally { setBusy(false); }
  }

  async function rebuildIndex() {
    setBusy(true); setIndexStarting(true); setError(""); setIndexResult(null); setIndexRun(null);
    try {
      setIndexRun(await invoke<SemanticRun>("start_semantic_index_run"));
    } catch (cause) { setError(formatAppError(cause, "全局索引或主题归属未能启动。")); }
    finally { setBusy(false); setIndexStarting(false); }
  }

  async function cancelIndex() {
    try { await invoke("cancel_semantic_index"); }
    catch (cause) { setError(formatAppError(cause, "取消索引失败。")); }
  }

  const indexActive = !!indexRun && ["queued", "running", "cancelling"].includes(indexRun.state);

  const filteredThreads = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    return (view?.threads ?? []).filter((item) => {
      const assignedTopic = item.assignment?.topicId ?? "";
      const matchesTopic = !topicFilter
        || (topicFilter === "__unclassified__" ? !assignedTopic : assignedTopic === topicFilter);
      const topicName = view?.labels.find((label) => label.id === assignedTopic)?.name ?? "";
      return matchesTopic && (!query || `${item.thread.title ?? ""} ${item.thread.preview} ${item.project.name} ${topicName}`.toLocaleLowerCase().includes(query));
    });
  }, [search, topicFilter, view]);
  const labelById = useMemo(() => new Map((view?.labels ?? []).map((label) => [label.id, label])), [view?.labels]);
  const byProject = useMemo(() => {
    const groups = new Map<string, { name: string; threads: TopicThread[] }>();
    for (const thread of filteredThreads) {
      const group = groups.get(thread.project.id) ?? { name: thread.project.name, threads: [] };
      group.threads.push(thread); groups.set(thread.project.id, group);
    }
    return [...groups.entries()].sort((a, b) => a[1].name.localeCompare(b[1].name, "zh-CN"));
  }, [filteredThreads]);

  return <>
    {error && <div className="page-error" role="alert">{error}</div>}
    <section className="panel topic-index-panel">
      <div className="topic-index-heading"><div><h2>主题分析</h2><p className="panel-intro">将总结与产物摘要发送至已配置的嵌入服务和 Jev。</p></div>
        <div className="topic-index-actions"><button className="primary-button" disabled={busy || loading || indexActive} onClick={() => void rebuildIndex()}>{indexStarting ? "正在启动…" : indexActive ? "分析运行中…" : "生成索引并分配主题"}<span>↗</span></button>{indexActive && <button className="plain-button" disabled={indexRun?.state === "cancelling"} onClick={() => void cancelIndex()}>{indexRun?.state === "cancelling" ? "取消中…" : "取消"}</button>}<button className="browse-button" disabled={busy} onClick={() => void refresh()}>刷新状态</button></div>
      </div>
      {indexStarting && <ProgressMeter progress={{ stage: "准备全局主题分析", completed: 0, total: null, unit: "项", calls: 0 }} label="全局主题启动进度" />}
      {indexRun && <div className="analysis-progress prominent" role="status"><strong>全局主题处理 · {projectStateNames[indexRun.state]}</strong>
        <ProgressMeter progress={semanticProgress(indexRun)} label="全局主题处理进度" totalMayChange />
        {indexRun.totalCalls > 0 && <small>已确认模型调用 {indexRun.totalCalls} 次</small>}
        {indexRun.error && <em>{formatAppError(indexRun.error, "全局主题处理失败。")}</em>}
      </div>}
      <div className="topic-index-stats"><span>已索引 {view?.indexedCount ?? 0}</span><span>待索引 {view?.pendingIndexCount ?? 0}</span><span>待主题归属 {view?.pendingTopicAssignments ?? 0}</span><span>跨项目语义近邻 {view?.crossProjectCandidateCount ?? 0} 对</span><span>待 Jev 判断 {view?.pendingCrossProjectPairs ?? 0} 对</span><span>主题标签 {view?.labels.length ?? 0} / 80</span><details className="technical-details"><summary>分类统计</summary><span>当前归属门槛 {((view?.topicAssignmentThreshold ?? 0.45) * 100).toFixed(0)}%</span>{(view?.manualTopicSampleCount ?? 0) > 0 && <span>人工校正样本 {view!.manualTopicSampleCount} · 建议一致率 {((view!.manualTopicAgreement ?? 0) * 100).toFixed(0)}%</span>}<p>一致率仅反映人工校正样本。</p></details></div>
      {indexResult && <div className="jev-result" role="status"><strong>索引完成</strong><span>新建向量 {indexResult.indexed} 条，复用 {indexResult.reused} 条，缺少有效总结 {indexResult.unavailableSummaries} 条，自动归属主题 {indexResult.topicAssignments} 条，待主题归属 {indexResult.pendingTopicAssignments} 条。本次共用 Jev 推理 {indexResult.jevCalls} 次；跨项目候选 {indexResult.crossProjectCandidates} 对，新增支持关系 {indexResult.crossProjectRelations} 条，待后续批次判断 {indexResult.pendingCrossProjectPairs} 对。实际嵌入模型：{indexResult.actualModel ?? "未返回"}。</span></div>}
    </section>
    <section className="panel topic-label-panel">
      <h2>主题标签</h2>
      <form className="topic-create-form" onSubmit={(event) => void createTopic(event)}><label>主题名称<input value={name} maxLength={80} onChange={(event) => setName(event.target.value)} placeholder="例如：桌面应用构建" /></label><label>主题说明<input value={description} maxLength={500} onChange={(event) => setDescription(event.target.value)} placeholder="描述主题范围（可选）" /></label><button className="browse-button" disabled={busy || indexActive || !name.trim()}>添加主题</button></form>
      {view?.labels.length ? <div className="topic-label-list">{view.labels.map((topic) => <article className="topic-label-card" key={topic.id}><div><strong>{topic.name}</strong><p>{topic.description || "尚无说明"}</p><small>{view.threads.filter((item) => item.assignment?.topicId === topic.id).length} 条会话</small></div><button className="browse-button" disabled={busy || indexActive} onClick={() => void updateTopic(topic)}>编辑</button><button className="plain-button" disabled={busy || indexActive} onClick={() => void deleteTopic(topic)}>删除</button></article>)}</div> : <p className="empty-list">尚无主题，请先添加标签。</p>}
    </section>
    <section className="panel topic-thread-panel">
      <div className="topic-thread-heading"><div><h2>会话</h2></div><div className="topic-thread-filters"><select aria-label="按主题筛选全局会话" value={topicFilter} onChange={(event) => setTopicFilter(event.target.value)}><option value="">全部主题</option><option value="__unclassified__">未分类</option>{view?.labels.map((label) => <option key={label.id} value={label.id}>{label.name}</option>)}</select><input aria-label="筛选全局主题会话" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="搜索标题、预览或项目" /></div></div>
      {loading ? <p className="empty-list">正在读取主题视图…</p> : byProject.length === 0 ? <p className="empty-list">没有匹配会话。</p> : byProject.map(([projectId, group]) => <section className="topic-project-group" key={projectId}><h3>{group.name}<span>{group.threads.length} 条</span></h3>{group.threads.map((item) => {
        const assignment = item.assignment;
        const currentLabel = assignment?.topicId ? labelById.get(assignment.topicId) : null;
        const suggestedLabel = assignment?.suggestedTopicId ? labelById.get(assignment.suggestedTopicId) : null;
        return <article className="topic-thread-row" key={item.thread.id}><div className="topic-thread-copy"><strong>{threadDisplayTitle(item.thread)}</strong><p>{item.thread.preview || "没有预览内容"}</p><div className="topic-thread-meta"><span>{item.indexed ? "语义索引可用" : item.summaryAvailable ? "待生成语义索引" : "缺少当前版本总结"}</span>{assignment && <span>{assignment.manual ? "人工归属" : `Jev ${(assignment.confidence * 100).toFixed(0)}%`}</span>}{!currentLabel && suggestedLabel && <span>最佳候选：{suggestedLabel.name}</span>}</div>
          {item.semanticNeighbors.length > 0 && <div className="topic-neighbors"><span>语义近邻：</span>{item.semanticNeighbors.slice(0, 3).map((neighbor) => { const found = view?.threads.find((candidate) => candidate.thread.id === neighbor.threadId); const outcome = view?.crossProjectRelations.find((pair) => (pair.leftThreadId === item.thread.id && pair.rightThreadId === neighbor.threadId) || (pair.rightThreadId === item.thread.id && pair.leftThreadId === neighbor.threadId)); const kinds = outcome?.relations.map((relation) => relation.kind).join("、"); return <span key={neighbor.threadId}>{found ? threadDisplayTitle(found.thread) : neighbor.threadId} · {found?.project.name ?? "项目"} · {(neighbor.score * 100).toFixed(0)}%{outcome ? ` · Jev ${kinds || (outcome.status === "undetermined" ? "无法判断" : "未支持关系")}` : " · 待判断"}</span>; })}</div>}
        </div><label className="topic-assignment">主要主题<select aria-label={`${threadDisplayTitle(item.thread)}的主要主题`} disabled={busy || indexActive} value={assignment?.topicId ?? ""} onChange={(event) => void setThreadTopic(item.thread.id, event.target.value || null)}><option value="">未分类</option>{view?.labels.map((topic) => <option key={topic.id} value={topic.id}>{topic.name}</option>)}</select></label></article>;
      })}</section>)}
    </section>
  </>;
}
