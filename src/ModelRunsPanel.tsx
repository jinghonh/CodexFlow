import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";
import { ProgressMeter } from "./ProgressMeter";
import { projectStateNames } from "./analysisProgress";
import type { ModelRunsSnapshot, Progress } from "./analysisProgress";
import { analysisIntentHasRun, analysisStartIntents, analysisStartIntentsChanged, dismissAnalysisStartIntent, recoverAnalysisStarts } from "./analysisStartIntent";
import type { AnalysisStartIntent } from "./analysisStartIntent";

type Entry = { id: string; kind: "project" | "summary" | "semantic"; name: string;
  state: string; progress: Progress; started: number; projectId?: string; threadId?: string;
  error?: string; result: string; active: boolean; preparing?: boolean };

function entries(snapshot: ModelRunsSnapshot): Entry[] {
  return snapshot.runs.map((run): Entry => ({
    id: run.id, kind: run.kind, name: run.name, state: projectStateNames[run.state],
    progress: { stage: run.stage, completed: run.completed, total: run.total, unit: run.unit, calls: run.calls },
    started: run.startedAtUnixMs, projectId: run.projectId ?? undefined, threadId: run.threadId ?? undefined,
    error: run.error ?? undefined, result: run.result,
    active: ["queued", "running", "cancelling"].includes(run.state),
  }));
}

export function ModelRunsPanel({ onOpen }: { onOpen: (entry: { kind: Entry["kind"]; name: string; projectId?: string; threadId?: string }) => void }) {
  const [snapshot, setSnapshot] = useState<ModelRunsSnapshot | null>(null);
  const [pending, setPending] = useState<AnalysisStartIntent[]>(analysisStartIntents);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState("");
  const [busyId, setBusyId] = useState("");

  async function refresh() {
    try { setSnapshot(await invoke<ModelRunsSnapshot>("get_model_runs")); setError(""); }
    catch (cause) { setError(formatAppError(cause, "读取分析运行失败。")); }
  }

  useEffect(() => {
    void refresh();
    void recoverAnalysisStarts();
    const timer = window.setInterval(() => void refresh(), 2000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    const update = () => setPending(analysisStartIntents());
    window.addEventListener(analysisStartIntentsChanged, update);
    return () => window.removeEventListener(analysisStartIntentsChanged, update);
  }, []);

  const runs = useMemo(() => {
    const starting: Entry[] = pending.filter((intent) => !snapshot || !analysisIntentHasRun(intent, snapshot)).map((intent) => ({
      id: intent.id, kind: intent.kind,
      name: intent.kind === "summary" ? "单条会话总结" : intent.stageSelection?.relations ? "候选关系判断"
        : intent.stageSelection?.naming ? "工作流命名" : "项目会话总结",
      state: intent.state === "preparing" ? "准备中" : "失败",
      progress: { stage: "读取分析材料", completed: 0, total: null, unit: "项", calls: 0 },
      started: intent.startedAt, projectId: intent.projectId, threadId: intent.threadId,
      error: intent.error, result: "", active: intent.state === "preparing", preparing: true,
    }));
    return [...starting, ...(snapshot ? entries(snapshot) : [])].sort((a, b) => b.started - a.started);
  }, [snapshot, pending]);
  const activeCount = runs.filter((run) => run.active).length;

  async function cancel(entry: Entry) {
    setBusyId(entry.id); setError("");
    try {
      if (entry.kind === "project") await invoke("cancel_analysis_run", { runId: entry.id });
      else if (entry.kind === "summary") await invoke("cancel_summary_run", { runId: entry.id });
      else await invoke("cancel_semantic_index");
      await refresh();
    } catch (cause) { setError(formatAppError(cause, "取消分析失败。")); }
    finally { setBusyId(""); }
  }

  return <>
    <button className="model-runs-toggle" aria-expanded={open} onClick={() => setOpen((value) => !value)}>
      运行列表{activeCount > 0 && <strong>{activeCount}</strong>}
    </button>
    {open && <div className="model-runs-backdrop" onClick={() => setOpen(false)}>
      <section className="model-runs-panel" role="dialog" aria-label="分析运行列表" onClick={(event) => event.stopPropagation()}>
        <div className="model-runs-heading"><div><span className="panel-kicker">模型分析</span><h2>运行列表</h2></div><button className="plain-button" onClick={() => setOpen(false)}>关闭</button></div>
        {error && <p className="page-error" role="alert">{error}</p>}
        {runs.length === 0 ? <p className="empty-list">还没有模型分析运行。</p> : <div className="model-runs-list">{runs.map((run) => <article className="model-run-card" key={run.id}>
          <div className="model-run-heading"><strong>{run.name}</strong><span>{run.state}</span></div>
          <small>{new Date(run.started).toLocaleString("zh-CN")}{run.projectId ? ` · 项目 ${run.projectId}` : ""}{run.threadId ? ` · 会话 ${run.threadId}` : ""}</small>
          <ProgressMeter progress={run.progress} label={`${run.name}进度`} totalMayChange={run.name === "候选关系判断" || run.name === "工作流命名" || run.kind === "semantic"} />
          {run.progress.calls > 0 && <small>模型调用 {run.progress.calls} 次</small>}
          {run.result && <p>{run.result}</p>}
          {run.error && <p className="model-run-error">{run.error}</p>}
          <div className="model-run-actions"><button className="browse-button" onClick={() => { onOpen(run); setOpen(false); }}>查看所在页面</button>
            {run.active && !run.preparing && run.state !== "取消中" && <button className="plain-button" disabled={busyId === run.id} onClick={() => void cancel(run)}>取消</button>}
            {run.preparing && !run.active && <button className="plain-button" onClick={() => dismissAnalysisStartIntent(run.id)}>移除记录</button>}</div>
        </article>)}</div>}
      </section>
    </div>}
  </>;
}
