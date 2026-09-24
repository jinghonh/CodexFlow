import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { formatAppError } from "./appError";

type Limits = { callLimit: number; concurrencyLimit: number; timeoutSeconds: number; retryLimit: number; inputCharacterLimit: number };
type Stage = { stage: "summary" | "relation" | "evidenceSelection" | "naming"; service: string; model: string;
  sendScope: string; pendingItems: number; maximumCalls: number; available: boolean; note: string };
type Preview = { projectId: string; inputVersion: string; stages: Stage[]; cachedSummaries: number;
  unavailableSummaries: number; maximumCandidates: number; evidenceSelectionCallLimit: number;
  pendingGroups: number | null; limits: Limits; jevConfigured: boolean };
type Run = { id: string; projectId: string; state: "queued" | "running" | "cancelling" | "cancelled" | "paused" | "complete" | "partial" | "failed";
  pauseReason: string | null; batchNumber: number; batchCalls: number; totalCalls: number; totalQuestions: number;
  jevPinnedModel?: string | null; jevProbeAttempts?: number;
  inputTokens: number | null; outputTokens: number | null; processed: number; succeeded: number; failed: number; pending: number;
  interrupted: boolean; error: { message: string } | null; limits: Limits;
  units: { id: string; stage: Stage["stage"]; state: string; attempts: number; actualModel: string | null; error: { message: string } | null }[] };
const initialLimits: Limits = { callLimit: 100, concurrencyLimit: 2, timeoutSeconds: 180, retryLimit: 2, inputCharacterLimit: 40000 };
const stageNames: Record<Stage["stage"], string> = { summary: "会话总结", relation: "候选分类", evidenceSelection: "证据选择", naming: "工作流命名" };
const stateNames: Record<Run["state"], string> = { queued: "待执行", running: "执行中", cancelling: "取消中", cancelled: "已取消",
  paused: "已暂停", complete: "完成", partial: "部分完成", failed: "失败" };
function message(error: unknown): string {
  return formatAppError(error, "分析操作失败。");
}

export function ProjectAnalysisView({ projectId, refreshVersion, settingsRevision = 0, onRelationResultsChanged }: {
  projectId: string; refreshVersion: string; settingsRevision?: number; onRelationResultsChanged?: () => void;
}) {
  const [limits, setLimits] = useState<Limits>(initialLimits);
  const [previewState, setPreviewState] = useState<{ key: string; value: Preview } | null>(null);
  const [run, setRun] = useState<Run | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const succeededRelationUnits = run?.units
    .filter((unit) => (unit.stage === "relation" || unit.stage === "evidenceSelection") && unit.state === "succeeded")
    .map((unit) => unit.id).sort().join("|") || "";
  const relationResultsRevision = succeededRelationUnits ? `${run?.id}:${succeededRelationUnits}` : "";
  useEffect(() => {
    if (relationResultsRevision) onRelationResultsChanged?.();
  }, [relationResultsRevision, onRelationResultsChanged]);
  const terminalRun = run && ["paused", "cancelled", "complete", "partial", "failed"].includes(run.state)
    ? `${run.id}:${run.state}:${run.totalCalls}:${run.processed}` : "";
  const previewKey = JSON.stringify([projectId, refreshVersion, settingsRevision, limits, terminalRun]);
  const preview = previewState?.key === previewKey ? previewState.value : null;

  useEffect(() => {
    let active = true;
    setPreviewState(null); setError("");
    invoke<Preview>("get_analysis_preview", { projectId, limits })
      .then((value) => { if (active) setPreviewState({ key: previewKey, value }); })
      .catch((caught) => { if (active) setError(message(caught)); });
    return () => { active = false; };
  }, [previewKey]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    setRun(null);
    invoke<Run | null>("get_latest_analysis_run", { projectId })
      .then((value) => { if (active) setRun(value); })
      .catch((caught) => { if (active) setError(message(caught)); });
    listen<Run>("analysis-run", (event) => {
      if (active && event.payload.projectId === projectId) setRun(event.payload);
    }).then((stop) => { if (active) unlisten = stop; else stop(); }).catch(() => {});
    return () => { active = false; unlisten?.(); };
  }, [projectId]);

  useEffect(() => {
    if (!run || !["queued", "running", "cancelling"].includes(run.state)) return;
    let active = true;
    const timer = window.setInterval(() => {
      invoke<Run | null>("get_analysis_run", { runId: run.id })
        .then((value) => { if (active && value) setRun(value); })
        .catch((caught) => { if (active) setError(message(caught)); });
    }, 1000);
    return () => { active = false; window.clearInterval(timer); };
  }, [run?.id, run?.state]);

  async function operate(command: string, args: Record<string, unknown>) {
    setBusy(true); setError("");
    try { setRun(await invoke<Run>(command, args)); }
    catch (caught) { setError(message(caught)); }
    finally { setBusy(false); }
  }

  const active = run?.state === "queued" || run?.state === "running" || run?.state === "cancelling";
  const summary = preview?.stages[0];
  const canStart = (!!summary?.available || (summary?.pendingItems ?? 0) === 0) &&
    (((summary?.pendingItems ?? 0) > 0) ||
      (!!preview?.stages[1]?.available && (preview?.stages[1]?.pendingItems ?? 0) > 0) ||
      (!!preview?.stages[3]?.available && (preview?.stages[3]?.pendingItems ?? 0) > 0)) &&
    !active && run?.state !== "paused" && !busy;
  const valid = Number.isInteger(limits.callLimit) && limits.callLimit > 0 && Number.isInteger(limits.timeoutSeconds) && limits.timeoutSeconds > 0 &&
    Number.isInteger(limits.concurrencyLimit) && limits.concurrencyLimit >= 1 && limits.concurrencyLimit <= 2 &&
    Number.isInteger(limits.retryLimit) && limits.retryLimit >= 0 && limits.retryLimit <= 2 &&
    Number.isInteger(limits.inputCharacterLimit) && limits.inputCharacterLimit >= 2000;

  return <section id="project-analysis" className="panel analysis-panel" aria-label="项目分析批次">
    <div className="panel-kicker">06 / 项目分析</div>
    <h2>预览与批次</h2>
    <p className="panel-intro">只有手动启动才会调用模型。预览使用已保存的文本服务与 Jev 设置。文本服务负责会话总结和工作流命名，Jev 负责关系判断和证据选择。调用次数是上限，不是费用或令牌估计。</p>
    <div className="analysis-limits">
      <label>本批调用上限<input aria-label="本批调用上限" type="number" min="1" value={limits.callLimit} onChange={(event) => setLimits({ ...limits, callLimit: Number(event.target.value) })} /></label>
      <label>总并发上限<input aria-label="总并发上限" type="number" min="1" max="2" value={limits.concurrencyLimit} onChange={(event) => setLimits({ ...limits, concurrencyLimit: Number(event.target.value) })} /></label>
      <label>单次超时（秒）<input aria-label="单次超时" type="number" min="1" value={limits.timeoutSeconds} onChange={(event) => setLimits({ ...limits, timeoutSeconds: Number(event.target.value) })} /></label>
      <label>自动重试次数<input aria-label="自动重试次数" type="number" min="0" max="2" value={limits.retryLimit} onChange={(event) => setLimits({ ...limits, retryLimit: Number(event.target.value) })} /></label>
      <label>单次输入字符上限<input aria-label="单次输入字符上限" type="number" min="2000" value={limits.inputCharacterLimit} onChange={(event) => setLimits({ ...limits, inputCharacterLimit: Number(event.target.value) })} /></label>
    </div>
    {!valid && <p className="page-error" role="alert">调用上限和超时须为正整数；总并发为 1–2，自动重试为 0–2，输入至少 2000 字符。</p>}
    {error && <p className="page-error" role="alert">{error}</p>}
    {!preview && !error && <p>正在计算分析预览…</p>}
    {preview && <>
      <div className="candidate-summary"><strong>{summary?.pendingItems ?? 0} 条待总结</strong><span>{preview.cachedSummaries} 条有效缓存</span><span>{preview.unavailableSummaries} 条来源内容暂不可用</span></div>
      <div className="analysis-stage-list">{preview.stages.map((stage) => <div className="analysis-stage" key={stage.stage}>
        <strong>{stageNames[stage.stage]} · {stage.available ? "可执行" : "尚不可执行"}</strong>
        <span>{stage.service} / {stage.model}</span>
        <span>待处理 {stage.pendingItems}；调用上界 {stage.maximumCalls}</span>
        <small>{stage.sendScope}</small><small>{stage.note}</small>
      </div>)}</div>
      <p className="analysis-note">候选最多 {preview.maximumCandidates} 对，证据选择最多 {preview.evidenceSelectionCallLimit} 次 Jev POST；每对两阶段合计最多两次推理请求。待命名分组尚未知。Jev {preview.jevConfigured ? "已配置" : "未配置"}，关系材料将发送到预览所示服务。缓存命中不计推理，重试另计调用。</p>
    </>}
    <div className="summary-actions"><button className="primary-button" disabled={!valid || !canStart} onClick={() => void operate("start_project_analysis", { projectId, limits })}>启动项目分析</button>
      {active && run?.state !== "cancelling" && <button className="browse-button" disabled={busy || !!run.pauseReason} onClick={() => void operate("pause_analysis_run", { runId: run.id })}>暂停</button>}
      {(active || run?.state === "paused") && <button className="browse-button" disabled={busy || run?.state === "cancelling"} onClick={() => void operate("cancel_analysis_run", { runId: run!.id })}>取消</button>}
      {run && ["paused", "cancelled", "partial", "failed"].includes(run.state) && <button className="browse-button" disabled={busy || !valid} onClick={() => void operate("continue_analysis_run", { runId: run.id, callLimit: limits.callLimit })}>继续未完成项</button>}
    </div>
    {run && <div className="analysis-progress" role="status"><strong>分析{stateNames[run.state]} · 第 {run.batchNumber} 批</strong>
      <span>运行 {run.id}</span><span>已处理 {run.processed}；成功 {run.succeeded}；失败 {run.failed}；待处理 {run.pending}</span>
      <span>本批 {run.batchCalls} / {run.limits.callLimit} 次；累计 {run.totalCalls} 次调用，{run.totalQuestions} 道题</span>
      <span>实际模型：{[...new Set(run.units.map((unit) => unit.actualModel).filter(Boolean))].join("、") || "待确认"}</span>
      {run.jevPinnedModel && <span>Jev 本批固定版本：{run.jevPinnedModel}{run.jevProbeAttempts ? `（合成探测 ${run.jevProbeAttempts} 次）` : "（固定版本无需探测）"}</span>}
      {(run.inputTokens !== null || run.outputTokens !== null) && <span>服务回报用量：输入 {run.inputTokens ?? "未知"}，输出 {run.outputTokens ?? "未知"} 令牌</span>}
      <small>此运行固定启动时的模型、超时、重试和输入上限；继续时仅使用上方新的调用上限。</small>
      {run.interrupted && <em>应用退出中断后已恢复状态，可继续未完成单元。</em>}
      {run.pauseReason && <em>{run.pauseReason}</em>}
      {run.error && <em>{message(run.error)}</em>}
      {run.units.filter((unit) => unit.state === "failed").slice(0, 5).map((unit) => <small key={unit.id}>{unit.id}：{unit.error ? message(unit.error) : "分析失败"}</small>)}
      {run.state === "cancelled" && <small>取消已确认本地请求结束；远端服务仍可能计费。</small>}
    </div>}
  </section>;
}
