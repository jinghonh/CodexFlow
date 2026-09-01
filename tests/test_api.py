from __future__ import annotations

from pathlib import Path

import httpx
import pytest

from codexflow.app import create_app
from codexflow.source import CodexThread, SourceFailure, SourceReadResult


def thread(thread_id: str, cwd: Path, *, archived: bool = False) -> CodexThread:
    return CodexThread(
        id=thread_id,
        title=f"Title {thread_id}",
        preview=f"Preview {thread_id}",
        created_at="2024-01-01T00:00:00Z",
        updated_at="2024-01-01T01:00:00Z",
        recency_at="2024-01-01T01:00:00Z",
        cwd=str(cwd),
        source="vscode" if archived else "cli",
        archived=archived,
        history_mode=None,
        status="idle",
        project_id=None,
        git_info=None,
    )


class ApiSource:
    def __init__(self, result: SourceReadResult) -> None:
        self.result = result
        self.read_count = 0

    def read_snapshot(self) -> SourceReadResult:
        self.read_count += 1
        return self.result


def ready(*threads: CodexThread) -> SourceReadResult:
    return SourceReadResult(
        status="ready",
        threads=tuple(threads),
        generated_at="2024-01-01T02:00:00Z",
        user_agent="Codex Desktop/0.150.1 fixture",
    )


@pytest.mark.asyncio
async def test_http_api_selects_project_and_returns_source_conversations(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = ApiSource(
        ready(
            thread("active-id", project),
            thread("archived-id", project, archived=True),
        )
    )
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        health = await client.get("/api/health")
        selected = await client.post("/api/project/select", json={"path": str(project)})
        assert source.read_count == 1
        snapshot = await client.get("/api/snapshot")

    assert health.status_code == 200
    assert health.json()["listenHost"] == "127.0.0.1"
    assert selected.status_code == 200
    assert selected.json()["project"]["realPath"] == str(project)
    assert snapshot.status_code == 200
    body = snapshot.json()
    assert body["source"]["status"] == "ready"
    assert [item["id"] for item in body["conversations"]] == [
        "active-id",
        "archived-id",
    ]
    assert body["conversations"][1]["codex"]["archived"] is True


@pytest.mark.asyncio
async def test_http_snapshot_exposes_graph_nodes_without_source_field_copies(tmp_path: Path) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    graph_path.write_text(
        """
version: 1
nodes:
  missing-id:
    title: Historical conversation
edges:
  - id: edge-1
    source: present-id
    target: missing-id
    type: continues
""",
        encoding="utf-8",
    )
    app = create_app(source=ApiSource(ready(thread("present-id", project))))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        selected = await client.post("/api/project/select", json={"path": str(project)})
        snapshot = await client.get("/api/snapshot")

    assert selected.status_code == 200
    assert selected.json()["project"]["graphFileStatus"] == "ready"
    assert snapshot.status_code == 200
    body = snapshot.json()
    nodes = {node["id"]: node for node in body["graph"]["nodes"]}
    assert nodes["present-id"]["displayTitle"] == "Title present-id"
    assert nodes["missing-id"] == {
        "id": "missing-id",
        "displayTitle": "Historical conversation",
        "missing": True,
        "hidden": False,
        "layout": None,
    }
    assert "cwd" not in nodes["present-id"]
    assert "createdAt" not in nodes["present-id"]
    assert body["graph"]["edges"] == [
        {"id": "edge-1", "source": "present-id", "target": "missing-id", "type": "continues"}
    ]


@pytest.mark.asyncio
async def test_http_snapshot_explains_source_threads_excluded_by_project_membership(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    inside = project / "src"
    outside = tmp_path / "other-project"
    inside.mkdir(parents=True)
    outside.mkdir()
    source = ApiSource(ready(thread("inside", inside), thread("outside", outside)))
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        selected = await client.post("/api/project/select", json={"path": str(project)})
        snapshot = await client.get("/api/snapshot")

    assert selected.status_code == 200
    assert snapshot.status_code == 200
    body = snapshot.json()
    assert [item["id"] for item in body["conversations"]] == ["inside"]
    assert body["excludedConversations"] == [
        {
            "id": "outside",
            "cwd": str(outside),
            "resolvedCwd": str(outside.resolve()),
            "gitRoot": None,
            "worktreeRoot": None,
            "reason": "outside_project",
        }
    ]


@pytest.mark.asyncio
async def test_http_runtime_serves_the_built_dashboard_at_root() -> None:
    app = create_app(source=ApiSource(ready()))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        response = await client.get("/")

    assert response.status_code == 200
    assert "CodexFlow" in response.text


@pytest.mark.asyncio
async def test_http_api_returns_source_unavailable_without_a_snapshot(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    result = SourceReadResult(
        status="unavailable",
        threads=(),
        generated_at=None,
        user_agent=None,
        error=SourceFailure("source_unavailable", "fixture app-server failed"),
    )
    app = create_app(source=ApiSource(result))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        snapshot = await client.get("/api/snapshot")

    assert snapshot.status_code == 503
    assert snapshot.json()["error"]["code"] == "source_unavailable"
    assert snapshot.json()["error"]["retryable"] is True


@pytest.mark.asyncio
async def test_http_refresh_keeps_the_last_complete_conversations_when_source_is_stale(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = ApiSource(ready(thread("stable-id", project)))
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        first = await client.get("/api/snapshot")
        source.result = SourceReadResult(
            status="stale",
            threads=(thread("stable-id", project),),
            generated_at="2024-01-01T02:00:00Z",
            user_agent="Codex Desktop/0.150.1 fixture",
            error=SourceFailure("source_unavailable", "refresh failed"),
        )
        refreshed = await client.post("/api/refresh")

    assert first.status_code == 200
    assert refreshed.status_code == 200
    assert refreshed.json()["source"]["status"] == "stale"
    assert [item["id"] for item in refreshed.json()["conversations"]] == ["stable-id"]


@pytest.mark.asyncio
async def test_http_project_select_reports_a_graph_parse_error_without_overwriting_the_file(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    graph_path.write_text("version: [", encoding="utf-8")
    app = create_app(source=ApiSource(ready()))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        selected = await client.post("/api/project/select", json={"path": str(project)})

    assert selected.status_code == 422
    assert selected.json()["error"]["code"] == "graph_parse_error"
    assert graph_path.read_text(encoding="utf-8") == "version: ["
