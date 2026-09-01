from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from .source import CodexThread


@dataclass(frozen=True)
class ConversationOverlay:
    title: str | None = None
    tags: tuple[str, ...] = ()
    status: str = "none"
    note: str | None = None
    hidden: bool = False
    layout: dict[str, float] | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "title": self.title,
            "tags": list(self.tags),
            "status": self.status,
            "note": self.note,
            "hidden": self.hidden,
            "layout": self.layout,
        }


@dataclass(frozen=True)
class DerivedConversationState:
    missing: bool
    unlinked: bool
    source_available: bool
    valid_observation_range: bool

    def to_dict(self) -> dict[str, bool]:
        return {
            "missing": self.missing,
            "unlinked": self.unlinked,
            "sourceAvailable": self.source_available,
            "validObservationRange": self.valid_observation_range,
        }


@dataclass(frozen=True)
class Conversation:
    id: str
    codex: CodexThread | None
    overlay: ConversationOverlay
    derived: DerivedConversationState
    display_title: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "codex": _codex_to_dict(self.codex),
            "overlay": self.overlay.to_dict(),
            "derived": self.derived.to_dict(),
            "displayTitle": self.display_title,
        }


@dataclass(frozen=True)
class GraphSummary:
    etag: str = "absent"
    file_status: str = "not_loaded"
    edges: tuple[dict[str, Any], ...] = ()


@dataclass(frozen=True)
class ProjectView:
    original_path: str
    real_path: str
    git_root: str | None
    worktree_root: str | None
    is_git_project: bool
    graph_file: str
    graph_file_status: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "originalPath": self.original_path,
            "realPath": self.real_path,
            "gitRoot": self.git_root,
            "worktreeRoot": self.worktree_root,
            "isGitProject": self.is_git_project,
            "graphFile": self.graph_file,
            "graphFileStatus": self.graph_file_status,
        }


@dataclass(frozen=True)
class DashboardSnapshot:
    project: ProjectView
    source_status: str
    generated_at: str | None
    user_agent: str | None
    source_error: dict[str, Any] | None
    graph: GraphSummary
    conversations: tuple[Conversation, ...]

    def to_dict(self) -> dict[str, Any]:
        return {
            "project": self.project.to_dict(),
            "source": {
                "status": self.source_status,
                "generatedAt": self.generated_at,
                "userAgent": self.user_agent,
                "error": self.source_error,
            },
            "graph": {
                "etag": self.graph.etag,
                "fileStatus": self.graph.file_status,
                "edges": list(self.graph.edges),
            },
            "conversations": [conversation.to_dict() for conversation in self.conversations],
        }


def _codex_to_dict(thread: CodexThread | None) -> dict[str, Any]:
    if thread is None:
        return {
            "title": None,
            "preview": "",
            "createdAt": None,
            "updatedAt": None,
            "recencyAt": None,
            "cwd": "",
            "source": "unknown",
            "archived": False,
            "historyMode": None,
            "status": "unknown",
            "projectId": None,
            "gitInfo": None,
        }
    return {
        "title": thread.title,
        "preview": thread.preview,
        "createdAt": thread.created_at,
        "updatedAt": thread.updated_at,
        "recencyAt": thread.recency_at,
        "cwd": thread.cwd,
        "source": thread.source,
        "archived": thread.archived,
        "historyMode": thread.history_mode,
        "status": thread.status,
        "projectId": thread.project_id,
        "gitInfo": thread.git_info,
    }
