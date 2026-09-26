import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";

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
type Period = { start: number; end: number };
type Placed = { turn: Turn; lane: number; start: number; end: number };

const dayMs = 86_400_000;
const hourMs = 3_600_000;
const labelWidth = 238;
const weekDays = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
const qualityText: Record<Quality, string> = { complete: "完整", partial: "部分已知", unknown: "未知" };
const basisText: Record<Thread["lastActivityBasis"], string> = {
  turnEnd: "来源回合结束", turnStart: "来源回合开始；不据此推断活动结束", metadataUpdate: "仅元数据更新时间，不能证明活动结束", unknown: "来源未提供",
};
const stateText: Record<TimeState, string> = {
  complete: "有来源起止时间", durationOnly: "仅有来源时长，无时间线位置", running: "执行中，尚无完整结束时间",
  incomplete: "时间不完整", invalid: "来源时间无效", unknown: "时间未知",
};

function turnStateText(turn: Turn): string {
  if (turn.timeState === "incomplete" && ["completed", "interrupted", "failed"].includes(turn.sourceStatus)) return "已结束，时间不完整";
  return stateText[turn.timeState];
}

function startOfDay(value: Date | number): Date {
  const date = new Date(value instanceof Date ? value.getTime() : value);
  date.setHours(0, 0, 0, 0);
  return date;
}

function startOfUnit(value: Date | number, grain: Grain): Date {
  const date = startOfDay(value);
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

function periodFor(date: Date | number, grain: Grain): Period {
  const start = startOfUnit(date, grain);
  return { start: start.getTime(), end: nextUnit(start, grain).getTime() };
}

function moveDate(date: Date, grain: Grain, amount: number): Date {
  const moved = new Date(date);
  if (grain === "day") moved.setDate(moved.getDate() + amount);
  if (grain === "week") moved.setDate(moved.getDate() + amount * 7);
  if (grain === "month") {
    const originalDay = moved.getDate();
    moved.setDate(1);
    moved.setMonth(moved.getMonth() + amount);
    const lastDay = new Date(moved.getFullYear(), moved.getMonth() + 1, 0).getDate();
    moved.setDate(Math.min(originalDay, lastDay));
  }
  return moved;
}

function isSameDay(left: Date | number, right: Date | number): boolean {
  return startOfDay(left).getTime() === startOfDay(right).getTime();
}

function formatFullDate(value: Date | number): string {
  const date = new Date(value instanceof Date ? value.getTime() : value);
  return `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日 ${weekDays[date.getDay()]}`;
}

function formatPeriodLabel(date: Date, grain: Grain, now: number): string {
  if (grain === "day") {
    if (isSameDay(date, now)) return "今天";
    const yesterday = new Date(now);
    yesterday.setDate(yesterday.getDate() - 1);
    if (isSameDay(date, yesterday)) return "昨天";
    const dayBefore = new Date(now);
    dayBefore.setDate(dayBefore.getDate() - 2);
    return isSameDay(date, dayBefore) ? "前天" : formatFullDate(date);
  }
  if (grain === "month") return `${date.getFullYear()}年${date.getMonth() + 1}月`;
  const period = periodFor(date, "week");
  const start = new Date(period.start);
  const end = new Date(period.end - 1);
  const startText = `${start.getFullYear()}年${start.getMonth() + 1}月${start.getDate()}日`;
  const endText = start.getFullYear() === end.getFullYear()
    ? `${end.getMonth() + 1}月${end.getDate()}日`
    : `${end.getFullYear()}年${end.getMonth() + 1}月${end.getDate()}日`;
  return `${startText}—${endText}`;
}

function calendarDates(month: Date): Date[] {
  const first = startOfUnit(month, "month");
  first.setDate(first.getDate() - ((first.getDay() + 6) % 7));
  return Array.from({ length: 42 }, (_, index) => {
    const date = new Date(first);
    date.setDate(date.getDate() + index);
    return date;
  });
}

function activityInterval(turn: Turn, now: number): { start: number; end: number } | null {
  if (turn.intervalStartUnixMs !== null && turn.intervalEndUnixMs !== null) {
    return turn.intervalEndUnixMs >= turn.intervalStartUnixMs
      ? { start: turn.intervalStartUnixMs, end: turn.intervalEndUnixMs }
      : null;
  }
  if (turn.timeState !== "running" || turn.startedAtUnixMs === null || turn.startedAtUnixMs > now) return null;
  return { start: turn.startedAtUnixMs, end: now };
}

function overlaps(interval: { start: number; end: number }, period: Period): boolean {
  return interval.start === interval.end
    ? interval.start >= period.start && interval.start < period.end
    : interval.start < period.end && interval.end > period.start;
}

function hasActivityOnDate(threads: Thread[], date: Date, now: number): boolean {
  const day = periodFor(date, "day");
  return threads.some((thread) => thread.turns.some((turn) => {
    const interval = activityInterval(turn, now);
    return interval !== null && overlaps(interval, day);
  }));
}

function hourLabel(value: number): string {
  const date = new Date(value);
  return `${String(date.getHours()).padStart(2, "0")}:00`;
}

function periodTicks(start: number, end: number, grain: Grain): Tick[] {
  const ticks: Tick[] = [];
  if (grain === "day") {
    for (let at = start; at <= end && ticks.length < 30; at += hourMs) {
      const endOfDay = at === end && !isSameDay(at, start);
      ticks.push({ at, label: endOfDay ? "24:00" : hourLabel(at) });
    }
    const labels = new Map<string, number>();
    for (const tick of ticks) labels.set(tick.label, (labels.get(tick.label) ?? 0) + 1);
    return ticks.map((tick) => {
      if ((labels.get(tick.label) ?? 0) < 2) return tick;
      const offset = new Intl.DateTimeFormat("zh-CN", { timeZoneName: "shortOffset" })
        .formatToParts(new Date(tick.at)).find((part) => part.type === "timeZoneName")?.value;
      return { ...tick, label: `${tick.label} ${offset ?? ""}`.trim() };
    });
  }
  for (let date = new Date(start); date.getTime() < end && ticks.length < 40; date.setDate(date.getDate() + 1)) {
    const label = grain === "week"
      ? `${date.getMonth() + 1}月${date.getDate()}日 ${weekDays[date.getDay()]}`
      : `${date.getDate()}日`;
    ticks.push({ at: date.getTime(), label });
  }
  return ticks;
}

function hourFloor(value: number): number {
  const date = new Date(value);
  date.setMinutes(0, 0, 0);
  return date.getTime();
}

function hourCeil(value: number): number {
  const date = new Date(value);
  const floor = hourFloor(value);
  if (floor === value) return value;
  date.setTime(floor);
  date.setHours(date.getHours() + 1);
  return date.getTime();
}

function makePeriodScale(threads: Thread[], grain: Grain, zoom: number, selectedDate: Date, now: number) {
  const period = periodFor(selectedDate, grain);
  let start = period.start;
  let end = period.end;
  if (grain === "day") {
    const intervals = threads.flatMap((thread) => thread.turns.flatMap((turn) => {
      const interval = activityInterval(turn, now);
      if (interval === null || !overlaps(interval, period)) return [];
      return [{ start: Math.max(interval.start, period.start), end: Math.min(interval.end, period.end) }];
    }));
    if (intervals.length) {
      const earliest = intervals.reduce((value, interval) => Math.min(value, interval.start), Infinity);
      const latest = intervals.reduce((value, interval) => Math.max(value, interval.end), -Infinity);
      start = Math.max(period.start, hourFloor(earliest));
      end = Math.min(period.end, hourCeil(latest));
      if (end <= start) end = Math.min(period.end, hourCeil(start + 1));
      if (isSameDay(selectedDate, now)) end = Math.min(period.end, Math.max(end, hourCeil(now)));
      if (end <= start) { start = period.start; end = period.end; }
    }
  }
  const spanDays = (end - start) / dayMs;
  const pixelsPerDay = grain === "week" ? 118 : 32;
  const naturalWidth = grain === "day" ? ((end - start) / hourMs) * 36 : spanDays * pixelsPerDay;
  const width = Math.max(620, Math.ceil(naturalWidth * zoom));
  return { start, end, width, ticks: periodTicks(start, end, grain) };
}

export function makeScale(threads: Thread[], grain: Grain, zoom: number, selectedDate?: Date, now = Date.now()): {
  start: number; end: number; width: number; ticks: Tick[];
} | null {
  if (selectedDate) return makePeriodScale(threads, grain, zoom, selectedDate, now);
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
    const label = grain === "month" ? `${date.getFullYear()}年${date.getMonth() + 1}月`
      : grain === "week" ? `${date.getMonth() + 1}月${date.getDate()}日周`
        : `${date.getMonth() + 1}月${date.getDate()}日`;
    ticks.push({ at: date.getTime(), label });
  }
  return { start, end, width, ticks };
}

function placeTurns(turns: Turn[], period: Period, now: number): Placed[] {
  const lanes: number[] = [];
  return turns.flatMap((turn) => {
    const interval = activityInterval(turn, now);
    if (interval === null || !overlaps(interval, period)) return [];
    return [{ turn, start: Math.max(interval.start, period.start), end: Math.min(interval.end, period.end) }];
  }).sort((a, b) => a.start - b.start || a.turn.ordinal - b.turn.ordinal)
    .map(({ turn, start, end }) => {
      const lane = lanes.findIndex((laneEnd) => laneEnd < start);
      const index = lane < 0 ? lanes.length : lane;
      lanes[index] = end;
      return { turn, lane: index, start, end };
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

export function ProjectTimelineView({ projectId, refreshVersion, connected, onSelectThread, selectedThreadId = null, visibleThreadIds, workstreams = [], onHistoryLoaded }: {
  projectId: string; refreshVersion: string; connected: boolean; onSelectThread: (threadId: string) => void;
  selectedThreadId?: string | null; visibleThreadIds?: Set<string>; workstreams?: { id: string; name: string; members: string[] }[];
  onHistoryLoaded?: () => void;
}) {
  const loadedProjectId = useRef(projectId);
  const [timeline, setTimeline] = useState<Timeline | null>(null);
  const [error, setError] = useState("");
  const [grain, setGrain] = useState<Grain>("day");
  const [selectedDate, setSelectedDate] = useState(() => startOfDay(new Date()));
  const [now, setNow] = useState(() => Date.now());
  const [calendarOpen, setCalendarOpen] = useState(false);
  const [calendarMonth, setCalendarMonth] = useState(() => startOfUnit(new Date(), "month"));
  const [zoom, setZoom] = useState(1);
  const [rowLimit, setRowLimit] = useState(40);
  const [selected, setSelected] = useState<{ threadId: string; turnId: string | null } | null>(null);
  const [loading, setLoading] = useState<{ done: number; total: number } | null>(null);
  const [loadError, setLoadError] = useState("");
  const board = useRef<HTMLDivElement>(null);
  const dateControl = useRef<HTMLDivElement>(null);
  const dateButton = useRef<HTMLButtonElement>(null);
  const zoomAnchor = useRef<number | null>(null);
  const stopLoading = useRef(false);

  useEffect(() => () => { stopLoading.current = true; }, [projectId]);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!calendarOpen) setCalendarMonth(startOfUnit(selectedDate, "month"));
  }, [calendarOpen, selectedDate.getTime()]);

  useEffect(() => {
    if (!calendarOpen) return;
    const dismissOutside = (event: PointerEvent) => {
      if (!dateControl.current?.contains(event.target as Node)) setCalendarOpen(false);
    };
    const dismissEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setCalendarOpen(false);
        dateButton.current?.focus();
      }
    };
    document.addEventListener("pointerdown", dismissOutside);
    document.addEventListener("keydown", dismissEscape);
    return () => {
      document.removeEventListener("pointerdown", dismissOutside);
      document.removeEventListener("keydown", dismissEscape);
    };
  }, [calendarOpen]);

  useEffect(() => {
    let active = true;
    if (loadedProjectId.current !== projectId) {
      loadedProjectId.current = projectId;
      setTimeline(null);
    }
    setError("");
    invoke<Timeline>("get_project_timeline", { projectId })
      .then((value) => { if (active) setTimeline(value); })
      .catch((caught) => { if (active) setError(formatAppError(caught, "读取项目时间线失败。")); });
    return () => { active = false; };
  }, [projectId, refreshVersion]);

  const visible = useMemo(() => (timeline?.threads ?? []).filter((thread) => !visibleThreadIds || visibleThreadIds.has(thread.threadId)), [timeline, visibleThreadIds]);
  const owners = useMemo(() => new Map(workstreams.flatMap((stream) => stream.members.map((id) => [id, stream.id] as const))), [workstreams]);
  const period = useMemo(() => periodFor(selectedDate, grain), [selectedDate.getTime(), grain]);
  const rows = useMemo(() => visible.flatMap((thread) => {
    const placed = placeTurns(thread.turns, period, now);
    if (!placed.length) return [];
    const height = Math.max(72, 28 + (Math.max(0, ...placed.map(({ lane }) => lane)) + 1) * 20);
    return [{ thread, placed, height, group: owners.get(thread.threadId) ?? "ungrouped" }];
  }).sort((a, b) => {
    const left = owners.get(a.thread.threadId) ?? "~";
    const right = owners.get(b.thread.threadId) ?? "~";
    return left.localeCompare(right) || (a.thread.lastActivityAtUnixMs ?? 0) - (b.thread.lastActivityAtUnixMs ?? 0) || a.thread.threadId.localeCompare(b.thread.threadId);
  }), [visible, owners, period.start, period.end, now]);
  const scale = useMemo(() => makeScale(visible, grain, zoom, selectedDate, now)!, [visible, grain, zoom, selectedDate.getTime(), now]);
  const position = (at: number) => ((at - scale.start) / (scale.end - scale.start)) * scale.width;
  const isToday = isSameDay(selectedDate, now);
  const incompleteCount = visible.filter((thread) => !thread.turnsComplete).length;
  const canMoveForward = periodFor(moveDate(selectedDate, grain, 1), grain).start <= periodFor(now, grain).start;
  const calendarMonthStart = startOfUnit(calendarMonth, "month");
  const canAdvanceCalendarMonth = calendarMonthStart.getTime() < startOfUnit(now, "month").getTime();
  const dateLabel = formatPeriodLabel(selectedDate, grain, now);
  const dateUnit = { day: "日", week: "周", month: "月" }[grain];
  const ticks = scale.ticks;
  useEffect(() => { setRowLimit(40); }, [projectId, visibleThreadIds, selectedDate.getTime(), grain]);
  useEffect(() => {
    if (!selectedThreadId) return;
    const index = rows.findIndex((row) => row.thread.threadId === selectedThreadId);
    if (index >= 0) setRowLimit((previous) => Math.max(previous, index + 1));
  }, [rows, selectedThreadId]);
  const shownRows = rows.slice(0, rowLimit);
  const effectiveSelectedId = selectedThreadId ?? selected?.threadId;
  const selectedThread = rows.find((row) => row.thread.threadId === effectiveSelectedId)?.thread;
  const selectedTurn = selectedThread && selectedThread.threadId === selected?.threadId ? selectedThread.turns.find((turn) => turn.id === selected?.turnId) : undefined;
  const choose = (threadId: string, turnId: string | null) => { setSelected({ threadId, turnId }); onSelectThread(threadId); };
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const showNow = grain === "day" && isToday;
  const calendarDays = calendarDates(calendarMonth);

  useLayoutEffect(() => {
    if (zoomAnchor.current !== null && board.current) {
      board.current.scrollLeft = zoomAnchor.current * scale.width - board.current.clientWidth / 2 + labelWidth;
      zoomAnchor.current = null;
    }
  }, [scale]);

  useLayoutEffect(() => {
    if (board.current) board.current.scrollLeft = 0;
  }, [selectedDate.getTime(), grain]);

  function changeView(nextGrain: Grain, nextZoom: number) {
    if (board.current && nextGrain === grain && nextZoom !== zoom && scale.width > 0) {
      zoomAnchor.current = (board.current.scrollLeft + board.current.clientWidth / 2 - labelWidth) / scale.width;
    }
    setGrain(nextGrain);
    setZoom(nextZoom);
  }

  function chooseDate(date: Date) {
    setSelectedDate(startOfDay(date));
    setCalendarOpen(false);
  }

  function movePeriod(amount: number) {
    const moved = moveDate(selectedDate, grain, amount);
    if (amount < 0 || periodFor(moved, grain).start <= periodFor(now, grain).start) {
      setSelectedDate(startOfDay(moved));
      setCalendarOpen(false);
    }
  }

  async function loadMissingTurns() {
    if (!timeline || loading) return;
    const missing = visible.filter((thread) => !thread.turnsComplete);
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
    onHistoryLoaded?.();
  }

  return <section id="project-timeline" className="panel timeline-panel" aria-label="项目活动时间线">
    <div className="timeline-heading"><div><div className="panel-kicker">探索 / 活动时间线</div><h2>回合活动</h2>
      <p className="panel-intro">每段色条来自一个回合的活动区间。恢复间隔留白；跨日活动在本地午夜处分段。</p></div>
      <div className="timeline-controls" aria-label="时间线视图控制">
        {connected && incompleteCount > 0 && <button className="timeline-load" onClick={() => loading ? stopLoading.current = true : void loadMissingTurns()}>{loading ? `停止读取 · ${loading.done}/${loading.total}` : "读取缺失回合"}</button>}
        <div className="timeline-grains">{(["day", "week", "month"] as const).map((value) => <button key={value} className={grain === value ? "active" : ""} aria-pressed={grain === value} onClick={() => changeView(value, zoom)}>{ { day: "日", week: "周", month: "月" }[value] }</button>)}</div>
        <button aria-label="缩小时间线" disabled={zoom <= 0.5} onClick={() => changeView(grain, Math.max(0.5, zoom / 2))}>−</button><span>{Math.round(zoom * 100)}%</span><button aria-label="放大时间线" disabled={zoom >= 4} onClick={() => changeView(grain, Math.min(4, zoom * 2))}>＋</button>
      </div></div>
    <div className="timeline-date-row">
      <div className="timeline-date-control" ref={dateControl}>
        <button type="button" className="timeline-date-arrow" aria-label={`上一${dateUnit}`} onClick={() => movePeriod(-1)}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m15 5-7 7 7 7" /></svg>
        </button>
        <button type="button" ref={dateButton} className="timeline-date-label" aria-haspopup="dialog" aria-expanded={calendarOpen}
          aria-controls="timeline-date-calendar" onClick={() => { setCalendarMonth(startOfUnit(selectedDate, "month")); setCalendarOpen((open) => !open); }}>
          <span>{dateLabel}</span><svg className="timeline-date-caret" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6" /></svg>
        </button>
        <button type="button" className="timeline-date-arrow" aria-label={`下一${dateUnit}`} disabled={!canMoveForward} onClick={() => movePeriod(1)}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 5 7 7-7 7" /></svg>
        </button>
        {calendarOpen && <div id="timeline-date-calendar" className="timeline-calendar" role="dialog" aria-label="选择日期">
          <div className="timeline-calendar-heading">
            <button type="button" aria-label="上个月" onClick={() => setCalendarMonth((month) => moveDate(month, "month", -1))}>
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m15 5-7 7 7 7" /></svg>
            </button>
            <strong>{calendarMonthStart.getFullYear()}年{calendarMonthStart.getMonth() + 1}月</strong>
            <button type="button" aria-label="下个月" disabled={!canAdvanceCalendarMonth} onClick={() => setCalendarMonth((month) => moveDate(month, "month", 1))}>
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 5 7 7-7 7" /></svg>
            </button>
          </div>
          <div className="timeline-calendar-weekdays" aria-hidden="true">{["一", "二", "三", "四", "五", "六", "日"].map((day) => <span key={day}>{day}</span>)}</div>
          <div className="timeline-calendar-days" aria-label="日历日期">
            {calendarDays.map((date) => {
              const outsideMonth = date.getMonth() !== calendarMonthStart.getMonth();
              const future = startOfDay(date).getTime() > startOfDay(now).getTime();
              const activity = hasActivityOnDate(visible, date, now);
              const selectedDay = isSameDay(date, selectedDate);
              const today = isSameDay(date, now);
              const label = `${formatFullDate(date)}${activity ? "，有回合活动" : ""}${future ? "，不可选择未来日期" : ""}`;
              return <button type="button" key={date.getTime()} className={`timeline-calendar-day ${outsideMonth ? "outside-month" : ""} ${selectedDay ? "selected" : ""} ${today ? "today" : ""}`}
                aria-label={label} aria-pressed={selectedDay} aria-current={today ? "date" : undefined} disabled={future} onClick={() => chooseDate(date)}>
                <span>{date.getDate()}</span>{activity && <i className="timeline-calendar-dot" aria-hidden="true" />}
              </button>;
            })}
          </div>
          <p className="timeline-calendar-note">小点表示已读取到回合活动</p>
        </div>}
      </div>
    </div>
    <div className="timeline-legend"><strong>工作流泳道</strong><span>显示 {rows.length} / {visible.length} 条会话（所选范围内）；每条会话只占一个主要工作流</span><span>系统时区：{zone}</span>
      {incompleteCount > 0 && <span className="timeline-incomplete-hint">{incompleteCount} 条会话历史未完整；活动条和日历小点依据已读取回合</span>}
      <span>横向滚动查看所选日／周／月</span></div>
    {error && <div className="page-error" role="alert">{error}</div>}
    {loadError && <div className="page-error" role="alert">{loadError}</div>}
    {!timeline && !error && <p className="empty-list">正在读取已缓存回合…</p>}
    {timeline && <div ref={board} className="timeline-board" role="region" aria-label="可横向滚动的活动时间线" tabIndex={0}>
      <div className="timeline-content" style={{ width: labelWidth + scale.width }}>
        <div className="timeline-axis" style={{ gridTemplateColumns: `${labelWidth}px ${scale.width}px` }}>
          <div className="timeline-axis-label">会话 · 全历史已知时长</div>
          <div className="timeline-axis-track">
            {ticks.map((tick) => <span key={tick.at} className="timeline-tick" style={{ left: position(tick.at) }}>{tick.label}</span>)}
            {showNow && <i className="timeline-now-line" style={{ left: position(now) }} title="当前时刻" aria-label="当前时刻" />}
          </div>
        </div>
        {shownRows.map(({ thread, placed, height, group }, index) => <Fragment key={thread.threadId}>
          {(index === 0 || shownRows[index - 1].group !== group) && <div className="timeline-group" style={{ gridTemplateColumns: `${labelWidth}px ${scale.width}px` }}>
            <strong>{workstreams.find((stream) => stream.id === group)?.name ?? "未分组会话"}</strong><span>{rows.filter((row) => row.group === group).length} 条会话</span>
          </div>}
          <div className="timeline-row" style={{ gridTemplateColumns: `${labelWidth}px ${scale.width}px`, minHeight: height }}>
            <button className={`timeline-row-label ${effectiveSelectedId === thread.threadId ? "selected" : ""}`} onClick={() => choose(thread.threadId, null)} title={thread.title}>
              <strong>{thread.title}</strong><span className={`timeline-quality quality-${thread.quality}`}>{qualityText[thread.quality]}</span><small title={thread.knownDurationMs === null ? "来源未提供可累计时长" : `${thread.knownDurationMs.toLocaleString("zh-CN")} 毫秒`}>{thread.knownDurationMs === null ? "全历史已知时长：未知" : `全历史已知时长：${duration(thread.knownDurationMs)}`}</small>
            </button>
            <div className="timeline-track" style={{ height }}>
              {ticks.filter((tick) => tick.at < scale.end).map((tick) => <i key={tick.at} className="timeline-gridline" style={{ left: position(tick.at) }} aria-hidden="true" />)}
              {showNow && <i className="timeline-now-line" style={{ left: position(now) }} title="当前时刻" aria-hidden="true" />}
              {placed.map(({ turn, lane, start: intervalStart, end: intervalEnd }) => {
                const start = position(intervalStart);
                const width = Math.max(3, position(intervalEnd) - start);
                const running = turn.timeState === "running" && turn.intervalEndUnixMs === null;
                const endLabel = running ? "当前时间（执行中）" : preciseTime(intervalEnd);
                return <button key={turn.id} className={`timeline-segment ${running ? "running" : ""} ${effectiveSelectedId === thread.threadId && selected?.threadId === thread.threadId && selected.turnId === turn.id ? "selected" : ""} ${intervalStart === intervalEnd ? "instant" : ""}`}
                  style={{ left: start, width, top: 18 + lane * 20 }} aria-label={`${thread.title} 第 ${turn.ordinal + 1} 回合，${preciseTime(intervalStart)} 至 ${endLabel}`}
                  title={`${preciseTime(intervalStart)} → ${endLabel}`} onClick={() => choose(thread.threadId, turn.id)} />;
              })}
            </div>
          </div>
        </Fragment>)}
        {shownRows.length < rows.length && <button className="timeline-load" onClick={() => setRowLimit((previous) => previous + 40)}>显示更多会话（已显示 {shownRows.length} / {rows.length}）</button>}
      </div>
    </div>}
    {timeline && rows.length > 0 && <>
      <div className="timeline-details" aria-live="polite">
        {selectedThread ? <><div><strong>{selectedThread.title}</strong><code>{selectedThread.threadId}</code><span>时间状态：{qualityText[selectedThread.quality]}；回合记录{selectedThread.turnsComplete ? "完整" : "未完整取得"}</span></div>
          {selectedThread.turns.length > 0 && <div className="timeline-turn-picker" aria-label="选择回合">{selectedThread.turns.map((turn) => <button key={turn.id} className={selectedTurn?.id === turn.id ? "active" : ""} onClick={() => choose(selectedThread.threadId, turn.id)} title={turnStateText(turn)}>#{turn.ordinal + 1} {turn.timeState === "complete" ? "区间" : turnStateText(turn)}</button>)}</div>}
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
