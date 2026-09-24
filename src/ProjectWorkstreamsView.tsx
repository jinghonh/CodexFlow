import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Stream = { id: string; name: string; members: string[]; relationIds: string[];
  nameInputVersion: string | null; nameError: string | null };
type View = { workstreams: Stream[]; ungroupedThreadIds: string[]; crossRelationIds: string[] };
type Node = { id: string; title: string | null; referenceOnly: boolean };
type Evidence = { threadId: string; turnId: string; itemId: string };
type Relation = { id: string; fromThreadId: string; toThreadId: string; kind: string;
  source: string; basis?: string; evidence?: Evidence[] | { left: Evidence; right: Evidence } };
type Graph = { nodes: Node[]; relations: Relation[]; derivedRelations: Relation[]; inferredRelations: Relation[] };

export function ProjectWorkstreamsView({ projectId, refreshVersion, onSelectThread, onSelectEvidence }: {
  projectId: string; refreshVersion: number; onSelectThread: (id: string) => void;
  onSelectEvidence: (evidence: { threadId: string; turnId: string; itemId: string }) => void;
}) {
  const [view, setView] = useState<View | null>(null);
  const [graph, setGraph] = useState<Graph | null>(null);
  const [error, setError] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    Promise.all([
      invoke<View>("get_project_workstreams", { projectId }),
      invoke<Graph>("get_project_graph", { projectId }),
    ]).then(([nextView, nextGraph]) => {
      if (active) { setView(nextView); setGraph(nextGraph); setError(""); }
    }).catch((caught) => { if (active) setError(caught?.message || "无法读取工作流。"); });
    return () => { active = false; };
  }, [projectId, refreshVersion]);
  const selected = view?.workstreams.find((item) => item.id === selectedId) ?? view?.workstreams[0];
  const title = (id: string) => graph?.nodes.find((node) => node.id === id)?.title || id;
  const relations = [...(graph?.relations ?? []), ...(graph?.derivedRelations ?? []), ...(graph?.inferredRelations ?? [])];
  const relation = (id: string) => relations.find((item) => item.id === id);
  const evidence = (item: Relation): Evidence | undefined => Array.isArray(item.evidence)
    ? item.evidence[0] : item.evidence?.left;
  const sourceName = (item: Relation) => item.source === "observed" ? "观察关系" : item.source === "derived" ? "规则关系" : "推断关系";
  return <section id="workstreams" className="panel workstreams-panel" aria-label="项目工作流">
    <div className="panel-kicker">04 / 工作流</div><h2>工作流导航</h2>
    <p className="panel-intro">分组由有效关系生成。名称来自 Codex 临时分析；跨组联系和未分组会话保留在这里。</p>
    {error && <p className="page-error" role="alert">{error}</p>}
    {!view && !error && <p>正在读取工作流…</p>}
    {view && <>
      <div className="workstream-navigation">
        {view.workstreams.map((item) => <button key={item.id} className={selected?.id === item.id ? "selected" : ""}
          onClick={() => setSelectedId(item.id)}><strong>{item.name}</strong><small>{item.members.length} 条会话{item.nameInputVersion ? " · 已命名" : " · 暂用名"}</small></button>)}
        {view.workstreams.length === 0 && <p>当前没有可形成工作流的关系。</p>}
      </div>
      {selected && <div className="workstream-detail"><h3>{selected.name}</h3>
        {selected.nameError && <p className="page-error" role="status">命名未完成：{selected.nameError}。分组与来源关系已保留。</p>}
        <div className="workstream-members"><strong>成员</strong>{selected.members.map((id) =>
          <button key={id} onClick={() => onSelectThread(id)}>{title(id)}<small>{id}</small></button>)}</div>
        <div className="workstream-relations"><strong>内部来源关系</strong>{selected.relationIds.map((id) => {
          const item = relation(id);
          return item && <div key={id}><span>{sourceName(item)} · {item.kind} · {title(item.fromThreadId)} → {title(item.toThreadId)}</span>
            {item.basis && <small>{item.basis}</small>}
            {evidence(item) && <button className="plain-button" onClick={() => onSelectEvidence(evidence(item)!)}>查看来源</button>}
          </div>;
        })}</div>
      </div>}
      <div className="workstream-ungrouped"><h3>未分组会话 · {view.ungroupedThreadIds.length}</h3>
        {view.ungroupedThreadIds.map((id) => <button key={id} onClick={() => onSelectThread(id)}>{title(id)}<small>{id}</small></button>)}
      </div>
      {view.crossRelationIds.length > 0 && <div className="workstream-cross"><h3>跨工作流关系</h3>
        {view.crossRelationIds.map((id) => { const item = relation(id);
          return item && <div key={id}>{sourceName(item)} · {item.kind} · {title(item.fromThreadId)} → {title(item.toThreadId)}</div>;
        })}</div>}
    </>}
  </section>;
}
