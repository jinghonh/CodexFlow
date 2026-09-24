import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { formatAppError } from "./appError";

type Summary = { content: { goal: string; activity: string; outcome: string; decisions: string; issues: string };
  evidenceIds: string[]; model: string; createdAtUnixMs: number; sourceUpdatedAt: number; inputDigest?: string };
type Preview = { model: string; characterLimit: number; characterCount: number; totalFacts: number;
  includedFacts: number; totalMessages: number; includedMessages: number; truncated: boolean;
  turnsComplete: boolean; itemsComplete: boolean; sourceCurrent: boolean; contentAvailable: boolean;
  readError?: string | null;
  cachedSummary: Summary | null; cacheCurrent: boolean; staleReason?: string | null; analysisBlockedReason: string | null };
type Run = { id: string; state: "running" | "cancelling" | "complete" | "failed" | "cancelled";
  model: string; temporaryThreadId: string | null; turnId: string | null; reusedCache: boolean;
  error: { message: string } | null };
type Location = { turnId: string; turnOffset: number; offset: number };
type SummaryEvidenceCheck = { id: string; state: "valid" | "missingThread" | "missingTurn" | "missingItem" |
  "missingFact" | "wrongHierarchy" | "excerptMissing" | "staleVersion"; message: string;
  itemId: string | null; location: Location | null; excerpt: string | null };

function errorText(error: unknown): string {
  return formatAppError(error, "总结请求失败。");
}

export function ThreadSummaryView({ threadId, connected, revision, onLocate }: {
  threadId: string; connected: boolean; revision: number;
  onLocate: (location: Location, itemId: string) => Promise<void>;
}) {
  const loadedThreadId = useRef(threadId);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [run, setRun] = useState<Run | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [evidenceChecks, setEvidenceChecks] = useState<Record<string, SummaryEvidenceCheck>>({});
  const [evidenceMessage, setEvidenceMessage] = useState("");

  useEffect(() => {
    let active = true;
    if (loadedThreadId.current !== threadId) {
      loadedThreadId.current = threadId;
      setPreview(null);
      setRun(null);
    }
    setError("");
    Promise.all([
      invoke<Preview>("get_summary_preview", { threadId }).catch((caught) => {
        if (active) setError(errorText(caught));
        return null;
      }),
      invoke<Run | null>("get_latest_summary_run", { threadId }),
    ]).then(([nextPreview, nextRun]) => { if (active) { if (nextPreview) setPreview(nextPreview); setRun(nextRun); } })
      .catch((caught) => { if (active) setError(errorText(caught)); });
    return () => { active = false; };
  }, [threadId, revision]);

  useEffect(() => { setEvidenceChecks({}); setEvidenceMessage(""); }, [threadId, preview?.cachedSummary?.createdAtUnixMs]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    listen<{ units: { id: string; state: string }[] }>("analysis-run", (event) => {
      if (!active || !event.payload.units.some((unit) => unit.id === threadId && unit.state === "succeeded")) return;
      void Promise.all([
        invoke<Preview>("get_summary_preview", { threadId }),
        invoke<Run | null>("get_latest_summary_run", { threadId }),
      ]).then(([nextPreview, nextRun]) => { if (active) { setPreview(nextPreview); setRun(nextRun); } })
        .catch((caught) => { if (active) setError(errorText(caught)); });
    }).then((stop) => { if (active) unlisten = stop; else stop(); }).catch(() => {});
    return () => { active = false; unlisten?.(); };
  }, [threadId]);

  useEffect(() => {
    if (!run || (run.state !== "running" && run.state !== "cancelling")) return;
    let active = true;
    const timer = window.setInterval(() => {
      void invoke<Run | null>("get_summary_run", { runId: run.id }).then(async (nextRun) => {
        if (!active || !nextRun) return;
        setRun(nextRun);
        if (nextRun.state !== "running" && nextRun.state !== "cancelling") {
          const nextPreview = await invoke<Preview>("get_summary_preview", { threadId });
          if (active) setPreview(nextPreview);
        }
      }).catch((caught) => { if (active) setError(errorText(caught)); });
    }, 700);
    return () => { active = false; window.clearInterval(timer); };
  }, [run?.id, run?.state, threadId]);

  async function generate() {
    setBusy(true); setError("");
    try {
      const nextRun = await invoke<Run>("start_thread_summary", { threadId });
      setRun(nextRun);
      if (nextRun.state !== "running" && nextRun.state !== "cancelling")
        setPreview(await invoke<Preview>("get_summary_preview", { threadId }));
    }
    catch (caught) { setError(errorText(caught)); }
    finally { setBusy(false); }
  }

  async function cancel() {
    if (!run) return;
    setError("");
    try {
      const nextRun = await invoke<Run>("cancel_summary_run", { runId: run.id });
      setRun(nextRun);
      if (nextRun.state !== "running" && nextRun.state !== "cancelling")
        setPreview(await invoke<Preview>("get_summary_preview", { threadId }));
    }
    catch (caught) { setError(errorText(caught)); }
  }

  async function inspectEvidence(evidenceId: string) {
    setEvidenceMessage("");
    try {
      const check = await invoke<SummaryEvidenceCheck>("inspect_summary_evidence", { threadId, evidenceId });
      setEvidenceChecks((previous) => ({ ...previous, [evidenceId]: check }));
      if (check.state !== "valid" || !check.location || !check.itemId) {
        setEvidenceMessage(check.message);
        return;
      }
      await onLocate(check.location, check.itemId);
      setEvidenceMessage(`已定位到回合 ${check.location.turnId} 的来源条目。`);
    } catch (caught) { setEvidenceMessage(errorText(caught)); }
  }

  const running = run?.state === "running" || run?.state === "cancelling";
  const summary = preview?.cachedSummary;
  const labels = { goal: "目标", activity: "活动", outcome: "结果", decisions: "决定", issues: "问题" };
  return <section className="thread-summary" aria-label="会话总结">
    <div className="history-heading"><div><h3>AI 会话总结</h3><small>由 Codex 临时分析会话生成；下列文字是模型解释，来源执行状态以结构化事实为准。</small></div>
      <div className="summary-actions"><button className="browse-button" disabled={!connected || !preview?.contentAvailable || !preview.sourceCurrent || !preview.turnsComplete || !preview.itemsComplete || !!preview.analysisBlockedReason || running || busy} onClick={() => void generate()}>
        {busy ? "正在启动…" : summary ? "重新生成" : "手动生成"}</button>
        {running && <button className="browse-button" disabled={run?.state === "cancelling"} onClick={() => void cancel()}>{run?.state === "cancelling" ? "取消中…" : "取消分析"}</button>}
      </div></div>
    {preview && <div className="summary-coverage">
      <span>下次调用：{preview.model}</span>
      {run && <span>最近运行模型：{run.model}</span>}
      <span>输入：{preview.characterCount.toLocaleString()} / {preview.characterLimit.toLocaleString()} 字符</span>
      <span>事实：{preview.includedFacts} / {preview.totalFacts}；消息：{preview.includedMessages} / {preview.totalMessages}</span>
      {preview.truncated && <em>已截断或抽样，部分内容未发送</em>}
      {(!preview.turnsComplete || !preview.itemsComplete) && <em>部分内容；旧总结保留，完整读取后再分析</em>}
      {preview.readError && <em>读取失败：{preview.readError}</em>}
      {!preview.sourceCurrent && <em>来源已有更新，请重新读取历史</em>}
      {!preview.contentAvailable && preview.turnsComplete && preview.itemsComplete && <em>没有可用会话内容，请先读取历史</em>}
      {preview.analysisBlockedReason && <em role="alert">{preview.analysisBlockedReason}</em>}
      {summary && <em>{preview.cacheCurrent ? "总结有效" : `总结过期：${preview.staleReason ?? "分析输入已变化"}`}</em>}
    </div>}
    {run && <p className="history-lookup-message" role="status">{run.state === "running" ? "分析中" : run.state === "cancelling" ? "取消中，等待回合终态或专用进程退出" : run.state === "complete" ? run.reusedCache ? "已复用有效缓存" : "总结已保存" : run.state === "cancelled" ? "已取消，旧总结保留" : "分析失败，旧总结保留"}{run.error ? `：${errorText(run.error)}` : ""}</p>}
    {error && <p className="page-error" role="alert">{error}</p>}
    {summary ? <div className="summary-fields">{(Object.keys(labels) as (keyof typeof labels)[]).map((key) =>
      <div key={key}><strong>{labels[key]}</strong><p>{summary.content[key]}</p></div>)}
      <div className="summary-evidence"><strong>引用的来源证据</strong>{summary.evidenceIds.map((id) => {
        const check = evidenceChecks[id];
        return <div key={id}><code>{id}</code>
          <button className="browse-button" onClick={() => void inspectEvidence(id)}>检查并定位</button>
          {check && <small className={check.state === "valid" ? "" : "thread-warning"}>{check.message}</small>}
          {check?.excerpt && <blockquote>{check.excerpt}</blockquote>}</div>;
      })}{evidenceMessage && <p role="status">{evidenceMessage}</p>}</div>
      <small>模型 {summary.model} · 来源更新版本 {summary.sourceUpdatedAt} · 输入版本 {summary.inputDigest?.slice(0, 12) ?? "旧版未记录"} · 保存于 {new Date(summary.createdAtUnixMs).toLocaleString("zh-CN")}</small></div>
      : <p className="empty-list">待分析：尚无已保存的 AI 总结。点击“手动生成”后才会调用 Codex。</p>}
  </section>;
}
