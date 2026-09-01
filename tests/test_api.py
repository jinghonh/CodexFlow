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
async def test_http_patch_node_updates_overlay_and_returns_the_new_snapshot(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=ApiSource(ready(thread("stable-id", project))))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        assert initial.headers["etag"] == '"absent"'
        updated = await client.patch(
            "/api/graph/nodes/stable-id",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={
                "title": "Local title",
                "tags": [" alpha ", "alpha", "beta"],
                "status": "active",
                "note": "Keep this context",
                "hidden": True,
                "layout": {"x": 320, "y": 180},
            },
        )
        reloaded = await client.get("/api/snapshot")

    assert updated.status_code == 200
    body = updated.json()
    conversation = body["conversations"][0]
    assert conversation["overlay"] == {
        "title": "Local title",
        "tags": ["alpha", "beta"],
        "status": "active",
        "note": "Keep this context",
        "hidden": True,
        "layout": {"x": 320.0, "y": 180.0},
    }
    assert body["graph"]["etag"] != "absent"
    assert updated.headers["etag"] == f'"{body["graph"]["etag"]}"'
    assert reloaded.json()["conversations"][0]["overlay"] == conversation["overlay"]


@pytest.mark.asyncio
async def test_http_patch_node_rejects_invalid_status_without_creating_a_file(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=ApiSource(ready(thread("stable-id", project))))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        response = await client.patch(
            "/api/graph/nodes/stable-id",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={"status": "archived"},
        )

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "invalid_request"
    assert not (project / ".codex" / "graph.yaml").exists()


@pytest.mark.asyncio
async def test_http_patch_node_reports_an_etag_conflict_without_overwriting(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir()
    graph_path.write_text("version: 1\nnodes: {}\n", encoding="utf-8")
    app = create_app(source=ApiSource(ready(thread("stable-id", project))))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        original = graph_path.read_text(encoding="utf-8")
        response = await client.patch(
            "/api/graph/nodes/stable-id",
            headers={"If-Match": "stale-etag"},
            json={"title": "New title"},
        )

    assert response.status_code == 412
    assert response.json()["error"]["code"] == "graph_conflict"
    assert graph_path.read_text(encoding="utf-8") == original


@pytest.mark.asyncio
async def test_http_patch_node_requires_an_etag_header(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=ApiSource(ready(thread("stable-id", project))))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        response = await client.patch(
            "/api/graph/nodes/stable-id",
            json={"title": "Local title"},
        )

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "invalid_request"


@pytest.mark.asyncio
async def test_http_patch_node_rejects_coerced_overlay_field_types(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=ApiSource(ready(thread("stable-id", project))))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        response = await client.patch(
            "/api/graph/nodes/stable-id",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={"hidden": 1, "layout": {"x": "320", "y": 180}},
        )
        huge_coordinate = await client.patch(
            "/api/graph/nodes/stable-id",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={"layout": {"x": 10**1000, "y": 180}},
        )

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "invalid_request"
    assert huge_coordinate.status_code == 400
    assert huge_coordinate.json()["error"]["code"] == "invalid_request"
    assert not (project / ".codex" / "graph.yaml").exists()


@pytest.mark.asyncio
async def test_http_api_creates_builtin_and_custom_edges_with_etags(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = ApiSource(ready(thread("source-id", project), thread("target-id", project)))
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        builtin = await client.post(
            "/api/graph/edges",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={
                "source": "source-id",
                "target": "target-id",
                "type": "continues",
                "label": "source continues target",
            },
        )
        custom = await client.post(
            "/api/graph/edges",
            headers={"If-Match": builtin.json()["graph"]["etag"]},
            json={
                "source": "target-id",
                "target": "source-id",
                "type": "  blocks_release  ",
            },
        )

    assert builtin.status_code == 200
    assert builtin.json()["edge"] == builtin.json()["graph"]["edges"][0]
    assert builtin.headers["etag"] == f'"{builtin.json()["graph"]["etag"]}"'
    assert custom.status_code == 200
    assert [edge["type"] for edge in custom.json()["graph"]["edges"]] == [
        "continues",
        "blocks_release",
    ]
    assert all(edge["id"] for edge in custom.json()["graph"]["edges"])


@pytest.mark.asyncio
async def test_http_api_updates_and_deletes_an_edge_without_deleting_conversations(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = ApiSource(ready(thread("source-id", project), thread("target-id", project)))
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        created = await client.post(
            "/api/graph/edges",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={
                "source": "source-id",
                "target": "target-id",
                "type": "continues",
                "label": "before edit",
            },
        )
        created_edge = created.json()["graph"]["edges"][0]
        updated = await client.patch(
            f"/api/graph/edges/{created_edge['id']}",
            headers={"If-Match": created.json()["graph"]["etag"]},
            json={"type": "implements", "label": "after edit"},
        )
        deleted = await client.delete(
            f"/api/graph/edges/{created_edge['id']}",
            headers={"If-Match": updated.json()["graph"]["etag"]},
        )

    assert updated.status_code == 200
    assert updated.json()["graph"]["edges"] == [
        {
            "id": created_edge["id"],
            "source": "source-id",
            "target": "target-id",
            "type": "implements",
            "label": "after edit",
        }
    ]
    assert updated.headers["etag"] == f'"{updated.json()["graph"]["etag"]}"'
    assert deleted.status_code == 200
    assert deleted.json()["graph"]["edges"] == []
    assert [conversation["id"] for conversation in deleted.json()["conversations"]] == [
        "source-id",
        "target-id",
    ]


@pytest.mark.asyncio
async def test_http_api_reports_edge_constraint_errors_without_changing_the_graph(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = ApiSource(ready(thread("alpha", project), thread("beta", project)))
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        created = await client.post(
            "/api/graph/edges",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={"source": "alpha", "target": "beta", "type": "related_to"},
        )
        duplicate = await client.post(
            "/api/graph/edges",
            headers={"If-Match": created.json()["graph"]["etag"]},
            json={"source": "beta", "target": "alpha", "type": "related_to"},
        )
        self_edge = await client.post(
            "/api/graph/edges",
            headers={"If-Match": created.json()["graph"]["etag"]},
            json={"source": "alpha", "target": "alpha", "type": "continues"},
        )
        missing_etag = await client.post(
            "/api/graph/edges",
            json={"source": "alpha", "target": "beta", "type": "continues"},
        )
        final = await client.get("/api/snapshot")

    assert created.status_code == 200
    assert duplicate.status_code == 409
    assert duplicate.json()["error"]["code"] == "duplicate_edge"
    assert self_edge.status_code == 409
    assert self_edge.json()["error"]["code"] == "self_edge"
    assert missing_etag.status_code == 400
    assert missing_etag.json()["error"]["code"] == "invalid_request"
    assert len(final.json()["graph"]["edges"]) == 1
    assert final.json()["graph"]["edges"][0]["source"] == "alpha"
    assert final.json()["graph"]["edges"][0]["target"] == "beta"


@pytest.mark.asyncio
async def test_http_api_keeps_a_dangling_edge_and_missing_endpoint_visible(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = ApiSource(ready(thread("present-id", project)))
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        created = await client.post(
            "/api/graph/edges",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={"source": "historical-id", "target": "present-id", "type": "references"},
        )

    assert created.status_code == 200
    body = created.json()
    assert body["edge"] == body["graph"]["edges"][0]
    assert body["edge"]["source"] == "historical-id"
    assert body["edge"]["target"] == "present-id"
    nodes = {node["id"]: node for node in body["graph"]["nodes"]}
    assert nodes["historical-id"]["missing"] is True
    assert {conversation["id"] for conversation in body["conversations"]} == {
        "historical-id",
        "present-id",
    }


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
async def test_http_refresh_discovers_new_source_conversations_without_creating_edges(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = ApiSource(ready(thread("source-id", project), thread("target-id", project)))
    app = create_app(source=source)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        created = await client.post(
            "/api/graph/edges",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={"source": "source-id", "target": "target-id", "type": "references"},
        )
        source.result = ready(
            thread("source-id", project),
            thread("target-id", project),
            thread("new-id", project),
            thread("archived-id", project, archived=True),
        )
        refreshed = await client.post("/api/refresh")

    assert created.status_code == 200
    assert refreshed.status_code == 200
    body = refreshed.json()
    assert [item["id"] for item in body["conversations"]] == [
        "source-id",
        "target-id",
        "new-id",
        "archived-id",
    ]
    assert {node["id"] for node in body["graph"]["nodes"]} == {
        "source-id",
        "target-id",
        "new-id",
        "archived-id",
    }
    assert body["graph"]["edges"] == [
        {
            "id": created.json()["graph"]["edges"][0]["id"],
            "source": "source-id",
            "target": "target-id",
            "type": "references",
        }
    ]


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
