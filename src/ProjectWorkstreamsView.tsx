import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";

type Stream = { id: string; name: string; members: string[]; relationIds: string[];
  nameInputVersion: string | null; nameError: string | null };
type View = { workstreams: Stream[]; ungroupedThreadIds: string[]; crossRelationIds: string[];
  revision: number; manuallyNamedWorkstreamIds: string[]; manuallyAssignedThreadIds: string[] };
type Node = { id: string; title: string | null; referenceOnly: boolean };
type Evidence = { threadId: string; turnId: string; itemId: string };
type Relation = { id: string; fromThreadId: string; toThreadId: string; kind: string;
  source: string; basis?: string; evidence?: Evidence[] | { left: Evidence; right: Evidence } };
type Graph = { nodes: Node[]; relations: Relation[]; derivedRelations: Relation[]; inferredRelations: Relation[] };
const PAGE_SIZE = 40;

export function ProjectWorkstreamsView({ projectId, refreshVersion, onSelectThread, onSelectEvidence, activeWorkstreamId = "", onFilterWorkstream, onChanged }: {
  projectId: string; refreshVersion: number; onSelectThread: (id: string) => void;
  onSelectEvidence: (evidence: { threadId: string; turnId: string; itemId: string }) => void;
  activeWorkstreamId?: string; onFilterWorkstream?: (id: string) => void; onChanged?: () => void;
}) {
  const loadedProjectId = useRef(projectId);
  const [view, setView] = useState<View | null>(null);
  const [graph, setGraph] = useState<Graph | null>(null);
  const [error, setError] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [nameDraft, setNameDraft] = useState("");
  const [editingName, setEditingName] = useState(false);
  const [targetDrafts, setTargetDrafts] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState("");
  const [saved, setSaved] = useState("");
  const [memberLimit, setMemberLimit] = useState(PAGE_SIZE);
  const [relationLimit, setRelationLimit] = useState(PAGE_SIZE);
  const [ungroupedLimit, setUngroupedLimit] = useState(PAGE_SIZE);
  const [crossLimit, setCrossLimit] = useState(PAGE_SIZE);
  useEffect(() => {
    let active = true;
    if (loadedProjectId.current !== projectId) {
      loadedProjectId.current = projectId;
      setView(null); setGraph(null); setSelectedId(null);
    }
    Promise.all([
      invoke<View>("get_project_workstreams", { projectId }),
      invoke<Graph>("get_project_graph", { projectId }),
    ]).then(([nextView, nextGraph]) => {
      if (active) { setView(nextView); setGraph(nextGraph); setError(""); }
    }).catch((caught) => { if (active) setError(formatAppError(caught, "无法读取工作流。")); });
    return () => { active = false; };
  }, [projectId, refreshVersion]);
  const selected = view?.workstreams.find((item) => item.id === selectedId) ?? view?.workstreams[0];
  useEffect(() => { setMemberLimit(PAGE_SIZE); setRelationLimit(PAGE_SIZE); }, [projectId, selected?.id]);
  useEffect(() => { setUngroupedLimit(PAGE_SIZE); setCrossLimit(PAGE_SIZE); }, [projectId]);
  useEffect(() => {
    if (!editingName) setNameDraft(selected?.name ?? "");
  }, [selected?.id, selected?.name, editingName]);
  const failureMessage = (caught: unknown) => {
    const failure = caught as { code?: string; message?: string };
    return failure?.code === "CONCURRENT_MODIFICATION"
      ? "工作流已被更新。请刷新工作流后重试；编辑内容已保留。"
      : formatAppError(caught, "保存工作流失败，请重试。");
  };
  async function refresh() {
    try {
      const [nextView, nextGraph] = await Promise.all([
        invoke<View>("get_project_workstreams", { projectId }),
        invoke<Graph>("get_project_graph", { projectId }),
      ]);
      setView(nextView); setGraph(nextGraph); setError(""); setSaved("");
    } catch (caught) { setError(failureMessage(caught)); }
  }
  async function change(command: string, args: Record<string, unknown>, key: string, done: () => void) {
    if (!view || saving) return;
    setSaving(key); setError(""); setSaved("");
    try {
      await invoke(command, { projectId, expectedRevision: view.revision, ...args });
      const next = await invoke<View>("get_project_workstreams", { projectId });
      setView(next); done(); setSaved("已保存。"); onChanged?.();
    } catch (caught) { setError(failureMessage(caught)); }
    finally { setSaving(""); }
  }
  const title = (id: string) => graph?.nodes.find((node) => node.id === id)?.title || id;
  const relations = [...(graph?.relations ?? []), ...(graph?.derivedRelations ?? []), ...(graph?.inferredRelations ?? [])];
  const relation = (id: string) => relations.find((item) => item.id === id);
  const evidence = (item: Relation): Evidence | undefined => Array.isArray(item.evidence)
    ? item.evidence[0] : item.evidence?.left;
  const sourceName = (item: Relation) => item.source === "observed" ? "观察关系" : item.source === "derived" ? "规则关系" : "推断关系";
  const ownerOf = (id: string) => view?.workstreams.find((stream) => stream.members.includes(id))?.id ?? "";
  const threadRow = (id: string) => {
    const current = ownerOf(id);
    const draft = targetDrafts[id] ?? current;
    return <div className="workstream-thread" key={id}>
      <button onClick={() => onSelectThread(id)}>{title(id)}<small>{id}</small></button>
      <label>主要工作流
        <select aria-label={`${title(id)}的主要工作流`} value={draft} disabled={!!saving}
          onChange={(event) => setTargetDrafts((before) => ({ ...before, [id]: event.target.value }))}>
          <option value="">未分组</option>
          {view?.workstreams.map((stream) => <option key={stream.id} value={stream.id}>{stream.name}</option>)}
        </select>
      </label>
      <button className="browse-button" disabled={!!saving || draft === current}
        onClick={() => change("move_thread_to_workstream", { threadId: id, targetId: draft || null }, id,
          () => setTargetDrafts((before) => { const next = { ...before }; delete next[id]; return next; }))}>保存归属</button>
      {view?.manuallyAssignedThreadIds?.includes(id) && <button className="plain-button" disabled={!!saving}
        onClick={() => change("restore_thread_workstream", { threadId: id }, id,
          () => setTargetDrafts((before) => { const next = { ...before }; delete next[id]; return next; }))}>恢复自动归属</button>}
    </div>;
  };
  return <section id="workstreams" className="panel workstreams-panel" aria-label="项目工作流">
    <div className="panel-kicker">04 / 工作流</div><h2>工作流导航</h2>
    <p className="panel-intro">分组由有效关系生成，自动名称来自 Codex 临时分析。可改名或调整会话的主要工作流；跨组联系与未分组会话保留在这里。</p>
    {error && <p className="page-error" role="alert">{error} <button className="browse-button" onClick={refresh}>刷新工作流</button></p>}
    {saved && <p role="status">{saved}</p>}
    {!view && !error && <p>正在读取工作流…</p>}
    {view && <>
      <div className="workstream-navigation">
        {view.workstreams.map((item) => <button key={item.id} className={selected?.id === item.id ? "selected" : ""}
          onClick={() => { setSelectedId(item.id); onFilterWorkstream?.(activeWorkstreamId === item.id ? "" : item.id); setEditingName(false); setSaved(""); }}><strong>{item.name}</strong><small>{item.members.length} 条会话{view.manuallyNamedWorkstreamIds?.includes(item.id) ? " · 人工命名" : item.nameInputVersion ? " · 自动命名" : " · 暂用名"}</small></button>)}
        {view.workstreams.length === 0 && <p>当前没有可形成工作流的关系。</p>}
      </div>
      {selected && <div className="workstream-detail"><h3>{selected.name}</h3>
        <div className="workstream-editor"><label htmlFor="workstream-name">工作流名称</label>
          <input id="workstream-name" value={nameDraft} maxLength={80} disabled={!!saving}
            onChange={(event) => { setNameDraft(event.target.value); setEditingName(true); }} />
          <button className="browse-button" disabled={!!saving || !editingName || nameDraft.trim().length < 2}
            onClick={() => change("rename_workstream", { workstreamId: selected.id, name: nameDraft }, selected.id,
              () => setEditingName(false))}>保存名称</button>
          {view.manuallyNamedWorkstreamIds?.includes(selected.id) && <button className="plain-button" disabled={!!saving}
            onClick={() => change("restore_workstream_name", { workstreamId: selected.id }, selected.id,
              () => setEditingName(false))}>恢复自动名称</button>}
        </div>
        {selected.nameError && <p className="page-error" role="status">命名未完成：{selected.nameError}。分组与来源关系已保留。</p>}
        <div className="workstream-members"><strong>成员 · {selected.members.length}</strong>{selected.members.slice(0, memberLimit).map(threadRow)}
          {memberLimit < selected.members.length && <button className="browse-button" onClick={() => setMemberLimit((limit) => limit + PAGE_SIZE)}>显示更多成员（{Math.min(memberLimit, selected.members.length)} / {selected.members.length}）</button>}</div>
        <div className="workstream-relations"><strong>内部来源关系 · {selected.relationIds.length}</strong>{selected.relationIds.slice(0, relationLimit).map((id) => {
          const item = relation(id);
          return item && <div key={id}><span>{sourceName(item)} · {item.kind} · {title(item.fromThreadId)} → {title(item.toThreadId)}</span>
            {item.basis && <small>{item.basis}</small>}
            {evidence(item) && <button className="plain-button" onClick={() => onSelectEvidence(evidence(item)!)}>查看来源</button>}
          </div>;
        })}{relationLimit < selected.relationIds.length && <button className="browse-button" onClick={() => setRelationLimit((limit) => limit + PAGE_SIZE)}>显示更多内部关系（{Math.min(relationLimit, selected.relationIds.length)} / {selected.relationIds.length}）</button>}</div>
      </div>}
      <div className="workstream-ungrouped"><h3>未分组会话 · {view.ungroupedThreadIds.length}</h3>
        {view.ungroupedThreadIds.slice(0, ungroupedLimit).map(threadRow)}
        {ungroupedLimit < view.ungroupedThreadIds.length && <button className="browse-button" onClick={() => setUngroupedLimit((limit) => limit + PAGE_SIZE)}>显示更多未分组会话（{Math.min(ungroupedLimit, view.ungroupedThreadIds.length)} / {view.ungroupedThreadIds.length}）</button>}
      </div>
      {view.crossRelationIds.length > 0 && <div className="workstream-cross"><h3>跨工作流关系</h3>
        {view.crossRelationIds.slice(0, crossLimit).map((id) => { const item = relation(id);
          return item && <div key={id}>{sourceName(item)} · {item.kind} · {title(item.fromThreadId)} → {title(item.toThreadId)}</div>;
        })}{crossLimit < view.crossRelationIds.length && <button className="browse-button" onClick={() => setCrossLimit((limit) => limit + PAGE_SIZE)}>显示更多跨工作流关系（{Math.min(crossLimit, view.crossRelationIds.length)} / {view.crossRelationIds.length}）</button>}</div>}
    </>}
  </section>;
}
