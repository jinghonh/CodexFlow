import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { formatAppError } from "./appError";

type Limits = { callLimit: number; concurrencyLimit: number; timeoutSeconds: number; retryLimit: number; inputCharacterLimit: number };
type StageSelection = { summary: boolean; relations: boolean; naming: boolean };
type PanelStage = "summary" | "relations" | "naming";
type Stage = { stage: "summary" | "relation" | "evidenceSelection" | "naming"; service: string; model: string;
  sendScope: string; pendingItems: number; maximumCalls: number; available: boolean; note: string };
type Preview = { projectId: string; inputVersion: string; stages: Stage[]; cachedSummaries: number;
  unavailableSummaries: number; maximumCandidates: number; evidenceSelectionCallLimit: number;
  pendingGroups: number | null; limits: Limits; jevConfigured: boolean };
type Run = { id: string; projectId: string; state: "queued" | "running" | "cancelling" | "cancelled" | "paused" | "complete" | "partial" | "failed";
  pauseReason: string | null; batchNumber: number; batchCalls: number; totalCalls: number; totalQuestions: number;
  stageSelection?: StageSelection;
  jevPinnedModel?: string | null; jevProbeAttempts?: number;
  inputTokens: number | null; outputTokens: number | null; processed: number; succeeded: number; failed: number; pending: number;
  interrupted: boolean; error: { message: string } | null; limits: Limits;
  units: { id: string; stage: Stage["stage"]; state: string; attempts: number; actualModel: string | null; error: { message: string } | null }[] };

const initialLimits: Limits = { callLimit: 100, concurrencyLimit: 2, timeoutSeconds: 180, retryLimit: 2, inputCharacterLimit: 40000 };
const stageNames: Record<Stage["stage"], string> = { summary: "会话总结", relation: "候选关系判断", evidenceSelection: "证据选择", naming: "工作流命名" };
const panelStageNames: Record<PanelStage, string> = { summary: "会话总结", relations: "候选关系判断", naming: "工作流命名" };
const stateNames: Record<Run["state"], string> = { queued: "待执行", running: "执行中", cancelling: "取消中", cancelled: "已取消",
  paused: "已暂停", complete: "完成", partial: "部分完成", failed: "失败" };

function message(error: unknown): string {
  return formatAppError(error, "分析操作失败。");
}

function stageSelectionFor(stage: PanelStage): StageSelection {
  return { summary: stage === "summary", relations: stage === "relations", naming: stage === "naming" };
}

function runIncludesStage(run: Run, stage: PanelStage): boolean {
  if (!run.stageSelection) return true;
  return stage === "summary" ? run.stageSelection.summary
    : stage === "relations" ? run.stageSelection.relations : run.stageSelection.naming;
}

function selectionDescription(selection?: StageSelection): string {
  if (!selection) return "会话总结、候选关系判断和工作流命名";
  const selected = [
    selection.summary ? "会话总结" : null,
    selection.relations ? "候选关系判断及必要的证据选择" : null,
    selection.naming ? "工作流命名" : null,
  ].filter((item): item is string => item !== null);
  return selected.join("、") || "无阶段";
}

export function ProjectAnalysisView({ projectId, refreshVersion, settingsRevision = 0, stage = "summary", onRelationResultsChanged }: {
  projectId: string; refreshVersion: string; settingsRevision?: number; stage?: PanelStage; onRelationResultsChanged?: () => void;
}) {
  const [limits, setLimits] = useState<Limits>(initialLimits);
  const stageSelection = stageSelectionFor(stage);
  const [previewState, setPreviewState] = useState<{ key: string; value: Preview } | null>(null);
  const [run, setRun] = useState<Run | null>(null);
  const [runLoaded, setRunLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const succeededRelationUnits = run?.units
    .filter((unit) => (unit.stage === "relation" || unit.stage === "evidenceSelection") && unit.state === "succeeded")
    .map((unit) => unit.id).sort().join("|") || "";
  const relationResultsRevision = succeededRelationUnits ? `${run?.id}:${succeededRelationUnits}` : "";
  useEffect(() => {
    if (stage === "relations" && relationResultsRevision) onRelationResultsChanged?.();
  }, [stage, relationResultsRevision, onRelationResultsChanged]);
  const terminalRun = run && ["paused", "cancelled", "complete", "partial", "failed"].includes(run.state)
    ? `${run.id}:${run.state}:${run.totalCalls}:${run.processed}` : "";
  const previewKey = JSON.stringify([projectId, refreshVersion, settingsRevision, limits, stageSelection, terminalRun]);
  const preview = previewState?.key === previewKey ? previewState.value : null;

  useEffect(() => {
    let active = true;
    setPreviewState(null); setError("");
    invoke<Preview>("get_analysis_preview", { projectId, limits, stageSelection })
      .then((value) => { if (active) setPreviewState({ key: previewKey, value }); })
      .catch((caught) => { if (active) setError(message(caught)); });
    return () => { active = false; };
  }, [previewKey]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    setRun(null);
    setRunLoaded(false);
    invoke<Run | null>("get_latest_analysis_run", { projectId })
      .then((value) => { if (active) setRun(value); })
      .catch((caught) => { if (active) setError(message(caught)); })
      .finally(() => { if (active) setRunLoaded(true); });
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
  const runBelongsHere = !!run && runIncludesStage(run, stage);
  const blockingRun = !!run && (active || run.state === "paused") && !runBelongsHere;
  const summaryStage = preview?.stages.find((item) => item.stage === "summary");
  const relationStage = preview?.stages.find((item) => item.stage === "relation");
  const namingStage = preview?.stages.find((item) => item.stage === "naming");
  const namingMayRun = stage === "naming" && ((preview?.pendingGroups ?? 0) > 0);
  const selectedWorkPending = stage === "summary" ? (summaryStage?.pendingItems ?? 0) > 0
    : stage === "relations" ? !!relationStage?.available && (relationStage.pendingItems > 0)
      : namingMayRun && !!namingStage?.available;
  const textWorkUnavailable = (stage === "summary" && (summaryStage?.pendingItems ?? 0) > 0 && !summaryStage?.available) ||
    (stage === "naming" && namingMayRun && !namingStage?.available);
  const relationWorkUnavailable = stage === "relations" && (relationStage?.pendingItems ?? 0) > 0 && !relationStage?.available;
  const canStart = selectedWorkPending && !textWorkUnavailable && !relationWorkUnavailable &&
    !active && run?.state !== "paused" && !busy && runLoaded;
  const valid = Number.isInteger(limits.callLimit) && limits.callLimit > 0 && Number.isInteger(limits.timeoutSeconds) && limits.timeoutSeconds > 0 &&
    Number.isInteger(limits.concurrencyLimit) && limits.concurrencyLimit >= 1 && limits.concurrencyLimit <= 2 &&
    Number.isInteger(limits.retryLimit) && limits.retryLimit >= 0 && limits.retryLimit <= 2 &&
    Number.isInteger(limits.inputCharacterLimit) && limits.inputCharacterLimit >= 2000;
  const visibleStages = preview?.stages.filter((item) => stage === "relations"
    ? item.stage === "relation" || item.stage === "evidenceSelection"
    : item.stage === stage) ?? [];

  return <section className="panel analysis-panel" aria-label={`${panelStageNames[stage]}分析`}>
    <div className="panel-kicker">分析阶段</div>
    <h2>{panelStageNames[stage]}</h2>
    <p className="panel-intro">{stage === "summary"
      ? "只处理尚无有效总结的会话。启动后按本阶段设置运行，并将结果保存到对应会话。同一项目一次只运行一个分析批次。"
      : stage === "relations"
        ? "判断待处理的关系候选；符合条件时自动选择证据。结果会进入关系图。同一项目一次只运行一个分析批次。"
        : "为已有关系分组生成工作流名称。请在关系判断完成后运行；同一项目一次只运行一个分析批次。"}</p>
    <details className="analysis-settings">
      <summary>运行参数</summary>
      <div className="analysis-limits">
        <label>本批调用上限<input aria-label="本批调用上限" type="number" min="1" value={limits.callLimit} onChange={(event) => setLimits({ ...limits, callLimit: Number(event.target.value) })} /></label>
        <label>总并发上限<input aria-label="总并发上限" type="number" min="1" max="2" value={limits.concurrencyLimit} onChange={(event) => setLimits({ ...limits, concurrencyLimit: Number(event.target.value) })} /></label>
        <label>单次超时（秒）<input aria-label="单次超时" type="number" min="1" value={limits.timeoutSeconds} onChange={(event) => setLimits({ ...limits, timeoutSeconds: Number(event.target.value) })} /></label>
        <label>自动重试次数<input aria-label="自动重试次数" type="number" min="0" max="2" value={limits.retryLimit} onChange={(event) => setLimits({ ...limits, retryLimit: Number(event.target.value) })} /></label>
        <label>单次输入字符上限<input aria-label="单次输入字符上限" type="number" min="2000" value={limits.inputCharacterLimit} onChange={(event) => setLimits({ ...limits, inputCharacterLimit: Number(event.target.value) })} /></label>
      </div>
      {!valid && <p className="page-error" role="alert">调用上限和超时须为正整数；总并发为 1–2，自动重试为 0–2，输入至少 2000 字符。</p>}
      <small className="analysis-note">参数仅用于本阶段新启动的批次；继续已有批次时沿用原有参数，仅更新调用上限。</small>
    </details>
    {textWorkUnavailable && <p className="analysis-note">待处理任务需要文本服务。请先在“来源设置”面板配置文本服务。</p>}
    {relationWorkUnavailable && <p className="analysis-note">待处理候选需要 Jev。请先在“来源设置”面板配置 Jev。</p>}
    {run?.state === "paused" && runBelongsHere && <p className="analysis-note">已暂停批次继续时沿用启动时的阶段选择与运行参数。</p>}
    {run && ["cancelled", "partial", "failed"].includes(run.state) && runBelongsHere && <p className="analysis-note">继续未完成项会沿用已保存阶段与运行参数；新批次会使用当前设置。</p>}
    {blockingRun && <p className="analysis-note" role="status">此项目的{selectionDescription(run?.stageSelection)}批次{run?.state === "paused" ? "已暂停" : "正在运行"}。请到所属阶段面板继续或取消后再启动当前阶段。</p>}
    {error && <p className="page-error" role="alert">{error}</p>}
    {!preview && !error && <p>正在计算本阶段预览…</p>}
    {preview && <>
      <div className="candidate-summary">
        {stage === "summary" && <><strong>{summaryStage?.pendingItems ?? 0} 条待总结</strong><span>{preview.cachedSummaries} 条有效缓存</span><span>{preview.unavailableSummaries} 条来源内容暂不可用</span></>}
        {stage === "relations" && <><strong>{relationStage?.pendingItems ?? 0} 对待判断</strong><span>最多检查 {preview.maximumCandidates} 对候选</span><span>证据选择最多 {preview.evidenceSelectionCallLimit} 次</span></>}
        {stage === "naming" && <><strong>{preview.pendingGroups ?? 0} 组待命名</strong><span>工作流命名使用已保存的关系分组</span></>}
      </div>
      <div className="analysis-stage-list">{visibleStages.map((item) => {
        const status = item.stage === "evidenceSelection" && item.available ? "满足条件时执行"
          : item.pendingItems === 0 ? "无需处理" : item.available ? "可执行" : "尚不可执行";
        return <div className="analysis-stage" key={item.stage}>
          <strong>{stageNames[item.stage]} · {status}</strong>
          <span>{item.service} / {item.model}</span>
          <span>待处理 {item.pendingItems}；调用上界 {item.maximumCalls}</span>
          <small>{item.sendScope}</small><small>{item.note}</small>
        </div>;
      })}</div>
      {stage === "relations" && <p className="analysis-note">候选排序仅用于筛选，不代表关系概率；证据选择会在判断发现支持关系时自动执行。缓存命中不计推理调用，重试另计调用。Jev {preview.jevConfigured ? "已配置" : "未配置"}。</p>}
      {stage === "naming" && preview.pendingGroups === null && <p className="analysis-note">当前待命名分组尚未计算。完成关系判断后重新载入此面板。</p>}
    </>}
    <div className="summary-actions">
      <button className="primary-button" disabled={!valid || !canStart} onClick={() => void operate("start_project_analysis", { projectId, limits, stageSelection })}>启动{panelStageNames[stage]}</button>
      {runBelongsHere && active && run?.state !== "cancelling" && <button className="browse-button" disabled={busy || !!run?.pauseReason} onClick={() => void operate("pause_analysis_run", { runId: run!.id })}>暂停</button>}
      {runBelongsHere && (active || run?.state === "paused") && <button className="browse-button" disabled={busy || run?.state === "cancelling"} onClick={() => void operate("cancel_analysis_run", { runId: run!.id })}>取消</button>}
      {runBelongsHere && run && ["paused", "cancelled", "partial", "failed"].includes(run.state) && <button className="browse-button" disabled={busy || !valid} onClick={() => void operate("continue_analysis_run", { runId: run.id, callLimit: limits.callLimit })}>继续未完成项</button>}
    </div>
    {runBelongsHere && run && <div className="analysis-progress" role="status"><strong>分析{stateNames[run.state]} · 第 {run.batchNumber} 批</strong>
      <span>运行 {run.id}</span><span>已处理 {run.processed}；成功 {run.succeeded}；失败 {run.failed}；待处理 {run.pending}</span>
      <span>本次选择：{selectionDescription(run.stageSelection)}</span>
      <span>本批 {run.batchCalls} / {run.limits.callLimit} 次；累计 {run.totalCalls} 次调用，{run.totalQuestions} 道题</span>
      <span>实际模型：{[...new Set(run.units.map((unit) => unit.actualModel).filter(Boolean))].join("、") || "待确认"}</span>
      {run.jevPinnedModel && <span>Jev 本批固定版本：{run.jevPinnedModel}{run.jevProbeAttempts ? `（合成探测 ${run.jevProbeAttempts} 次）` : "（固定版本无需探测）"}</span>}
      {(run.inputTokens !== null || run.outputTokens !== null) && <span>服务回报用量：输入 {run.inputTokens ?? "未知"}，输出 {run.outputTokens ?? "未知"} 令牌</span>}
      <small>此批次固定启动时的模型、超时、重试和输入上限；继续时仅使用上方新的调用上限。</small>
      {run.interrupted && <em>应用退出中断后已恢复状态，可继续未完成单元。</em>}
      {run.pauseReason && <em>{run.pauseReason}</em>}
      {run.error && <em>{message(run.error)}</em>}
      {run.units.filter((unit) => unit.state === "failed").slice(0, 5).map((unit) => <small key={unit.id}>{unit.id}：{unit.error ? message(unit.error) : "分析失败"}</small>)}
      {run.state === "cancelled" && <small>取消已确认本地请求结束；远端服务仍可能计费。</small>}
    </div>}
  </section>;
}
