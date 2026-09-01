from __future__ import annotations

import subprocess
from dataclasses import replace
from pathlib import Path

import pytest

from codexflow.graph import GraphOverlayError
from codexflow.project import ProjectGraphService, ProjectInvalidError
from codexflow.source import CodexThread, SourceReadResult


def make_thread(
    thread_id: str,
    cwd: Path,
    *,
    title: str | None = None,
    preview: str | None = None,
    archived: bool = False,
) -> CodexThread:
    return CodexThread(
        id=thread_id,
        title=title,
        preview=preview if preview is not None else f"Preview for {thread_id}",
        created_at="2024-01-01T00:00:00Z",
        updated_at="2024-01-01T01:00:00Z",
        recency_at="2024-01-01T01:00:00Z",
        cwd=str(cwd),
        source="cli",
        archived=archived,
        history_mode=None,
        status="idle",
        project_id=None,
        git_info=None,
    )


class StubSource:
    def __init__(self, result: SourceReadResult) -> None:
        self.result = result
        self.read_count = 0

    def read_snapshot(self) -> SourceReadResult:
        self.read_count += 1
        return self.result


class ProjectBoundGitResolver:
    def __init__(self, project: Path) -> None:
        self.project = project.resolve()
        self.paths: list[Path] = []

    def root_for(self, path: Path) -> Path | None:
        resolved = path.resolve()
        self.paths.append(resolved)
        try:
            resolved.relative_to(self.project)
        except ValueError as exc:
            raise AssertionError("git resolution escaped the selected Project") from exc
        return None


class FlakyGitResolver:
    def __init__(self, project: Path) -> None:
        self.project = project.resolve()

    def root_for(self, path: Path) -> Path | None:
        if path.resolve() == self.project:
            return None
        raise TimeoutError("git resolver timed out")


def ready(*threads: CodexThread) -> SourceReadResult:
    return SourceReadResult(
        status="ready",
        threads=tuple(threads),
        generated_at="2024-01-01T02:00:00Z",
        user_agent="Codex Desktop/0.150.1 fixture",
    )


def write_graph(project: Path, content: str) -> None:
    graph_directory = project / ".codex"
    graph_directory.mkdir(exist_ok=True)
    (graph_directory / "graph.yaml").write_text(content, encoding="utf-8")


def test_service_selects_a_real_project_and_filters_threads_by_path(tmp_path: Path) -> None:
    project = tmp_path / "project"
    inside = project / "src"
    outside = tmp_path / "project-other"
    inside.mkdir(parents=True)
    outside.mkdir()
    source = StubSource(
        ready(
            make_thread("inside", inside, title="Inside project"),
            make_thread("root", project),
            make_thread("outside", outside),
        )
    )
    service = ProjectGraphService(source)

    selected = service.select_project(str(project))
    snapshot = service.snapshot()

    assert selected.original_path == str(project)
    assert selected.real_path == str(project.resolve())
    assert selected.is_git_project is False
    assert [conversation.id for conversation in snapshot.conversations] == [
        "inside",
        "root",
    ]
    assert snapshot.conversations[0].display_title == "Inside project"
    assert snapshot.conversations[0].derived.unlinked is True
    assert source.read_count == 1


def test_service_rejects_a_missing_or_non_directory_project(tmp_path: Path) -> None:
    service = ProjectGraphService(StubSource(ready()))
    file_path = tmp_path / "not-a-directory"
    file_path.write_text("fixture", encoding="utf-8")

    with pytest.raises(ProjectInvalidError):
        service.select_project(str(tmp_path / "missing"))
    with pytest.raises(ProjectInvalidError):
        service.select_project(str(file_path))


def test_service_keeps_thread_id_stable_across_refreshes(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(ready(make_thread("stable-id", project, title="First title")))
    service = ProjectGraphService(source)
    service.select_project(str(project))

    first = service.snapshot()
    source.result = ready(make_thread("stable-id", project, title="Updated title"))
    second = service.snapshot()

    assert first.conversations[0].id == "stable-id"
    assert second.conversations[0].id == "stable-id"
    assert second.conversations[0].display_title == "Updated title"


def test_snapshot_uses_source_conversations_as_graph_nodes_without_an_overlay(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    source = StubSource(ready(make_thread("source-id", project, title="Codex title")))
    service = ProjectGraphService(source)

    service.select_project(str(project))
    snapshot = service.snapshot()

    assert snapshot.graph.file_status == "absent"
    assert snapshot.graph.edges == ()
    assert snapshot.graph.nodes == (
        {
            "id": "source-id",
            "displayTitle": "Codex title",
            "missing": False,
            "hidden": False,
            "layout": None,
        },
    )
    assert snapshot.conversations[0].overlay.title is None
    assert snapshot.conversations[0].overlay.hidden is False


def test_snapshot_merges_sparse_overlay_and_keeps_dangling_ids_as_missing_nodes(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    write_graph(
        project,
        """
version: 1
nodes:
  custom-id:
    title: Local title
    tags: [" alpha ", alpha, beta]
    status: active
    note: Keep this context
    hidden: true
    layout:
      x: 320
      y: 180
  missing-id:
    title: Orphaned work
edges:
  - id: edge-1
    source: custom-id
    target: missing-id
    type: continues
    label: historical link
""",
    )
    source = StubSource(
        ready(
            make_thread("custom-id", project, title="Codex title"),
            make_thread("codex-id", project, title="Codex title"),
            make_thread("preview-id", project, preview="Preview headline\nMore detail"),
            make_thread("fallback-id", project, preview=""),
        )
    )
    service = ProjectGraphService(source)

    service.select_project(str(project))
    snapshot = service.snapshot()

    conversations = {conversation.id: conversation for conversation in snapshot.conversations}
    graph_nodes = {node["id"]: node for node in snapshot.graph.nodes}

    assert snapshot.graph.file_status == "ready"
    assert snapshot.graph.etag != "absent"
    assert conversations["custom-id"].overlay.title == "Local title"
    assert conversations["custom-id"].overlay.tags == ("alpha", "beta")
    assert conversations["custom-id"].overlay.hidden is True
    assert conversations["custom-id"].overlay.layout == {"x": 320.0, "y": 180.0}
    assert conversations["custom-id"].display_title == "Local title"
    assert conversations["codex-id"].display_title == "Codex title"
    assert conversations["preview-id"].display_title == "Preview headline"
    assert conversations["fallback-id"].display_title == "fallback-id"

    assert conversations["missing-id"].codex is None
    assert conversations["missing-id"].derived.missing is True
    assert conversations["missing-id"].display_title == "Orphaned work"
    assert graph_nodes["missing-id"]["missing"] is True
    assert graph_nodes["custom-id"] == {
        "id": "custom-id",
        "displayTitle": "Local title",
        "missing": False,
        "hidden": True,
        "layout": {"x": 320.0, "y": 180.0},
    }
    assert "cwd" not in graph_nodes["custom-id"]
    assert "createdAt" not in graph_nodes["custom-id"]
    assert snapshot.graph.edges == (
        {
            "id": "edge-1",
            "source": "custom-id",
            "target": "missing-id",
            "type": "continues",
            "label": "historical link",
        },
    )


def test_graph_nodes_use_the_latest_source_title_after_refresh(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    write_graph(
        project,
        """
version: 1
nodes:
  stable-id:
    tags: [source]
""",
    )
    source = StubSource(ready(make_thread("stable-id", project, title="First source title")))
    service = ProjectGraphService(source)
    service.select_project(str(project))

    first = service.snapshot()
    source.result = ready(make_thread("stable-id", project, title="Latest source title"))
    second = service.snapshot()

    assert first.graph.nodes[0]["displayTitle"] == "First source title"
    assert second.graph.nodes[0]["displayTitle"] == "Latest source title"
    assert second.conversations[0].codex is not None
    assert second.conversations[0].codex.title == "Latest source title"


def test_empty_graph_file_is_rejected_when_version_is_missing(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    write_graph(project, "\n")
    service = ProjectGraphService(StubSource(ready()))
    service.select_project(str(project))

    with pytest.raises(GraphOverlayError) as failure:
        service.snapshot()

    assert failure.value.code == "graph_version_error"


def test_overlay_does_not_promote_a_source_thread_outside_the_selected_project(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "outside"
    project.mkdir()
    outside.mkdir()
    write_graph(
        project,
        """
version: 1
nodes:
  outside-id:
    title: Outside source
  missing-id:
    title: Historical conversation
""",
    )
    source = StubSource(ready(make_thread("inside-id", project), make_thread("outside-id", outside)))
    service = ProjectGraphService(source)
    service.select_project(str(project))

    snapshot = service.snapshot()

    assert [conversation.id for conversation in snapshot.conversations] == [
        "inside-id",
        "missing-id",
    ]
    assert snapshot.conversations[1].derived.missing is True
    assert [node["id"] for node in snapshot.graph.nodes] == ["inside-id", "missing-id"]


def test_service_keeps_invalid_observation_time_in_the_list(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    invalid_thread = replace(make_thread("invalid-time", project), created_at=None)
    service = ProjectGraphService(StubSource(ready(invalid_thread)))
    service.select_project(str(project))

    snapshot = service.snapshot()

    assert [conversation.id for conversation in snapshot.conversations] == ["invalid-time"]
    assert snapshot.conversations[0].derived.valid_observation_range is False


def test_git_project_excludes_a_nested_independent_repository(tmp_path: Path) -> None:
    project = tmp_path / "project"
    regular = project / "src"
    nested = project / "vendor"
    regular.mkdir(parents=True)
    nested.mkdir()
    subprocess.run(["git", "init", "--quiet", str(project)], check=True)
    subprocess.run(["git", "init", "--quiet", str(nested)], check=True)
    source = StubSource(
        ready(
            make_thread("regular", regular),
            make_thread("nested", nested),
        )
    )
    service = ProjectGraphService(source)

    service.select_project(str(project))
    snapshot = service.snapshot()

    assert service.project_view().is_git_project is True
    assert [conversation.id for conversation in snapshot.conversations] == ["regular"]
    assert snapshot.excluded_conversations[0].id == "nested"
    assert snapshot.excluded_conversations[0].reason == "different_git_root"


def test_non_git_project_also_excludes_a_nested_independent_repository(tmp_path: Path) -> None:
    project = tmp_path / "project"
    regular = project / "src"
    nested = project / "vendor"
    regular.mkdir(parents=True)
    nested.mkdir()
    subprocess.run(["git", "init", "--quiet", str(nested)], check=True)
    service = ProjectGraphService(
        StubSource(ready(make_thread("regular", regular), make_thread("nested", nested)))
    )

    selected = service.select_project(str(project))
    snapshot = service.snapshot()

    assert selected.is_git_project is False
    assert [conversation.id for conversation in snapshot.conversations] == ["regular"]
    assert snapshot.excluded_conversations[0].reason == "nested_git_repository"


def test_service_treats_a_symlink_alias_as_the_same_project_and_ignores_broken_cwd(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    alias = tmp_path / "project-alias"
    alias.symlink_to(project, target_is_directory=True)
    broken_cwd = project / "broken-cwd"
    broken_cwd.symlink_to(project / "does-not-exist", target_is_directory=True)
    source = StubSource(
        ready(
            make_thread("through-real-path", project),
            make_thread("through-alias", alias),
            make_thread("broken-cwd", broken_cwd),
        )
    )
    service = ProjectGraphService(source)

    service.select_project(str(alias))
    snapshot = service.snapshot()

    selected = service.project_view()
    assert selected.original_path == str(alias)
    assert selected.real_path == str(project.resolve())
    assert [conversation.id for conversation in snapshot.conversations] == [
        "through-real-path",
        "through-alias",
    ]


def test_snapshot_explains_excluded_source_paths_without_reading_them(tmp_path: Path) -> None:
    project = tmp_path / "project"
    inside = project / "src"
    outside = tmp_path / "project-sibling"
    outside_file = tmp_path / "private.txt"
    escape = project / "escape"
    broken_cwd = project / "broken-cwd"
    escape_target = tmp_path / "escape-target"
    inside.mkdir(parents=True)
    outside.mkdir()
    escape_target.mkdir()
    escape.symlink_to(escape_target, target_is_directory=True)
    outside_file.write_text("private outside content", encoding="utf-8")
    broken_cwd.symlink_to(project / "does-not-exist", target_is_directory=True)
    source = StubSource(
        ready(
            make_thread("inside", inside),
            make_thread("outside", outside),
            make_thread("outside-file", outside_file),
            make_thread("symlink-escape", escape),
            make_thread("broken", broken_cwd),
        )
    )
    service = ProjectGraphService(source)

    service.select_project(str(project))
    snapshot = service.snapshot()

    excluded = {item.id: item for item in snapshot.excluded_conversations}
    assert [conversation.id for conversation in snapshot.conversations] == ["inside"]
    assert excluded["outside"].cwd == str(outside)
    assert excluded["outside"].resolved_cwd == str(outside.resolve())
    assert excluded["outside"].reason == "outside_project"
    assert excluded["outside-file"].resolved_cwd == str(outside_file.resolve())
    assert excluded["outside-file"].reason == "outside_project"
    assert excluded["symlink-escape"].resolved_cwd == str(escape_target.resolve())
    assert excluded["symlink-escape"].reason == "outside_project"
    assert excluded["broken"].cwd == str(broken_cwd)
    assert excluded["broken"].resolved_cwd is None
    assert excluded["broken"].reason == "unresolvable_cwd"
    assert "private outside content" not in str(snapshot.to_dict())


def test_snapshot_never_resolves_git_metadata_for_a_cwd_outside_project(tmp_path: Path) -> None:
    project = tmp_path / "project"
    inside = project / "src"
    outside = tmp_path / "outside"
    inside.mkdir(parents=True)
    outside.mkdir()
    resolver = ProjectBoundGitResolver(project)
    service = ProjectGraphService(
        StubSource(ready(make_thread("inside", inside), make_thread("outside", outside))),
        git_resolver=resolver,
    )

    service.select_project(str(project))
    snapshot = service.snapshot()

    assert [conversation.id for conversation in snapshot.conversations] == ["inside"]
    assert all(path.is_relative_to(project.resolve()) for path in resolver.paths)


def test_git_resolution_failure_does_not_allow_a_cwd_into_an_unknown_repository(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    inside = project / "src"
    inside.mkdir(parents=True)
    resolver = FlakyGitResolver(project)
    service = ProjectGraphService(
        StubSource(ready(make_thread("unknown-git", inside))),
        git_resolver=resolver,
    )

    service.select_project(str(project))
    snapshot = service.snapshot()

    assert snapshot.conversations == ()
    assert snapshot.excluded_conversations[0].reason == "git_root_unresolvable"


def test_service_does_not_interpret_a_relative_cwd_as_a_project_membership(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    project = tmp_path / "project"
    relative_cwd = project / "src"
    relative_cwd.mkdir(parents=True)
    monkeypatch.chdir(project)
    source = StubSource(ready(make_thread("relative-cwd", Path("src"))))
    service = ProjectGraphService(source)

    service.select_project(str(project))
    snapshot = service.snapshot()

    assert snapshot.conversations == ()
    assert snapshot.excluded_conversations[0].reason == "cwd_not_absolute"


def test_service_keeps_a_git_worktree_separate_from_its_main_worktree(tmp_path: Path) -> None:
    repository = tmp_path / "repository"
    repository.mkdir()
    (repository / "README.md").write_text("fixture", encoding="utf-8")
    subprocess.run(["git", "init", "--quiet", str(repository)], check=True)
    subprocess.run(["git", "-C", str(repository), "add", "README.md"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(repository),
            "-c",
            "user.name=CodexFlow Fixture",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
        check=True,
    )
    worktree = repository / "worktree"
    subprocess.run(
        ["git", "-C", str(repository), "worktree", "add", "--quiet", "-b", "fixture-worktree", str(worktree)],
        check=True,
    )
    source = StubSource(
        ready(
            make_thread("main-worktree", repository),
            make_thread("independent-worktree", worktree),
        )
    )
    service = ProjectGraphService(source)

    service.select_project(str(repository))
    snapshot = service.snapshot()

    assert [conversation.id for conversation in snapshot.conversations] == ["main-worktree"]
    assert snapshot.excluded_conversations[0].reason == "different_git_root"

    worktree_service = ProjectGraphService(
        StubSource(
            ready(
                make_thread("main-worktree", repository),
                make_thread("independent-worktree", worktree),
            )
        )
    )
    selected_worktree = worktree_service.select_project(str(worktree))
    worktree_snapshot = worktree_service.snapshot()

    assert selected_worktree.worktree_root == str(worktree.resolve())
    assert [conversation.id for conversation in worktree_snapshot.conversations] == [
        "independent-worktree"
    ]
    assert worktree_snapshot.excluded_conversations[0].reason == "outside_project"
