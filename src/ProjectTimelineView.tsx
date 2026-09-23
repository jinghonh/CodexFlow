import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Grain = "day" | "week" | "month";
type Quality = "complete" | "partial" | "unknown";
type TimeState = "complete" | "durationOnly" | "running" | "incomplete" | "invalid" | "unknown";
type Turn = {
  id: string; ordinal: number; sourceStatus: string; timeState: TimeState;
  startedAtUnixMs: number | null; completedAtUnixMs: number | null;
  intervalStartUnixMs: number | null; intervalEndUnixMs: number | null;
  durationMs: number | null; timeError: string | null;
};
type Thread = { threadId: string; title: string; turnsComplete: boolean; quality: Quality; knownDurationMs: number | null;
  lastActivityAtUnixMs: number | null; lastActivityBasis: "turnEnd" | "turnStart" | "metadataUpdate" | "unknown"; turns: Turn[] };
type Timeline = { projectId: string; threads: Thread[] };
type Tick = { at: number; label: string };
type Placed = { turn: Turn; lane: number };

const dayMs = 86_400_000;
const labelWidth = 238;
const qualityText: Record<Quality, string> = { complete: "完整", partial: "部分已知", unknown: "未知" };
const basisText: Record<Thread["lastActivityBasis"], string> = {
  turnEnd: "来源回合结束", turnStart: "来源回合开始；结束未知", metadataUpdate: "仅元数据更新时间，不能证明活动结束", unknown: "来源未提供",
};
const stateText: Record<TimeState, string> = {
  complete: "有来源起止时间", durationOnly: "仅有来源时长，无时间线位置", running: "执行中，尚无完整结束时间",
  incomplete: "时间不完整", invalid: "来源时间无效", unknown: "时间未知",
};

function turnStateText(turn: Turn): string {
  if (turn.timeState === "incomplete" && ["completed", "interrupted", "failed"].includes(turn.sourceStatus)) return "已结束，时间不完整";
  return stateText[turn.timeState];
}

function startOfUnit(value: number, grain: Grain): Date {
  const date = new Date(value);
  date.setHours(0, 0, 0, 0);
  if (grain === "week") date.setDate(date.getDate() - ((date.getDay() + 6) % 7));
  if (grain === "month") date.setDate(1);
  return date;
}

function nextUnit(date: Date, grain: Grain): Date {
  const next = new Date(date);
  if (grain === "day") next.setDate(next.getDate() + 1);
  if (grain === "week") next.setDate(next.getDate() + 7);
  if (grain === "month") next.setMonth(next.getMonth() + 1);
  return next;
}

function tickLabel(date: Date, grain: Grain): string {
  if (grain === "month") return `${date.getFullYear()}年${date.getMonth() + 1}月`;
  if (grain === "week") return `${date.getMonth() + 1}月${date.getDate()}日周`;
  return `${date.getMonth() + 1}月${date.getDate()}日`;
}

export function makeScale(threads: Thread[], grain: Grain, zoom: number): {
  start: number; end: number; width: number; ticks: Tick[];
} | null {
  let earliest = Infinity;
  let latest = -Infinity;
  for (const thread of threads) for (const turn of thread.turns) {
    if (turn.intervalStartUnixMs !== null && turn.intervalEndUnixMs !== null) {
      earliest = Math.min(earliest, turn.intervalStartUnixMs);
      latest = Math.max(latest, turn.intervalEndUnixMs);
    }
  }
  if (!Number.isFinite(earliest)) return null;
  const start = startOfUnit(earliest, grain).getTime();
  let endDate = startOfUnit(latest, grain);
  endDate = nextUnit(endDate, grain);
  const end = endDate.getTime();
  const pixelsPerDay = { day: 110, week: 25, month: 7.5 }[grain] * zoom;
  const width = Math.max(620, Math.ceil(((end - start) / dayMs) * pixelsPerDay));
  const ticks: Tick[] = [];
  for (let date = new Date(start); date.getTime() < end && ticks.length < 2000; date = nextUnit(date, grain)) {
    ticks.push({ at: date.getTime(), label: tickLabel(date, grain) });
  }
  return { start, end, width, ticks };
}

function placeTurns(turns: Turn[]): Placed[] {
  const lanes: number[] = [];
  return turns.filter((turn) => turn.intervalStartUnixMs !== null && turn.intervalEndUnixMs !== null)
    .sort((a, b) => a.intervalStartUnixMs! - b.intervalStartUnixMs! || a.ordinal - b.ordinal)
    .map((turn) => {
      const lane = lanes.findIndex((end) => end < turn.intervalStartUnixMs!);
      const index = lane < 0 ? lanes.length : lane;
      lanes[index] = turn.intervalEndUnixMs!;
      return { turn, lane: index };
    });
}

function preciseTime(value: number | null): string {
  return value === null ? "来源未提供" : new Date(value).toLocaleString("zh-CN", {
    year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit",
    second: "2-digit", fractionalSecondDigits: 3, timeZoneName: "shortOffset", hourCycle: "h23",
  });
}

function duration(value: number | null): string {
  if (value === null) return "来源未提供";
  if (value === 0) return "0 毫秒";
  if (value < 1000) return `${value} 毫秒`;
  const seconds = Math.floor(value / 1000);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return hours ? `${hours} 小时 ${minutes} 分钟` : minutes ? `${minutes} 分钟 ${seconds % 60} 秒` : `${seconds} 秒`;
}

export function ProjectTimelineView({ projectId, refreshVersion, connected, onSelectThread }: {
  projectId: string; refreshVersion: string; connected: boolean; onSelectThread: (threadId: string) => void;
}) {
  const [timeline, setTimeline] = useState<Timeline | null>(null);
  const [error, setError] = useState("");
  const [grain, setGrain] = useState<Grain>("day");
  const [zoom, setZoom] = useState(1);
  const [selected, setSelected] = useState<{ threadId: string; turnId: string | null } | null>(null);
  const [loading, setLoading] = useState<{ done: number; total: number } | null>(null);
  const [loadError, setLoadError] = useState("");
  const board = useRef<HTMLDivElement>(null);
  const zoomAnchor = useRef<number | null>(null);
  const stopLoading = useRef(false);

  useEffect(() => () => { stopLoading.current = true; }, [projectId]);

  useEffect(() => {
    let active = true;
    setTimeline(null);
    setError("");
    invoke<Timeline>("get_project_timeline", { projectId })
      .then((value) => { if (active) setTimeline(value); })
      .catch((caught) => { if (active) setError(typeof caught?.message === "string" ? caught.message : "读取项目时间线失败。"); });
    return () => { active = false; };
  }, [projectId, refreshVersion]);

  const scale = useMemo(() => makeScale(timeline?.threads ?? [], grain, zoom), [timeline, grain, zoom]);
  const rows = useMemo(() => (timeline?.threads ?? []).map((thread) => {
    const placed = placeTurns(thread.turns);
    const height = Math.max(72, 28 + (Math.max(0, ...placed.map(({ lane }) => lane)) + 1) * 20);
    return { thread, placed, height };
  }), [timeline]);
  const position = (at: number) => scale ? ((at - scale.start) / (scale.end - scale.start)) * scale.width : 0;
  const selectedThread = timeline?.threads.find((thread) => thread.threadId === selected?.threadId);
  const selectedTurn = selectedThread?.turns.find((turn) => turn.id === selected?.turnId);
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;

  useLayoutEffect(() => {
    if (zoomAnchor.current !== null && board.current && scale) {
      board.current.scrollLeft = zoomAnchor.current * scale.width - board.current.clientWidth / 2 + labelWidth;
      zoomAnchor.current = null;
    }
  }, [scale]);

  function changeView(nextGrain: Grain, nextZoom: number) {
    if (board.current && scale) {
      zoomAnchor.current = (board.current.scrollLeft + board.current.clientWidth / 2 - labelWidth) / scale.width;
    }
    setGrain(nextGrain);
    setZoom(nextZoom);
  }

  async function loadMissingTurns() {
    if (!timeline || loading) return;
    const missing = timeline.threads.filter((thread) => !thread.turnsComplete);
    stopLoading.current = false;
    setLoadError("");
    setLoading({ done: 0, total: missing.length });
    let errors = 0;
    for (const [index, thread] of missing.entries()) {
      if (stopLoading.current) break;
      try {
        await invoke("load_thread_history", { threadId: thread.threadId });
        const next = await invoke<Timeline>("get_project_timeline", { projectId });
        if (!stopLoading.current) setTimeline(next);
      } catch { errors += 1; }
      setLoading({ done: index + 1, total: missing.length });
    }
    setLoading(null);
    if (errors) setLoadError(`${errors} 条会话读取失败；已保留原有缓存，可再次尝试。`);
  }

  return <section id="project-timeline" className="panel timeline-panel" aria-label="项目活动时间线">
    <div className="timeline-heading"><div><div className="panel-kicker">04 / 真实活动时间线</div><h2>回合活动</h2>
      <p className="panel-intro">每段色条来自一个回合的有效起止时间。恢复间隔留白；重叠回合分层显示。</p></div>
      <div className="timeline-controls" aria-label="时间线视图控制">
        {connected && timeline?.threads.some((thread) => !thread.turnsComplete) && <button className="timeline-load" onClick={() => loading ? stopLoading.current = true : void loadMissingTurns()}>{loading ? `停止读取 · ${loading.done}/${loading.total}` : "读取缺失回合"}</button>}
        <div className="timeline-grains">{(["day", "week", "month"] as const).map((value) => <button key={value} className={grain === value ? "active" : ""} aria-pressed={grain === value} onClick={() => changeView(value, zoom)}>{ { day: "日", week: "周", month: "月" }[value] }</button>)}</div>
        <button aria-label="缩小时间线" disabled={zoom <= 0.5} onClick={() => changeView(grain, Math.max(0.5, zoom / 2))}>−</button><span>{Math.round(zoom * 100)}%</span><button aria-label="放大时间线" disabled={zoom >= 4} onClick={() => changeView(grain, Math.min(4, zoom * 2))}>＋</button>
      </div></div>
    <div className="timeline-legend"><strong>未分组会话</strong><span>{timeline?.threads.length ?? 0} 条</span><span>系统时区：{zone}</span><span>横向滚动查看更早或更晚的活动</span></div>
    {error && <div className="page-error" role="alert">{error}</div>}
    {loadError && <div className="page-error" role="alert">{loadError}</div>}
    {!timeline && !error && <p className="empty-list">正在读取已缓存回合…</p>}
    {timeline && timeline.threads.length === 0 && <p className="empty-list">此项目暂无会话。</p>}
    {timeline && timeline.threads.length > 0 && <>
      {!scale && <p className="timeline-empty">暂无可定位的活动区间；仍可查看已知时长和时间状态。打开会话历史后，时间线会更新。</p>}
      <div ref={board} className="timeline-board" role="region" aria-label="可横向滚动的活动时间线" tabIndex={0}>
        <div className="timeline-content" style={{ width: labelWidth + (scale?.width ?? 620) }}>
          <div className="timeline-axis" style={{ gridTemplateColumns: `${labelWidth}px ${scale?.width ?? 620}px` }}><div className="timeline-axis-label">会话 · 已知时长</div><div className="timeline-axis-track">
            {scale?.ticks.map((tick) => <span key={tick.at} className="timeline-tick" style={{ left: position(tick.at) }}>{tick.label}</span>)}
            {!scale && <span className="timeline-axis-unknown">位置未知</span>}
          </div></div>
          {rows.map(({ thread, placed, height }) => <div className="timeline-row" key={thread.threadId} style={{ gridTemplateColumns: `${labelWidth}px ${scale?.width ?? 620}px`, minHeight: height }}>
            <button className={`timeline-row-label ${selected?.threadId === thread.threadId ? "selected" : ""}`} onClick={() => setSelected({ threadId: thread.threadId, turnId: null })} title={thread.title}>
              <strong>{thread.title}</strong><span className={`timeline-quality quality-${thread.quality}`}>{qualityText[thread.quality]}</span><small title={thread.knownDurationMs === null ? "来源未提供可累计时长" : `${thread.knownDurationMs.toLocaleString("zh-CN")} 毫秒`}>{thread.knownDurationMs === null ? "已知时长：未知" : `已知时长：${duration(thread.knownDurationMs)}`}</small>
            </button>
            <div className="timeline-track" style={{ height }}>
              {placed.map(({ turn, lane }) => {
                const start = position(turn.intervalStartUnixMs!);
                const width = Math.max(3, position(turn.intervalEndUnixMs!) - start);
                return <button key={turn.id} className={`timeline-segment ${turn.intervalStartUnixMs === turn.intervalEndUnixMs ? "instant" : ""} ${selected?.threadId === thread.threadId && selected.turnId === turn.id ? "selected" : ""}`}
                  style={{ left: start, width, top: 18 + lane * 20 }} aria-label={`${thread.title} 第 ${turn.ordinal + 1} 回合，${preciseTime(turn.intervalStartUnixMs)} 至 ${preciseTime(turn.intervalEndUnixMs)}`}
                  title={`${preciseTime(turn.intervalStartUnixMs)} → ${preciseTime(turn.intervalEndUnixMs)}`} onClick={() => setSelected({ threadId: thread.threadId, turnId: turn.id })} />;
              })}
            </div>
          </div>)}
        </div>
      </div>
      <div className="timeline-details" aria-live="polite">
        {selectedThread ? <><div><strong>{selectedThread.title}</strong><code>{selectedThread.threadId}</code><span>时间状态：{qualityText[selectedThread.quality]}；回合记录{selectedThread.turnsComplete ? "完整" : "未完整取得"}</span></div>
          {selectedThread.turns.length > 0 && <div className="timeline-turn-picker" aria-label="选择回合">{selectedThread.turns.map((turn) => <button key={turn.id} className={selectedTurn?.id === turn.id ? "active" : ""} onClick={() => setSelected({ threadId: selectedThread.threadId, turnId: turn.id })} title={turnStateText(turn)}>#{turn.ordinal + 1} {turn.timeState === "complete" ? "区间" : turnStateText(turn)}</button>)}</div>}
          <dl className="timeline-last-activity"><dt>最后时间</dt><dd>{preciseTime(selectedThread.lastActivityAtUnixMs)} · {basisText[selectedThread.lastActivityBasis]}</dd></dl>
          {selectedTurn ? <dl><dt>回合</dt><dd>{selectedTurn.ordinal + 1} · <code>{selectedTurn.id}</code> · {turnStateText(selectedTurn)}（{selectedTurn.sourceStatus}）</dd>
            <dt>开始</dt><dd>{preciseTime(selectedTurn.startedAtUnixMs)}{selectedTurn.startedAtUnixMs !== null && <small> UTC {new Date(selectedTurn.startedAtUnixMs).toISOString()}</small>}</dd>
            <dt>结束</dt><dd>{preciseTime(selectedTurn.completedAtUnixMs)}{selectedTurn.completedAtUnixMs !== null && <small> UTC {new Date(selectedTurn.completedAtUnixMs).toISOString()}</small>}</dd>
            <dt>来源时长</dt><dd>{duration(selectedTurn.durationMs)}{selectedTurn.durationMs !== null && selectedTurn.durationMs >= 1000 && <small>{selectedTurn.durationMs.toLocaleString("zh-CN")} 毫秒</small>}</dd>{selectedTurn.timeError && <><dt>异常</dt><dd>{selectedTurn.timeError}</dd></>}</dl>
            : <p>{selectedThread.turns.length ? "选择回合查看精确来源时间与异常信息。" : "尚无已缓存回合。"}</p>}
          <button className="browse-button timeline-open" onClick={() => onSelectThread(selectedThread.threadId)}>打开会话历史</button></>
          : <p>选择会话或活动段，查看本地时间、UTC 来源时间和缺失原因。</p>}
      </div>
      <p className="timeline-note">已知时长只累计来源提供的有效毫秒时长，不用起止差值补算。回合经过时间不等于用户工时，也不等于纯模型计算时间。只有列表更新时间时，它只是元数据更新时间，不能证明活动结束。</p>
    </>}
  </section>;
}
