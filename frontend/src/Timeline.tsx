import { FormEvent, useEffect, useMemo, useState } from "react";

import type { TimelineOptions } from "./api";
import type {
  Conversation,
  ObservationRange,
  TimelineGranularity,
  TimelineSnapshot,
} from "./types";

interface TimelineViewProps {
  timeline: TimelineSnapshot | null | undefined;
  conversations: Conversation[];
  selectedId: string | null;
  loading: boolean;
  error: string | null;
  onSelect: (id: string) => void;
  onOptionsChange: (options: TimelineOptions) => Promise<void>;
}

const GRANULARITIES: TimelineGranularity[] = ["day", "week", "month"];

export function TimelineView({
  timeline,
  conversations,
  selectedId,
  loading,
  error,
  onSelect,
  onOptionsChange,
}: TimelineViewProps) {
  const [granularity, setGranularity] = useState<TimelineGranularity>(
    timeline?.granularity ?? "day",
  );
  const [timezone, setTimezone] = useState(timeline?.timezone ?? "UTC");
  const [draftTimezone, setDraftTimezone] = useState(timezone);
  const [windowStart, setWindowStart] = useState(0);
  const [windowSize, setWindowSize] = useState(7);
  const [localError, setLocalError] = useState<string | null>(null);

  useEffect(() => {
    if (!timeline) return;
    setGranularity(timeline.granularity);
    setTimezone(timeline.timezone);
    setDraftTimezone(timeline.timezone);
  }, [timeline?.granularity, timeline?.timezone]);

  useEffect(() => {
    if (!timeline) return;
    setWindowStart(Math.max(0, timeline.buckets.length - windowSize));
  }, [timeline?.granularity, timeline?.buckets.length, timeline?.buckets.at(-1)?.start]);

  useEffect(() => {
    if (!timeline || !selectedId) return;
    const range = timeline.ranges.find((item) => item.conversationId === selectedId);
    if (!range) return;
    const last = lastIntersectingBucketIndex(range, timeline);
    if (last >= 0) {
      setWindowStart(Math.max(0, Math.min(timeline.buckets.length - windowSize, last - Math.floor(windowSize / 2))));
    }
  }, [selectedId, timeline?.granularity, timeline?.buckets.length, timeline?.buckets.at(-1)?.start]);

  const conversationById = useMemo(
    () => new Map(conversations.map((conversation) => [conversation.id, conversation])),
    [conversations],
  );

  async function changeGranularity(next: TimelineGranularity) {
    if (next === granularity) return;
    setLocalError(null);
    try {
      await onOptionsChange({ granularity: next, timezone });
      setGranularity(next);
    } catch {
      // The parent retains the last complete snapshot and displays the error.
    }
  }

  async function submitTimezone(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const nextTimezone = draftTimezone.trim();
    if (!nextTimezone) {
      setLocalError("请输入有效时区，例如 UTC 或 Asia/Shanghai。");
      return;
    }
    setLocalError(null);
    try {
      await onOptionsChange({ granularity, timezone: nextTimezone });
      setTimezone(nextTimezone);
    } catch {
      // Keep the draft so the user can correct or retry it.
    }
  }

  const activeError = localError ?? error;

  if (!timeline) {
    return (
      <section className="timeline-section" aria-label="任务时间线">
        <div className="timeline-heading">
          <div>
            <p className="section-kicker">创建至最近更新</p>
            <h2 id="timeline-title">时间线</h2>
          </div>
        </div>
        <div className="timeline-empty">当前来源快照没有时间线数据。</div>
      </section>
    );
  }

  const maxWindowStart = Math.max(0, timeline.buckets.length - windowSize);
  const offset = Math.min(windowStart, maxWindowStart);
  const displayed = { ...timeline, buckets: timeline.buckets.slice(offset, offset + windowSize) };
  const validRanges = timeline.ranges.filter((range) => {
    const conversation = conversationById.get(range.conversationId);
    return range.valid && conversation !== undefined && !conversation.derived.missing;
  });
  const visibleWarnings = timeline.warnings.filter((warning) =>
    conversationById.has(warning.conversationId),
  );
  const visibleBuckets = displayed.buckets.map((bucket) => ({
    ...bucket,
    overlapCount: countVisibleOverlaps(bucket, timeline.ranges, conversationById),
  }));
  const gridTemplateColumns = `230px repeat(${displayed.buckets.length}, minmax(100px, 1fr))`;
  const trackTemplateColumns = `repeat(${displayed.buckets.length}, minmax(100px, 1fr))`;

  return (
    <section className="timeline-section" aria-label="任务时间线">
      <div className="timeline-heading">
        <div>
          <p className="section-kicker">创建至最近更新</p>
          <h2 id="timeline-title">时间线</h2>
        </div>
        <div className="timeline-controls">
          <div className="timeline-scale" role="group" aria-label="时间粒度">
            <span className="timeline-control-label">粒度</span>
            {GRANULARITIES.map((value) => (
              <button
                key={value}
                type="button"
                className={value === timeline.granularity ? "is-active" : ""}
                aria-pressed={value === timeline.granularity}
                disabled={loading}
                onClick={() => void changeGranularity(value)}
              >
                {{ day: "日", week: "周", month: "月" }[value]}
              </button>
            ))}
          </div>
          <form className="timeline-timezone" onSubmit={submitTimezone}>
            <label htmlFor="timeline-timezone">时区</label>
            <div className="timeline-timezone-input">
              <input
                id="timeline-timezone"
                value={draftTimezone}
                onChange={(event) => setDraftTimezone(event.target.value)}
                placeholder="America/Los_Angeles"
                list="timeline-timezone-options"
                autoComplete="off"
              />
              <button type="submit" disabled={loading}>应用</button>
            </div>
            <datalist id="timeline-timezone-options">
              <option value="UTC" />
              <option value="Asia/Shanghai" />
              <option value="America/Los_Angeles" />
              <option value="America/New_York" />
              <option value="Europe/London" />
            </datalist>
          </form>
        </div>
      </div>

      <div className="timeline-navigation">
        <button type="button" disabled={offset === 0} onClick={() => setWindowStart(Math.max(0, offset - windowSize))}>← 上一段</button>
        <button type="button" disabled={offset === maxWindowStart} onClick={() => setWindowStart(Math.min(maxWindowStart, offset + windowSize))}>下一段 →</button>
        <button type="button" disabled={offset === maxWindowStart} onClick={() => setWindowStart(maxWindowStart)}>回到最近</button>
        <button type="button" disabled={!selectedId} onClick={() => {
          const range = timeline.ranges.find((item) => item.conversationId === selectedId);
          const last = range ? lastIntersectingBucketIndex(range, timeline) : -1;
          if (last >= 0) setWindowStart(Math.max(0, Math.min(maxWindowStart, last - Math.floor(windowSize / 2))));
        }}>定位选中任务</button>
        <label>显示范围 <select aria-label="显示时间桶数量" value={windowSize} onChange={(event) => {
          const nextSize = Number(event.target.value);
          setWindowStart(Math.max(0, Math.min(timeline.buckets.length - nextSize, offset + windowSize - nextSize)));
          setWindowSize(nextSize);
        }}><option value={7}>7 个时间桶</option><option value={14}>14 个时间桶</option><option value={30}>30 个时间桶</option></select></label>
        <label className="timeline-position">浏览时间
          <input type="range" min={0} max={maxWindowStart} value={offset} disabled={maxWindowStart === 0} onChange={(event) => setWindowStart(Number(event.target.value))} aria-label="时间线位置" />
        </label>
        <span className="timeline-visible-range" aria-live="polite">{displayed.buckets[0]?.label} — {displayed.buckets.at(-1)?.label} · {offset + 1}–{Math.min(offset + windowSize, timeline.buckets.length)} / {timeline.buckets.length}</span>
      </div>
      <div className="timeline-meta" aria-live="polite">
        <span>{timeline.timezone}</span>
        <span>{{ day: "日", week: "周", month: "月" }[timeline.granularity]}时间桶</span>
        <span>相交任务数</span>
        <span>来源时间 · 只读</span>
        {loading && <span className="timeline-loading">更新中…</span>}
      </div>

      {activeError && (
        <p className="timeline-error" role="alert">
          {activeError}
        </p>
      )}

      {visibleWarnings.length > 0 && (
        <div className="timeline-warning" role="alert" aria-label="时间线提示">
          <div>
            <strong>无效的观测区间</strong>
            <span>{visibleWarnings.length} 条来源记录不会绘制或计数。</span>
          </div>
          <ul>
            {visibleWarnings.map((warning) => {
              const conversation = conversationById.get(warning.conversationId);
              return (
                <li key={warning.conversationId}>
                  <button type="button" onClick={() => onSelect(warning.conversationId)}>
                    {conversation?.displayTitle ?? warning.conversationId}
                  </button>
                  <span>{warning.message}</span>
                </li>
              );
            })}
          </ul>
        </div>
      )}

      {timeline.buckets.length === 0 ? (
        <div className="timeline-empty">没有有效的观测区间。</div>
      ) : (
        <div className="timeline-scroll" aria-label="时间线时间桶">
          <div className="timeline-grid" style={{ gridTemplateColumns }}>
            <div className="timeline-axis-label">任务 / 观测区间</div>
            {visibleBuckets.map((bucket) => (
              <div
                key={bucket.start}
                className="timeline-bucket"
                aria-label={`${bucket.label}, ${bucket.overlapCount} 个相交任务`}
                title={`${formatTimelineTime(bucket.start, timeline.timezone)} → ${formatTimelineTime(bucket.end, timeline.timezone)}`}
              >
                <span>{bucket.label}</span>
                <strong>{bucket.overlapCount}</strong>
              </div>
            ))}

            {validRanges.map((range) => {
              const conversation = conversationById.get(range.conversationId);
              const columns = rangeColumns(range, displayed);
              if (!conversation || !columns) return null;
              const rangeLabel = range.isPoint ? "时间点" : "区间";
              return (
                <div
                  key={range.conversationId}
                  className="timeline-row"
                  style={{ gridTemplateColumns }}
                >
                  <button
                    type="button"
                    className={`timeline-row-label ${conversation.id === selectedId ? "is-selected" : ""}`}
                    aria-pressed={conversation.id === selectedId}
                    data-conversation-id={conversation.id}
                    onClick={() => onSelect(conversation.id)}
                  >
                    <strong title={conversation.displayTitle}>{conversation.displayTitle}</strong>
                    <span>
                      {conversation.overlay.hidden ? "已隐藏 · " : ""}
                      {conversation.codex.archived ? "已归档" : "未归档"} · {capitalize(rangeLabel)}
                    </span>
                    <code>{conversation.id}</code>
                  </button>
                  <div
                    className="timeline-track"
                    style={{ gridColumn: "2 / -1", gridTemplateColumns: trackTemplateColumns }}
                  >
                    {displayed.buckets.map((bucket) => (
                      <span key={bucket.start} className="timeline-track-cell" aria-hidden="true" />
                    ))}
                    <button
                      type="button"
                      className={`timeline-bar ${range.isPoint ? "is-point" : ""} ${conversation.id === selectedId ? "is-selected" : ""}`}
                      style={rangeStyle(range, displayed)}
                      data-conversation-id={conversation.id}
                      aria-label={`${conversation.displayTitle}观测${rangeLabel}`}
                      title={`${formatTimelineTime(range.start, timeline.timezone)} → ${formatTimelineTime(range.end, timeline.timezone)}`}
                      onClick={() => onSelect(conversation.id)}
                    >
                      {range.isPoint ? <span className="timeline-point-mark" /> : <span>观测区间</span>}
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}
      <p className="timeline-caption">
        区间表示创建至最近更新的时间跨度，不代表连续工作时长。数字为时间桶内相交的不同任务数；时间来自来源记录，只读。
      </p>
    </section>
  );
}

export function rangeStyle(range: ObservationRange, timeline: TimelineSnapshot) {
  const buckets = timeline.buckets;
  function position(value: string | null) {
    const time = Date.parse(value ?? "");
    if (time <= Date.parse(buckets[0].start)) return 0;
    const index = buckets.findIndex(bucket => time < Date.parse(bucket.end));
    if (index < 0) return 100;
    const bucket = buckets[index];
    return (index + (time - Date.parse(bucket.start)) / (Date.parse(bucket.end) - Date.parse(bucket.start))) / buckets.length * 100;
  }
  const left = position(range.start), right = position(range.end);
  return { left: `${left}%`, width: range.isPoint ? "10px" : `${Math.max(0, right - left)}%` };
}

function rangeColumns(
  range: ObservationRange,
  timeline: TimelineSnapshot,
): { start: number; end: number } | null {
  let first = -1;
  let last = -1;
  timeline.buckets.forEach((bucket, index) => {
    if (rangeIntersectsBucket(range, bucket)) {
      if (first === -1) first = index;
      last = index;
    }
  });

  return first === -1 ? null : { start: first + 1, end: last + 2 };
}

function lastIntersectingBucketIndex(range: ObservationRange, timeline: TimelineSnapshot): number {
  for (let index = timeline.buckets.length - 1; index >= 0; index -= 1) {
    if (rangeIntersectsBucket(range, timeline.buckets[index])) return index;
  }
  return -1;
}

function countVisibleOverlaps(
  bucket: TimelineSnapshot["buckets"][number],
  ranges: ObservationRange[],
  conversations: Map<string, Conversation>,
): number {
  const counted = new Set<string>();
  ranges.forEach((range) => {
    if (!range.valid || counted.has(range.conversationId)) return;
    const conversation = conversations.get(range.conversationId);
    if (!conversation || conversation.derived.missing) return;
    if (rangeIntersectsBucket(range, bucket)) counted.add(range.conversationId);
  });
  return counted.size;
}

function rangeIntersectsBucket(
  range: ObservationRange,
  bucket: TimelineSnapshot["buckets"][number],
): boolean {
  if (!range.valid || !range.start || !range.end) return false;
  const start = Date.parse(range.start);
  const end = Date.parse(range.end);
  const bucketStart = Date.parse(bucket.start);
  const bucketEnd = Date.parse(bucket.end);
  if (![start, end, bucketStart, bucketEnd].every(Number.isFinite)) return false;
  return range.isPoint
    ? start >= bucketStart && start < bucketEnd
    : start < bucketEnd && end > bucketStart;
}

function capitalize(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

function formatTimelineTime(value: string | null, timezone: string): string {
  if (!value) return "未知 time";
  try {
    return new Intl.DateTimeFormat(undefined, {
      timeZone: timezone,
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(value));
  } catch {
    return `${value} UTC`;
  }
}
