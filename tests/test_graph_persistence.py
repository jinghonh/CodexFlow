from __future__ import annotations

import fcntl
import errno
import os
from hashlib import sha256
from pathlib import Path

import pytest
import httpx

import codexflow.graph as graph_module
from codexflow.app import create_app
from codexflow.graph import GraphOverlayError, migrate_graph_overlay
from codexflow.project import GraphMigrationOutcome, ProjectGraphService
from codexflow.source import CodexThread, SourceReadResult


def make_thread(thread_id: str, project: Path) -> CodexThread:
    return CodexThread(
        id=thread_id,
        title="Source title",
        preview="Source preview",
        created_at="2024-01-01T00:00:00Z",
        updated_at="2024-01-01T01:00:00Z",
        recency_at="2024-01-01T01:00:00Z",
        cwd=str(project),
        source="cli",
        archived=False,
        history_mode=None,
        status="idle",
        project_id=None,
        git_info=None,
    )


class StubSource:
    def __init__(self, project: Path) -> None:
        self.result = SourceReadResult(
            status="ready",
            threads=(make_thread("stable-id", project),),
            generated_at="2024-01-01T02:00:00Z",
            user_agent="fixture",
        )

    def read_snapshot(self) -> SourceReadResult:
        return self.result


def test_service_reports_graph_busy_when_another_writer_holds_the_sidecar_lock(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))
    before = service.snapshot()

    lock_path = project / ".codex" / "graph.yaml.lock"
    lock_path.parent.mkdir()
    lock_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with pytest.raises(GraphOverlayError) as failure:
            service.update_node_overlay(
                "stable-id",
                {"title": "Local title"},
                expected_etag=before.graph.etag,
            )
    finally:
        fcntl.flock(lock_fd, fcntl.LOCK_UN)
        os.close(lock_fd)

    assert failure.value.code == "graph_busy"
    assert not (project / ".codex" / "graph.yaml").exists()


def test_two_writers_with_the_same_baseline_cannot_silently_lose_an_update(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(project)
    first_writer = ProjectGraphService(source)
    second_writer = ProjectGraphService(source)
    first_writer.select_project(str(project))
    second_writer.select_project(str(project))
    baseline = first_writer.snapshot().graph.etag

    first_writer.update_node_overlay(
        "stable-id",
        {"title": "First writer"},
        expected_etag=baseline,
    )
    with pytest.raises(GraphOverlayError) as failure:
        second_writer.update_node_overlay(
            "stable-id",
            {"title": "Second writer"},
            expected_etag=baseline,
        )

    assert failure.value.code == "graph_conflict"
    assert "First writer" in (project / ".codex" / "graph.yaml").read_text(encoding="utf-8")


def test_service_allows_a_wildcard_etag_only_for_an_explicit_overwrite(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))

    updated = service.update_node_overlay(
        "stable-id",
        {"title": "Explicitly replaced title"},
        expected_etag="*",
        allow_overwrite=True,
    )

    assert updated.conversations[0].overlay.title == "Explicitly replaced title"


def test_migration_creates_a_unique_backup_before_upgrading_a_legacy_overlay(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 0\nnodes:\n  stable-id:\n    title: Legacy title\n"
    graph_path.write_bytes(original)

    migrated = migrate_graph_overlay(
        project,
        expected_etag=sha256(original).hexdigest(),
    )

    backups = sorted(graph_path.parent.glob("graph.yaml.bak.*"))
    assert len(backups) == 1
    assert backups[0].read_bytes() == original
    assert migrated.backup_path == backups[0]
    assert migrated.file_status == "ready"
    assert migrated.nodes["stable-id"].title == "Legacy title"
    assert graph_path.read_text(encoding="utf-8").startswith("version: 1\n")


def test_migration_interface_can_transform_an_unknown_legacy_shape(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 0\nnodes:\n  - id: stable-id\n    caption: Legacy title\n"
    graph_path.write_bytes(original)

    def migrate(document: dict[str, object]) -> dict[str, object]:
        assert document["nodes"] == [{"id": "stable-id", "caption": "Legacy title"}]
        return {
            "version": 1,
            "nodes": {"stable-id": {"title": "Legacy title"}},
        }

    migrated = migrate_graph_overlay(
        project,
        expected_etag=sha256(original).hexdigest(),
        migrator=migrate,
    )

    assert migrated.file_status == "ready"
    assert migrated.nodes["stable-id"].title == "Legacy title"


def test_service_exposes_disk_full_as_a_structured_graph_write_error(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))
    before = service.snapshot()

    def fail_fsync(_: int) -> None:
        raise OSError(errno.ENOSPC, "fixture disk full")

    monkeypatch.setattr("codexflow.graph.os.fsync", fail_fsync)
    with pytest.raises(GraphOverlayError) as failure:
        service.update_node_overlay(
            "stable-id",
            {"title": "Local title"},
            expected_etag=before.graph.etag,
        )

    assert failure.value.code == "graph_write_error"
    assert failure.value.details is not None
    assert failure.value.details["errno"] == errno.ENOSPC
    assert not (project / ".codex" / "graph.yaml").exists()


def test_successful_graph_save_flushes_the_file_and_parent_directory(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))
    before = service.snapshot()
    fsync_calls: list[int] = []

    monkeypatch.setattr(
        "codexflow.graph.os.fsync",
        lambda file_descriptor: fsync_calls.append(file_descriptor),
    )
    service.update_node_overlay(
        "stable-id",
        {"title": "Local title"},
        expected_etag=before.graph.etag,
    )

    assert len(fsync_calls) >= 2
    assert (project / ".codex" / "graph.yaml").exists()
    assert not (project / ".codex" / "graph.yaml.tmp").exists()
    assert not list((project / ".codex").glob(".graph.yaml.tmp"))


@pytest.mark.asyncio
async def test_http_api_requires_an_explicit_overwrite_flag_for_a_wildcard_etag(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=StubSource(project))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        rejected = await client.patch(
            "/api/graph/nodes/stable-id",
            headers={"If-Match": "*"},
            json={"title": "Rejected"},
        )
        accepted = await client.patch(
            "/api/graph/nodes/stable-id?overwrite=true",
            headers={"If-Match": "*"},
            json={"title": "Explicitly overwritten"},
        )

    assert rejected.status_code == 412
    assert rejected.json()["error"]["code"] == "graph_conflict"
    assert accepted.status_code == 200
    assert accepted.json()["conversations"][0]["overlay"]["title"] == "Explicitly overwritten"


def test_service_migrates_a_legacy_overlay_and_returns_the_backup_path(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir()
    original = b"version: 0\nnodes:\n  stable-id:\n    title: Legacy title\n"
    graph_path.write_bytes(original)
    source = StubSource(project)
    service = ProjectGraphService(source)
    service.select_project(str(project))

    outcome = service.migrate_graph(
        expected_etag=sha256(original).hexdigest(),
    )

    assert isinstance(outcome, GraphMigrationOutcome)
    assert outcome.snapshot.graph.file_status == "ready"
    assert outcome.backup_path is not None
    assert outcome.backup_path.read_bytes() == original


@pytest.mark.asyncio
async def test_http_api_migrates_a_legacy_overlay_and_returns_its_backup_path(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 0\nnodes:\n  stable-id:\n    title: Legacy title\n"
    graph_path.write_bytes(original)
    app = create_app(source=StubSource(project))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        selected = await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        migrated = await client.post(
            "/api/graph/migrate",
            headers={"If-Match": initial.json()["graph"]["etag"]},
        )

    assert selected.status_code == 200
    assert initial.status_code == 200
    assert initial.json()["graph"]["fileStatus"] == "legacy"
    assert migrated.status_code == 200
    assert migrated.json()["graph"]["fileStatus"] == "ready"
    assert migrated.json()["backupPath"]
    assert Path(migrated.json()["backupPath"]).read_bytes() == original


def test_service_saves_a_valid_graph_copy_without_replacing_the_current_file(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 1\nnodes:\n  stable-id:\n    title: Existing title\n"
    graph_path.write_bytes(original)
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))

    copy_path = service.save_graph_copy(
        {
            "version": 1,
            "nodes": {"stable-id": {"title": "Draft title"}},
        }
    )

    assert copy_path != graph_path
    assert copy_path.name.startswith("graph.yaml.copy.")
    assert copy_path.read_text(encoding="utf-8").startswith("version: 1\n")
    assert "Draft title" in copy_path.read_text(encoding="utf-8")
    assert graph_path.read_bytes() == original


def test_graph_copy_preserves_unknown_fields_from_the_current_overlay(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    graph_path.write_text(
        """
version: 1
vendorRoot: keep-root
nodes:
  stable-id:
    title: Existing title
    vendorNode: keep-node
edges:
  - id: edge-1
    source: stable-id
    target: other-id
    type: references
    vendorEdge: keep-edge
""",
        encoding="utf-8",
    )
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))

    copy_path = service.save_graph_copy(
        {
            "version": 1,
            "nodes": {"stable-id": {"title": "Draft title"}},
            "edges": [
                {
                    "id": "edge-1",
                    "source": "stable-id",
                    "target": "other-id",
                    "type": "references",
                }
            ],
        }
    )

    copy_text = copy_path.read_text(encoding="utf-8")
    assert "vendorRoot: keep-root" in copy_text
    assert "vendorNode: keep-node" in copy_text
    assert "vendorEdge: keep-edge" in copy_text
    assert "title: Draft title" in copy_text


def test_failed_migration_keeps_the_legacy_file_and_its_backup_recoverable(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 0\nnodes:\n  stable-id:\n    title: Legacy title\n"
    graph_path.write_bytes(original)

    def fail_replace(source_path: str, target_path: Path) -> None:
        raise PermissionError(errno.EACCES, "fixture read-only directory", str(target_path))

    monkeypatch.setattr("codexflow.graph.os.replace", fail_replace)
    with pytest.raises(GraphOverlayError) as failure:
        migrate_graph_overlay(
            project,
            expected_etag=sha256(original).hexdigest(),
        )

    backups = sorted(graph_path.parent.glob("graph.yaml.bak.*"))
    assert failure.value.code == "graph_read_only"
    assert graph_path.read_bytes() == original
    assert len(backups) == 1
    assert backups[0].read_bytes() == original
    assert not list(graph_path.parent.glob(".graph.yaml.tmp.*"))


def test_future_graph_version_is_readable_but_all_mutations_are_read_only(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 99\nnodes:\n  stable-id:\n    title: Future title\n"
    graph_path.write_bytes(original)
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))

    snapshot = service.snapshot()
    with pytest.raises(GraphOverlayError) as failure:
        service.update_node_overlay(
            "stable-id",
            {"title": "Should not save"},
            expected_etag=snapshot.graph.etag,
        )

    assert snapshot.graph.file_status == "future"
    assert failure.value.code == "graph_read_only"
    assert graph_path.read_bytes() == original


def test_future_graph_version_remains_readable_when_its_shape_is_unknown(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 99\nnodes:\n  stable-id: future-node-shape\nedges: future-edge-shape\n"
    graph_path.write_bytes(original)
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))

    snapshot = service.snapshot()

    assert snapshot.graph.file_status == "future"
    assert [conversation.id for conversation in snapshot.conversations] == ["stable-id"]
    assert graph_path.read_bytes() == original


def test_duplicate_yaml_keys_are_rejected_without_touching_the_original_file(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 1\nnodes: {}\nnodes: {}\n"
    graph_path.write_bytes(original)
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))

    with pytest.raises(GraphOverlayError) as failure:
        service.snapshot()

    assert failure.value.code == "graph_parse_error"
    assert graph_path.read_bytes() == original


def test_graph_file_permission_errors_are_observable_at_the_service_boundary(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 1\n"
    graph_path.write_bytes(original)
    service = ProjectGraphService(StubSource(project))
    service.select_project(str(project))

    real_read_bytes = graph_module.Path.read_bytes

    def fail_read_bytes(path: Path) -> bytes:
        if path == graph_path:
            raise PermissionError(errno.EACCES, "fixture permission denied", str(path))
        return real_read_bytes(path)

    monkeypatch.setattr(graph_module.Path, "read_bytes", fail_read_bytes)
    with pytest.raises(GraphOverlayError) as failure:
        service.snapshot()

    assert failure.value.code == "graph_read_only"
    assert failure.value.details is not None
    assert failure.value.details["errno"] == errno.EACCES


@pytest.mark.asyncio
async def test_http_api_saves_a_graph_copy_without_replacing_the_current_file(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    graph_path = project / ".codex" / "graph.yaml"
    graph_path.parent.mkdir(parents=True)
    original = b"version: 1\nnodes:\n  stable-id:\n    title: Existing title\n"
    graph_path.write_bytes(original)
    app = create_app(source=StubSource(project))

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        response = await client.post(
            "/api/graph/copy",
            json={
                "document": {
                    "version": 1,
                    "nodes": {"stable-id": {"title": "Draft title"}},
                }
            },
        )

    assert response.status_code == 200
    copy_path = Path(response.json()["copyPath"])
    assert copy_path.name.startswith("graph.yaml.copy.")
    assert "Draft title" in copy_path.read_text(encoding="utf-8")
    assert graph_path.read_bytes() == original


@pytest.mark.asyncio
async def test_http_api_exposes_a_read_only_graph_write_error(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=StubSource(project))

    def fail_replace(source_path: str, target_path: Path) -> None:
        raise PermissionError(errno.EACCES, "fixture read-only directory", str(target_path))

    monkeypatch.setattr("codexflow.graph.os.replace", fail_replace)
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        response = await client.patch(
            "/api/graph/nodes/stable-id",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={"title": "Local title"},
        )

    assert response.status_code == 423
    assert response.json()["error"]["code"] == "graph_read_only"
    assert response.json()["error"]["details"]["errno"] == errno.EACCES
    assert not (project / ".codex" / "graph.yaml").exists()


@pytest.mark.asyncio
async def test_http_api_exposes_lock_competition_as_graph_busy(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=StubSource(project))
    lock_path = project / ".codex" / "graph.yaml.lock"
    lock_path.parent.mkdir()
    lock_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        async with httpx.AsyncClient(
            transport=httpx.ASGITransport(app=app), base_url="http://test"
        ) as client:
            await client.post("/api/project/select", json={"path": str(project)})
            initial = await client.get("/api/snapshot")
            response = await client.patch(
                "/api/graph/nodes/stable-id",
                headers={"If-Match": initial.json()["graph"]["etag"]},
                json={"title": "Local title"},
            )
    finally:
        fcntl.flock(lock_fd, fcntl.LOCK_UN)
        os.close(lock_fd)

    assert response.status_code == 409
    assert response.json()["error"]["code"] == "graph_busy"


@pytest.mark.asyncio
async def test_http_api_exposes_disk_full_as_a_graph_write_error(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    app = create_app(source=StubSource(project))

    def fail_fsync(_: int) -> None:
        raise OSError(errno.ENOSPC, "fixture disk full")

    monkeypatch.setattr("codexflow.graph.os.fsync", fail_fsync)
    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app), base_url="http://test"
    ) as client:
        await client.post("/api/project/select", json={"path": str(project)})
        initial = await client.get("/api/snapshot")
        response = await client.patch(
            "/api/graph/nodes/stable-id",
            headers={"If-Match": initial.json()["graph"]["etag"]},
            json={"title": "Local title"},
        )

    assert response.status_code == 500
    assert response.json()["error"]["code"] == "graph_write_error"
    assert response.json()["error"]["details"]["errno"] == errno.ENOSPC
