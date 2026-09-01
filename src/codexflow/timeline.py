from __future__ import annotations

import os
from collections.abc import Iterable
from dataclasses import dataclass
from datetime import UTC, date, datetime, time, timedelta, tzinfo
from pathlib import Path
from typing import TYPE_CHECKING, Any, Literal
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from .source import CodexThread

if TYPE_CHECKING:
    from .models import Conversation

TimelineGranularity = Literal["day", "week", "month"]
DEFAULT_GRANULARITY: TimelineGranularity = "day"


class TimelineInputError(ValueError):
    """A Timeline display option cannot be interpreted safely."""


@dataclass(frozen=True)
class ObservationRange:
    conversation_id: str
    start: str | None
    end: str | None
    valid: bool
    is_point: bool
    error: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "conversationId": self.conversation_id,
            "start": self.start,
            "end": self.end,
            "valid": self.valid,
            "isPoint": self.is_point,
            "error": self.error,
        }


@dataclass(frozen=True)
class TimelineWarning:
    conversation_id: str
    code: str
    message: str

    def to_dict(self) -> dict[str, str]:
        return {
            "conversationId": self.conversation_id,
            "code": self.code,
            "message": self.message,
        }


@dataclass(frozen=True)
class TimeBucket:
    start: str
    end: str
    label: str
    overlap_count: int

    def to_dict(self) -> dict[str, Any]:
        return {
            "start": self.start,
            "end": self.end,
            "label": self.label,
            "overlapCount": self.overlap_count,
        }


@dataclass(frozen=True)
class TimelineSnapshot:
    granularity: TimelineGranularity = DEFAULT_GRANULARITY
    timezone: str = "UTC"
    ranges: tuple[ObservationRange, ...] = ()
    buckets: tuple[TimeBucket, ...] = ()
    warnings: tuple[TimelineWarning, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        return {
            "granularity": self.granularity,
            "timezone": self.timezone,
            "ranges": [item.to_dict() for item in self.ranges],
            "buckets": [item.to_dict() for item in self.buckets],
            "warnings": [item.to_dict() for item in self.warnings],
        }


def observation_range_for_thread(thread: CodexThread) -> ObservationRange:
    start = _parse_timestamp(thread.created_at)
    end = _parse_timestamp(thread.updated_at)
    errors: list[str] = []
    if start is None:
        errors.append("createdAt 无效")
    if end is None:
        errors.append("updatedAt 无效")
    if start is not None and end is not None and end < start:
        errors.append("updatedAt 早于 createdAt")

    valid = not errors
    return ObservationRange(
        conversation_id=thread.id,
        start=_format_timestamp(start),
        end=_format_timestamp(end),
        valid=valid,
        is_point=valid and start == end,
        error="；".join(errors) if errors else None,
    )


def build_timeline(
    conversations: Iterable[Conversation],
    *,
    granularity: str = DEFAULT_GRANULARITY,
    timezone: str | None = None,
) -> TimelineSnapshot:
    normalized_granularity = _normalize_granularity(granularity)
    zone, timezone_name = _resolve_timezone(timezone)
    ranges: list[ObservationRange] = []
    warnings: list[TimelineWarning] = []
    valid_ranges: list[tuple[ObservationRange, datetime, datetime]] = []

    for conversation in conversations:
        thread = conversation.codex
        if thread is None or conversation.derived.missing:
            continue
        observation_range = observation_range_for_thread(thread)
        ranges.append(observation_range)
        if not observation_range.valid:
            warnings.append(
                TimelineWarning(
                    conversation_id=conversation.id,
                    code="invalid_observation_range",
                    message=observation_range.error or "Observation range 无效",
                )
            )
            continue
        start = _parse_timestamp(thread.created_at)
        end = _parse_timestamp(thread.updated_at)
        if start is not None and end is not None:
            valid_ranges.append((observation_range, start, end))

    if not valid_ranges:
        return TimelineSnapshot(
            granularity=normalized_granularity,
            timezone=timezone_name,
            ranges=tuple(ranges),
            warnings=tuple(warnings),
        )

    first_bucket: datetime | None = None
    last_bucket: datetime | None = None
    for _, start, end in valid_ranges:
        start_local = start.astimezone(zone)
        end_probe = start if start == end else end - timedelta(microseconds=1)
        end_local = end_probe.astimezone(zone)
        range_first = _bucket_start(start_local, normalized_granularity, zone)
        range_last = _bucket_start(end_local, normalized_granularity, zone)
        first_bucket = range_first if first_bucket is None else min(first_bucket, range_first)
        last_bucket = range_last if last_bucket is None else max(last_bucket, range_last)

    assert first_bucket is not None and last_bucket is not None
    buckets: list[TimeBucket] = []
    current = first_bucket
    while current <= last_bucket:
        next_bucket = _next_bucket(current, normalized_granularity, zone)
        bucket_start = current.astimezone(UTC)
        bucket_end = next_bucket.astimezone(UTC)
        conversation_ids = {
            observation_range.conversation_id
            for observation_range, start, end in valid_ranges
            if _range_intersects_bucket(start, end, bucket_start, bucket_end)
        }
        buckets.append(
            TimeBucket(
                start=_format_timestamp(bucket_start),
                end=_format_timestamp(bucket_end),
                label=_bucket_label(current, normalized_granularity),
                overlap_count=len(conversation_ids),
            )
        )
        current = next_bucket

    return TimelineSnapshot(
        granularity=normalized_granularity,
        timezone=timezone_name,
        ranges=tuple(ranges),
        buckets=tuple(buckets),
        warnings=tuple(warnings),
    )


def _normalize_granularity(value: str) -> TimelineGranularity:
    if value not in {"day", "week", "month"}:
        raise TimelineInputError("Timeline 粒度必须是 day、week 或 month")
    return value  # type: ignore[return-value]


def _resolve_timezone(value: str | None) -> tuple[tzinfo, str]:
    timezone_name = value.strip() if isinstance(value, str) and value.strip() else _system_timezone_name()
    try:
        return ZoneInfo(timezone_name), timezone_name
    except (ZoneInfoNotFoundError, ValueError) as exc:
        raise TimelineInputError(f"无法识别用户时区：{timezone_name}") from exc


def _system_timezone_name() -> str:
    local_timezone = datetime.now().astimezone().tzinfo
    key = getattr(local_timezone, "key", None)
    if isinstance(key, str) and key:
        return key

    configured = os.environ.get("TZ")
    if configured:
        configured = configured.removeprefix(":")
        try:
            ZoneInfo(configured)
        except (ZoneInfoNotFoundError, ValueError):
            pass
        else:
            return configured

    localtime_path = Path("/etc/localtime")
    try:
        resolved = localtime_path.resolve(strict=True)
        parts = resolved.parts
        if "zoneinfo" in parts:
            return "/".join(parts[parts.index("zoneinfo") + 1 :])
    except (OSError, RuntimeError):
        pass

    return "UTC"


def _parse_timestamp(value: str | None) -> datetime | None:
    if not isinstance(value, str) or not value.strip():
        return None
    normalized = value.strip()
    if normalized.endswith(("Z", "z")):
        normalized = f"{normalized[:-1]}+00:00"
    try:
        parsed = datetime.fromisoformat(normalized)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        return None
    return parsed.astimezone(UTC)


def _format_timestamp(value: datetime | None) -> str | None:
    if value is None:
        return None
    timespec = "microseconds" if value.microsecond else "seconds"
    return value.astimezone(UTC).isoformat(timespec=timespec).replace("+00:00", "Z")


def _bucket_start(local_value: datetime, granularity: TimelineGranularity, zone: tzinfo) -> datetime:
    local_date = local_value.date()
    if granularity == "day":
        bucket_date = local_date
    elif granularity == "week":
        bucket_date = local_date - timedelta(days=local_date.isoweekday() - 1)
    else:
        bucket_date = local_date.replace(day=1)
    return datetime.combine(bucket_date, time.min, tzinfo=zone)


def _next_bucket(current: datetime, granularity: TimelineGranularity, zone: tzinfo) -> datetime:
    current_date = current.date()
    if granularity == "day":
        next_date = current_date + timedelta(days=1)
    elif granularity == "week":
        next_date = current_date + timedelta(days=7)
    else:
        next_date = (
            date(current_date.year + 1, 1, 1)
            if current_date.month == 12
            else date(current_date.year, current_date.month + 1, 1)
        )
    return datetime.combine(next_date, time.min, tzinfo=zone)


def _bucket_label(local_value: datetime, granularity: TimelineGranularity) -> str:
    if granularity == "day":
        return local_value.date().isoformat()
    if granularity == "week":
        iso_year, iso_week, _ = local_value.date().isocalendar()
        return f"{iso_year}-W{iso_week:02d}"
    return f"{local_value.year:04d}-{local_value.month:02d}"


def _range_intersects_bucket(
    start: datetime,
    end: datetime,
    bucket_start: datetime,
    bucket_end: datetime,
) -> bool:
    if start == end:
        return bucket_start <= start < bucket_end
    return start < bucket_end and end > bucket_start
