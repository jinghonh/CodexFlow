import type { Progress } from "./analysisProgress";

export function ProgressMeter({ progress, label, totalMayChange = false }: { progress: Progress; label: string; totalMayChange?: boolean }) {
  const { completed, total, unit } = progress;
  const count = total === null ? `已处理 ${completed} ${unit}` : `已处理 ${completed} / ${total} ${unit}`;
  return <div className="model-progress">
    <span>{progress.stage} · {count}{total === null ? " · 正在运行" : totalMayChange ? " · 总数可能变化" : ""}</span>
    <div className={`model-progress-track ${total === null ? "indeterminate" : ""}`}
      role="progressbar" aria-label={label} aria-valuemin={0}
      aria-valuenow={total === null ? undefined : completed}
      aria-valuemax={total === null ? undefined : Math.max(total, 1)}
      aria-valuetext={`${progress.stage}，${count}`}>
      <span style={total === null ? undefined : { width: `${total === 0 ? 100 : Math.min(100, completed / total * 100)}%` }} />
    </div>
  </div>;
}
