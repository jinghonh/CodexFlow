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
  const [localError, setLocalError] = useState<string | null>(null);

  useEffect(() => {
    if (!timeline) return;
    setGranularity(timeline.granularity);
    setTimezone(timeline.timezone);
    setDraftTimezone(timeline.timezone);
  }, [timeline?.granularity, timeline?.timezone]);

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
      setLocalError("Enter an IANA time zone, such as UTC or Asia/Shanghai.");
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
      <section className="timeline-section" aria-label="Conversation timeline">
        <div className="timeline-heading">
          <div>
            <p className="section-kicker">Observation range / source time</p>
            <h2 id="timeline-title">Timeline</h2>
          </div>
        </div>
        <div className="timeline-empty">Timeline data is not available in this source snapshot.</div>
      </section>
    );
  }

  const validRanges = timeline.ranges.filter((range) => {
    const conversation = conversationById.get(range.conversationId);
    return range.valid && conversation !== undefined && !conversation.derived.missing;
  });
  const gridTemplateColumns = `minmax(210px, 0.42fr) repeat(${timeline.buckets.length}, minmax(132px, 1fr))`;
  const trackTemplateColumns = `repeat(${timeline.buckets.length}, minmax(132px, 1fr))`;

  return (
    <section className="timeline-section" aria-label="Conversation timeline">
      <div className="timeline-heading">
        <div>
          <p className="section-kicker">Observation range / source time</p>
          <h2 id="timeline-title">Timeline</h2>
        </div>
        <div className="timeline-controls">
          <div className="timeline-scale" role="group" aria-label="Time bucket">
            <span className="timeline-control-label">Scale</span>
            {GRANULARITIES.map((value) => (
              <button
                key={value}
                type="button"
                className={value === timeline.granularity ? "is-active" : ""}
                aria-pressed={value === timeline.granularity}
                disabled={loading}
                onClick={() => void changeGranularity(value)}
              >
                {capitalize(value)}
              </button>
            ))}
          </div>
          <form className="timeline-timezone" onSubmit={submitTimezone}>
            <label htmlFor="timeline-timezone">Time zone</label>
            <div className="timeline-timezone-input">
              <input
                id="timeline-timezone"
                value={draftTimezone}
                onChange={(event) => setDraftTimezone(event.target.value)}
                placeholder="America/Los_Angeles"
                list="timeline-timezone-options"
                autoComplete="off"
              />
              <button type="submit" disabled={loading}>Apply</button>
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

      <div className="timeline-meta" aria-live="polite">
        <span>{timeline.timezone}</span>
        <span>{capitalize(timeline.granularity)} buckets</span>
        <span>Overlap count</span>
        <span>Source times · read-only</span>
        {loading && <span className="timeline-loading">Updating…</span>}
      </div>

      {activeError && (
        <p className="timeline-error" role="alert">
          {activeError}
        </p>
      )}

      {timeline.warnings.length > 0 && (
        <div className="timeline-warning" role="alert" aria-label="Timeline warnings">
          <div>
            <strong>Invalid observation range</strong>
            <span>{timeline.warnings.length} source record{timeline.warnings.length === 1 ? "" : "s"} will not be drawn or counted.</span>
          </div>
          <ul>
            {timeline.warnings.map((warning) => {
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
        <div className="timeline-empty">No valid Observation range is available to draw.</div>
      ) : (
        <div className="timeline-scroll" aria-label="Timeline buckets">
          <div className="timeline-grid" style={{ gridTemplateColumns }}>
            <div className="timeline-axis-label">Conversation / range</div>
            {timeline.buckets.map((bucket) => (
              <div
                key={bucket.start}
                className="timeline-bucket"
                aria-label={`${bucket.label}, ${bucket.overlapCount} overlapping conversations`}
                title={`${formatTimelineTime(bucket.start, timeline.timezone)} → ${formatTimelineTime(bucket.end, timeline.timezone)}`}
              >
                <span>{bucket.label}</span>
                <strong>{bucket.overlapCount}</strong>
              </div>
            ))}

            {validRanges.map((range) => {
              const conversation = conversationById.get(range.conversationId);
              const columns = rangeColumns(range, timeline);
              if (!conversation || !columns) return null;
              const rangeLabel = range.isPoint ? "point" : "range";
              return (
                <div
                  key={range.conversationId}
                  className="timeline-row"
                  style={{ gridTemplateColumns }}
                >
                  <button
                    type="button"
                    className={`timeline-row-label ${conversation.id === selectedId ? "is-selected" : ""}`}
                    onClick={() => onSelect(conversation.id)}
                  >
                    <strong>{conversation.displayTitle}</strong>
                    <span>
                      {conversation.overlay.hidden ? "Hidden · " : ""}
                      {conversation.codex.archived ? "Archived" : "Active"} · {capitalize(rangeLabel)}
                    </span>
                    <code>{conversation.id}</code>
                  </button>
                  <div
                    className="timeline-track"
                    style={{ gridColumn: "2 / -1", gridTemplateColumns: trackTemplateColumns }}
                  >
                    {timeline.buckets.map((bucket) => (
                      <span key={bucket.start} className="timeline-track-cell" aria-hidden="true" />
                    ))}
                    <button
                      type="button"
                      className={`timeline-bar ${range.isPoint ? "is-point" : ""} ${conversation.id === selectedId ? "is-selected" : ""}`}
                      style={{ gridColumn: `${columns.start} / ${columns.end}` }}
                      aria-label={`${conversation.displayTitle} observation ${rangeLabel}`}
                      title={`${formatTimelineTime(range.start, timeline.timezone)} → ${formatTimelineTime(range.end, timeline.timezone)}`}
                      onClick={() => onSelect(conversation.id)}
                    >
                      {range.isPoint ? <span className="timeline-point-mark">Point</span> : <span>Observation range</span>}
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}
      <p className="timeline-caption">
        Counts are distinct Conversations whose Observation ranges intersect each half-open bucket. Scroll horizontally to inspect the field; source times cannot be dragged.
      </p>
    </section>
  );
}

function rangeColumns(
  range: ObservationRange,
  timeline: TimelineSnapshot,
): { start: number; end: number } | null {
  if (!range.start || !range.end) return null;
  const start = Date.parse(range.start);
  const end = Date.parse(range.end);
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null;

  let first = -1;
  let last = -1;
  timeline.buckets.forEach((bucket, index) => {
    const bucketStart = Date.parse(bucket.start);
    const bucketEnd = Date.parse(bucket.end);
    const intersects = range.isPoint
      ? start >= bucketStart && start < bucketEnd
      : start < bucketEnd && end > bucketStart;
    if (intersects) {
      if (first === -1) first = index;
      last = index;
    }
  });

  return first === -1 ? null : { start: first + 1, end: last + 2 };
}

function capitalize(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

function formatTimelineTime(value: string | null, timezone: string): string {
  if (!value) return "unknown time";
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
