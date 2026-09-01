from __future__ import annotations

import subprocess
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

from .graph import GraphOverlay, read_graph_overlay, update_graph_node
from .models import (
    Conversation,
    ConversationOverlay,
    DashboardSnapshot,
    DerivedConversationState,
    ExcludedConversation,
    GraphSummary,
    ProjectView,
)
from .source import CodexThread, SourceReadResult


class ProjectInvalidError(Exception):
    def __init__(self, message: str, *, details: dict[str, object] | None = None) -> None:
        super().__init__(message)
        self.message = message
        self.details = details


class GitResolutionError(Exception):
    """Git metadata could not be resolved safely for a path."""


class ProjectNotSelectedError(Exception):
    pass


class SourceUnavailableError(Exception):
    def __init__(self, result: SourceReadResult) -> None:
        super().__init__(result.error.message if result.error else "Codex 来源不可用")
        self.result = result


class GitResolver(Protocol):
    def root_for(self, path: Path) -> Path | None:
        """Return the worktree root containing path, if any."""


class SubprocessGitResolver:
    def root_for(self, path: Path) -> Path | None:
        try:
            completed = subprocess.run(
                ["git", "-C", str(path), "rev-parse", "--show-toplevel"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                timeout=2,
                check=False,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            raise GitResolutionError(f"无法解析 Git 根：{path}") from exc
        if completed.returncode != 0:
            return None
        output = completed.stdout.strip()
        if not output:
            return None
        try:
            return Path(output).resolve(strict=True)
        except (OSError, RuntimeError) as exc:
            raise GitResolutionError(f"无法解析 Git 根：{path}") from exc


@dataclass(frozen=True)
class _ProjectContext:
    original_path: str
    real_path: Path
    git_root: Path | None
    worktree_root: Path | None


class ProjectGraphService:
    def __init__(self, source: object, *, git_resolver: GitResolver | None = None) -> None:
        self._source = source
        self._git_resolver = git_resolver or SubprocessGitResolver()
        self._project: _ProjectContext | None = None
        self._graph_file_status = "not_loaded"

    def select_project(self, path: str) -> ProjectView:
        if not isinstance(path, str) or not path.strip():
            raise ProjectInvalidError("Project 路径不能为空")
        original_path = path
        try:
            candidate = Path(path).expanduser()
            real_path = candidate.resolve(strict=True)
        except (OSError, RuntimeError, ValueError) as exc:
            raise ProjectInvalidError("Project 路径不存在或无法解析", details={"path": path}) from exc
        if not real_path.is_dir():
            raise ProjectInvalidError("Project 路径必须是目录", details={"path": path})
        try:
            git_root = self._resolve_git_root(real_path)
        except GitResolutionError as exc:
            raise ProjectInvalidError(
                "Project 的 Git 根无法解析",
                details={"path": str(real_path), "error": str(exc)},
            ) from exc
        self._project = _ProjectContext(
            original_path=original_path,
            real_path=real_path,
            git_root=git_root,
            worktree_root=git_root,
        )
        self._graph_file_status = "not_loaded"
        return self.project_view()

    def project_view(self) -> ProjectView:
        project = self._require_project()
        graph_path = project.real_path / ".codex" / "graph.yaml"
        return ProjectView(
            original_path=project.original_path,
            real_path=str(project.real_path),
            git_root=str(project.git_root) if project.git_root else None,
            worktree_root=str(project.worktree_root) if project.worktree_root else None,
            is_git_project=project.git_root is not None,
            graph_file=str(graph_path),
            graph_file_status=self._graph_file_status,
        )

    def snapshot(self) -> DashboardSnapshot:
        project = self._require_project()
        result = self._source.read_snapshot()
        if result.status in {"unavailable", "incompatible"}:
            raise SourceUnavailableError(result)
        graph_overlay = read_graph_overlay(project.real_path)
        self._graph_file_status = graph_overlay.file_status
        return self._build_snapshot(result, graph_overlay, project)

    def update_node_overlay(
        self,
        conversation_id: str,
        changes: Mapping[str, object],
        *,
        expected_etag: str,
    ) -> DashboardSnapshot:
        project = self._require_project()
        result = self._source.read_snapshot()
        if result.status in {"unavailable", "incompatible"}:
            raise SourceUnavailableError(result)
        graph_overlay = update_graph_node(
            project.real_path,
            conversation_id,
            changes,
            expected_etag=expected_etag,
        )
        self._graph_file_status = graph_overlay.file_status
        return self._build_snapshot(result, graph_overlay, project)

    def _build_snapshot(
        self,
        result: SourceReadResult,
        graph_overlay: GraphOverlay,
        project: _ProjectContext,
    ) -> DashboardSnapshot:
        included_threads, excluded_conversations = self._partition_threads(
            result.threads, project
        )
        conversations = _merge_conversations(
            included_threads,
            graph_overlay,
            source_ids={thread.id for thread in result.threads},
            source_available=result.has_complete_snapshot,
        )
        return DashboardSnapshot(
            project=self.project_view(),
            source_status=result.status,
            generated_at=result.generated_at,
            user_agent=result.user_agent,
            source_error=_source_error(result),
            graph=GraphSummary(
                etag=graph_overlay.etag,
                file_status=graph_overlay.file_status,
                edges=graph_overlay.edges,
                nodes=tuple(_graph_node(conversation) for conversation in conversations),
            ),
            conversations=tuple(conversations),
            excluded_conversations=tuple(excluded_conversations),
        )

    def _partition_threads(
        self,
        threads: tuple[CodexThread, ...],
        project: _ProjectContext,
    ) -> tuple[list[CodexThread], list[ExcludedConversation]]:
        included: list[CodexThread] = []
        excluded: list[ExcludedConversation] = []
        seen_ids: set[str] = set()
        for thread in threads:
            if thread.id in seen_ids:
                continue
            seen_ids.add(thread.id)
            belongs, explanation = self._classify_thread(thread, project)
            if belongs:
                included.append(thread)
            elif explanation is not None:
                excluded.append(explanation)
        return included, excluded

    def _classify_thread(
        self,
        thread: CodexThread,
        project: _ProjectContext,
    ) -> tuple[bool, ExcludedConversation | None]:
        raw_cwd = Path(thread.cwd)
        if not raw_cwd.is_absolute():
            return False, _excluded_conversation(thread, reason="cwd_not_absolute")
        try:
            cwd = raw_cwd.resolve(strict=True)
        except (OSError, RuntimeError, ValueError):
            return False, _excluded_conversation(thread, reason="unresolvable_cwd")
        if not _is_relative_to(cwd, project.real_path):
            return False, _excluded_conversation(
                thread,
                resolved_cwd=cwd,
                reason="outside_project",
            )
        if not cwd.is_dir():
            return False, _excluded_conversation(
                thread,
                resolved_cwd=cwd,
                reason="cwd_not_directory",
            )

        try:
            cwd_git_root = self._resolve_git_root(cwd)
        except GitResolutionError:
            return False, _excluded_conversation(
                thread,
                resolved_cwd=cwd,
                reason="git_root_unresolvable",
            )
        if project.git_root is not None:
            if cwd_git_root == project.worktree_root:
                return True, None
            return False, _excluded_conversation(
                thread,
                resolved_cwd=cwd,
                git_root=cwd_git_root,
                worktree_root=cwd_git_root,
                reason="different_git_root",
            )
        if cwd_git_root is None:
            return True, None
        return False, _excluded_conversation(
            thread,
            resolved_cwd=cwd,
            git_root=cwd_git_root,
            worktree_root=cwd_git_root,
            reason="nested_git_repository",
        )

    def _resolve_git_root(self, path: Path) -> Path | None:
        try:
            return self._git_resolver.root_for(path)
        except GitResolutionError:
            raise
        except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
            raise GitResolutionError(f"无法解析 Git 根：{path}") from exc

    def _require_project(self) -> _ProjectContext:
        if self._project is None:
            raise ProjectNotSelectedError("尚未选择 Project")
        return self._project


def _merge_conversations(
    threads: list[CodexThread],
    graph_overlay: GraphOverlay,
    *,
    source_ids: set[str],
    source_available: bool,
) -> list[Conversation]:
    thread_by_id = {thread.id: thread for thread in threads}
    conversation_ids = list(thread_by_id)
    referenced_ids = list(graph_overlay.nodes)
    for edge in graph_overlay.edges:
        referenced_ids.extend((edge["source"], edge["target"]))
    for conversation_id in referenced_ids:
        if conversation_id not in source_ids and conversation_id not in conversation_ids:
            conversation_ids.append(conversation_id)

    connected_ids = {
        endpoint
        for edge in graph_overlay.edges
        for endpoint in (edge["source"], edge["target"])
    }

    conversations: list[Conversation] = []
    for conversation_id in conversation_ids:
        thread = thread_by_id.get(conversation_id)
        overlay = graph_overlay.nodes.get(conversation_id, ConversationOverlay())
        missing = source_available and conversation_id not in source_ids
        valid_range = bool(
            thread
            and thread.created_at is not None
            and thread.updated_at is not None
            and thread.created_at <= thread.updated_at
        )
        conversations.append(
            Conversation(
                id=conversation_id,
                codex=thread,
                overlay=overlay,
                derived=DerivedConversationState(
                    missing=missing,
                    unlinked=conversation_id not in connected_ids,
                    source_available=source_available,
                    valid_observation_range=valid_range,
                ),
                display_title=_display_title(conversation_id, thread, overlay),
            )
        )
    return conversations


def _display_title(
    conversation_id: str,
    thread: CodexThread | None,
    overlay: ConversationOverlay,
) -> str:
    if overlay.title:
        return overlay.title
    if thread and thread.title and thread.title.strip():
        return thread.title
    if thread and thread.preview:
        first_line = thread.preview.splitlines()[0].strip()
        if first_line:
            return first_line
    return conversation_id


def _source_error(result: SourceReadResult) -> dict[str, object] | None:
    if result.error is None:
        return None
    return {
        "code": result.error.code,
        "message": result.error.message,
        "details": result.error.details,
        "retryable": result.error.retryable,
    }


def _graph_node(conversation: Conversation) -> dict[str, object]:
    return {
        "id": conversation.id,
        "displayTitle": conversation.display_title,
        "missing": conversation.derived.missing,
        "hidden": conversation.overlay.hidden,
        "layout": conversation.overlay.layout,
    }


def _excluded_conversation(
    thread: CodexThread,
    *,
    reason: str,
    resolved_cwd: Path | None = None,
    git_root: Path | None = None,
    worktree_root: Path | None = None,
) -> ExcludedConversation:
    return ExcludedConversation(
        id=thread.id,
        cwd=thread.cwd,
        resolved_cwd=str(resolved_cwd) if resolved_cwd is not None else None,
        git_root=str(git_root) if git_root is not None else None,
        worktree_root=str(worktree_root) if worktree_root is not None else None,
        reason=reason,
    )


def _is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True
