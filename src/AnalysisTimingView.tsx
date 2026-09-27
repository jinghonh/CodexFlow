import { useEffect, useState } from "react";
import type { ExecutionTiming, ProjectRun, SummaryRun } from "./analysisProgress";

export function elapsedTime(timing: ExecutionTiming | null | undefined, now: number): number | null {
  if (!timing) return null;
  return timing.elapsedMs + (timing.activeSinceUnixMs == null ? 0 : Math.max(0, now - timing.activeSinceUnixMs));
}

export function formatElapsed(milliseconds: number | null): string {
  if (milliseconds === null) return "未记录";
  if (milliseconds < 1000) return `${Math.floor(milliseconds)} 毫秒`;
  const seconds = Math.floor(milliseconds / 1000);
  if (seconds < 60) return `${seconds} 秒`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`;
  return `${Math.floor(seconds / 3600)} 小时 ${Math.floor(seconds % 3600 / 60)} 分 ${seconds % 60} 秒`;
}

function useNow(active: boolean) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active]);
  return now;
}

function timingText(timing: ExecutionTiming | null | undefined, now: number) {
  return `${timing?.incomplete ? "至少 " : ""}${formatElapsed(elapsedTime(timing, now))}`;
}

export function PreparingTimingView() {
  const [startedAt] = useState(Date.now);
  const now = useNow(true);
  return <span>准备耗时 {formatElapsed(Math.max(0, now - startedAt))}</span>;
}

const stageNames = { summary: "会话总结", relation: "关系判断", evidenceSelection: "证据选择", naming: "工作流命名" };
const unitStateNames: Record<string, string> = { pending: "待执行", running: "执行中", succeeded: "完成", failed: "失败" };

export function AnalysisTimingView({ run }: { run: ProjectRun }) {
  const now = useNow(run.timing?.activeSinceUnixMs != null);
  return <div className="analysis-timing">
    <span>累计耗时 <strong>{timingText(run.timing, now)}</strong></span>
    {run.limits.concurrencyLimit != null && <span>
      {["queued", "running", "cancelling"].includes(run.state) && `执行中 ${run.units.filter((unit) => unit.state === "running").length} 项 · `}
      本批并发上限 {run.limits.concurrencyLimit}
    </span>}
    <details className="technical-details">
      <summary>耗时明细</summary>
      <div className="analysis-stage-times">{run.stageTimings?.map((item) =>
        <span key={item.stage}>{stageNames[item.stage]} · {timingText(item.timing, now)}</span>)}</div>
      <div className="analysis-timing-table"><table>
        <thead><tr><th>任务</th><th>状态</th><th>累计耗时</th><th>服务调用</th></tr></thead>
        <tbody>{run.units.map((unit, index) => <tr key={`${unit.stage}:${unit.id}`}>
          <td><span>{stageNames[unit.stage]} {index + 1}</span><small title={unit.id}>{unit.id}</small></td>
          <td>{unitStateNames[unit.state] ?? unit.state}</td>
          <td>{timingText(unit.timing, now)}</td>
          <td>{unit.state === "succeeded" && unit.attempts === 0 && !unit.requestTiming
            ? unit.actualModel ? "复用缓存" : "未调用"
            : timingText(unit.requestTiming, now)}</td>
        </tr>)}</tbody>
      </table></div>
      <small>服务调用包含网络传输和服务端处理，暂无法拆分模型与网络耗时。累计耗时不含暂停和应用关闭时间。</small>
    </details>
  </div>;
}

export function SummaryTimingView({ run }: { run: SummaryRun }) {
  const now = useNow(run.timing?.activeSinceUnixMs != null);
  return <div className="analysis-timing">
    <span>累计耗时 <strong>{timingText(run.timing, now)}</strong></span>
    <details className="technical-details"><summary>耗时明细</summary>
      <span>服务调用 · {run.reusedCache ? "复用缓存" : timingText(run.requestTiming, now)}</span>
      <small>包含网络传输和服务端处理，暂无法拆分模型与网络耗时。</small>
    </details>
  </div>;
}
