from __future__ import annotations

import subprocess
from dataclasses import replace
from pathlib import Path

import pytest

from codexflow.project import ProjectGraphService, ProjectInvalidError
from codexflow.source import CodexThread, SourceReadResult


def make_thread(
    thread_id: str,
    cwd: Path,
    *,
    title: str | None = None,
    archived: bool = False,
) -> CodexThread:
    return CodexThread(
        id=thread_id,
        title=title,
        preview=f"Preview for {thread_id}",
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


def ready(*threads: CodexThread) -> SourceReadResult:
    return SourceReadResult(
        status="ready",
        threads=tuple(threads),
        generated_at="2024-01-01T02:00:00Z",
        user_agent="Codex Desktop/0.150.1 fixture",
    )


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

    assert [conversation.id for conversation in snapshot.conversations] == [
        "through-real-path",
        "through-alias",
    ]


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
    worktree = tmp_path / "worktree"
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
