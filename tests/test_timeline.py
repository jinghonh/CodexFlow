from __future__ import annotations

from pathlib import Path

import httpx
import pytest

from codexflow.app import create_app
from codexflow.project import ProjectGraphService
from codexflow.source import CodexThread, SourceReadResult


def make_thread(
    thread_id: str,
    cwd: Path,
    *,
    created_at: str,
    updated_at: str,
    archived: bool = False,
) -> CodexThread:
    return CodexThread(
        id=thread_id,
        title=thread_id,
        preview=thread_id,
        created_at=created_at,
        updated_at=updated_at,
        recency_at=updated_at,
        cwd=str(cwd),
        source="cli",
        archived=archived,
        history_mode=None,
        status="idle",
        project_id=None,
        git_info=None,
    )


class StubSource:
    def __init__(self, *threads: CodexThread) -> None:
        self.threads = threads

    def read_snapshot(self) -> SourceReadResult:
        return SourceReadResult(
            status="ready",
            threads=self.threads,
            generated_at="2024-01-03T00:00:00Z",
            user_agent="timeline fixture",
        )


def test_snapshot_exposes_day_buckets_with_half_open_observation_ranges(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "first",
            project,
            created_at="2024-01-01T23:00:00Z",
            updated_at="2024-01-02T00:00:00Z",
        ),
        make_thread(
            "second",
            project,
            created_at="2024-01-02T00:00:00Z",
            updated_at="2024-01-03T00:00:00Z",
        ),
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    timeline = service.snapshot(granularity="day", timezone="UTC").timeline

    assert timeline.granularity == "day"
    assert timeline.timezone == "UTC"
    assert [
        (bucket.start, bucket.end, bucket.overlap_count)
        for bucket in timeline.buckets
    ] == [
        ("2024-01-01T00:00:00Z", "2024-01-02T00:00:00Z", 1),
        ("2024-01-02T00:00:00Z", "2024-01-03T00:00:00Z", 1),
    ]


def test_snapshot_marks_zero_duration_as_a_point_in_only_its_bucket(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "range",
            project,
            created_at="2024-01-01T12:00:00Z",
            updated_at="2024-01-03T12:00:00Z",
        ),
        make_thread(
            "point",
            project,
            created_at="2024-01-02T12:00:00Z",
            updated_at="2024-01-02T12:00:00Z",
        ),
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    timeline = service.snapshot(granularity="day", timezone="UTC").timeline
    ranges = {item.conversation_id: item for item in timeline.ranges}

    assert ranges["point"].is_point is True
    assert ranges["range"].is_point is False
    assert [bucket.overlap_count for bucket in timeline.buckets] == [1, 2, 1]


def test_day_buckets_use_the_selected_local_timezone_boundaries(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "local-midnight",
            project,
            created_at="2024-01-01T07:30:00Z",
            updated_at="2024-01-01T08:30:00Z",
        )
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    timeline = service.snapshot(
        granularity="day",
        timezone="America/Los_Angeles",
    ).timeline

    assert [bucket.label for bucket in timeline.buckets] == ["2023-12-31", "2024-01-01"]
    assert [bucket.start for bucket in timeline.buckets] == [
        "2023-12-31T08:00:00Z",
        "2024-01-01T08:00:00Z",
    ]


def test_week_buckets_start_on_the_local_iso_monday(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "iso-week",
            project,
            created_at="2024-01-07T23:00:00Z",
            updated_at="2024-01-08T08:00:00Z",
        )
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    timeline = service.snapshot(
        granularity="week",
        timezone="America/Los_Angeles",
    ).timeline

    assert len(timeline.buckets) == 1
    assert timeline.buckets[0].label == "2024-W01"
    assert timeline.buckets[0].start == "2024-01-01T08:00:00Z"
    assert timeline.buckets[0].end == "2024-01-08T08:00:00Z"


def test_month_buckets_follow_the_selected_local_calendar(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "month-boundary",
            project,
            created_at="2024-02-01T07:30:00Z",
            updated_at="2024-03-01T08:30:00Z",
        )
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    timeline = service.snapshot(
        granularity="month",
        timezone="America/Los_Angeles",
    ).timeline

    assert [(bucket.label, bucket.overlap_count) for bucket in timeline.buckets] == [
        ("2024-01", 1),
        ("2024-02", 1),
        ("2024-03", 1),
    ]


def test_day_bucket_uses_the_real_dst_length_of_the_selected_timezone(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "dst-day",
            project,
            created_at="2024-03-10T10:00:00Z",
            updated_at="2024-03-10T20:00:00Z",
        )
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    timeline = service.snapshot(
        granularity="day",
        timezone="America/Los_Angeles",
    ).timeline

    assert timeline.buckets[0].start == "2024-03-10T08:00:00Z"
    assert timeline.buckets[0].end == "2024-03-11T07:00:00Z"


def test_timeline_counts_hidden_and_archived_but_not_missing_or_invalid(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir()
    graph_path.write_text(
        """
version: 1
nodes:
  hidden:
    hidden: true
  missing:
    title: Historical work
""",
        encoding="utf-8",
    )
    source = StubSource(
        make_thread(
            "hidden",
            project,
            created_at="2024-01-02T10:00:00Z",
            updated_at="2024-01-02T11:00:00Z",
        ),
        make_thread(
            "archived",
            project,
            created_at="2024-01-02T12:00:00Z",
            updated_at="2024-01-02T13:00:00Z",
            archived=True,
        ),
        make_thread(
            "invalid",
            project,
            created_at="2024-01-02T14:00:00Z",
            updated_at="2024-01-02T13:00:00Z",
        ),
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    snapshot = service.snapshot(granularity="day", timezone="UTC")
    timeline = snapshot.timeline

    assert {item.conversation_id for item in timeline.ranges} == {
        "hidden",
        "archived",
        "invalid",
    }
    assert timeline.buckets[0].overlap_count == 2
    assert [warning.conversation_id for warning in timeline.warnings] == ["invalid"]
    assert {conversation.id for conversation in snapshot.conversations} == {
        "hidden",
        "archived",
        "invalid",
        "missing",
    }
    assert snapshot.conversations[2].derived.valid_observation_range is False


def test_overlap_count_is_based_on_distinct_conversation_ids(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "same-id",
            project,
            created_at="2024-01-02T10:00:00Z",
            updated_at="2024-01-02T11:00:00Z",
        ),
        make_thread(
            "same-id",
            project,
            created_at="2024-01-02T10:00:00Z",
            updated_at="2024-01-02T11:00:00Z",
        ),
        make_thread(
            "other-id",
            project,
            created_at="2024-01-02T12:00:00Z",
            updated_at="2024-01-02T13:00:00Z",
        ),
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    timeline = service.snapshot(granularity="day", timezone="UTC").timeline

    assert [item.conversation_id for item in timeline.ranges] == ["same-id", "other-id"]
    assert timeline.buckets[0].overlap_count == 2


def test_invalid_observation_order_is_preserved_and_not_counted(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "invalid-order",
            project,
            created_at="2024-01-03T10:00:00Z",
            updated_at="2024-01-02T10:00:00Z",
        ),
        make_thread(
            "valid",
            project,
            created_at="2024-01-02T10:00:00Z",
            updated_at="2024-01-02T11:00:00Z",
        ),
    )
    service = ProjectGraphService(source)
    service.select_project(str(project))

    timeline = service.snapshot(granularity="day", timezone="UTC").timeline
    invalid = next(item for item in timeline.ranges if item.conversation_id == "invalid-order")

    assert invalid.valid is False
    assert invalid.start == "2024-01-03T10:00:00Z"
    assert invalid.end == "2024-01-02T10:00:00Z"
    assert [bucket.overlap_count for bucket in timeline.buckets] == [1]
    assert timeline.warnings[0].conversation_id == "invalid-order"


@pytest.mark.asyncio
async def test_http_snapshot_accepts_timeline_granularity_and_timezone(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(
        make_thread(
            "week-thread",
            project,
            created_at="2024-01-07T23:00:00Z",
            updated_at="2024-01-08T08:00:00Z",
        )
    )
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        response = await client.get(
            "/api/snapshot",
            params={"granularity": "week", "timezone": "America/Los_Angeles"},
        )

    assert response.status_code == 200
    body = response.json()
    assert body["timeline"]["granularity"] == "week"
    assert body["timeline"]["timezone"] == "America/Los_Angeles"
    assert body["timeline"]["buckets"][0]["label"] == "2024-W01"


@pytest.mark.asyncio
async def test_http_snapshot_rejects_an_unknown_timeline_option(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=StubSource())

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        response = await client.get(
            "/api/snapshot",
            params={"granularity": "quarter", "timezone": "UTC"},
        )

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "invalid_request"
