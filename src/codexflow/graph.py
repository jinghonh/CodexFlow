from __future__ import annotations

import errno
import fcntl
import math
import os
import tempfile
import uuid
from collections.abc import Callable, Mapping
from contextlib import contextmanager
from copy import deepcopy
from dataclasses import dataclass, replace
from datetime import UTC, datetime
from hashlib import sha256
from pathlib import Path
from typing import Any

import yaml

from .models import ConversationOverlay

CURRENT_GRAPH_VERSION = 1
LEGACY_GRAPH_VERSION = 0
BUILT_IN_EDGE_TYPES = {
    "continues",
    "depends_on",
    "implements",
    "reviewed_by",
    "fixes",
    "references",
    "related_to",
}
USER_STATUSES = {"none", "active", "done", "blocked"}
GRAPH_NODE_FIELDS = ("title", "tags", "status", "note", "hidden", "layout")


class GraphOverlayError(Exception):
    def __init__(
        self,
        code: str,
        message: str,
        *,
        details: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.details = details


@dataclass(frozen=True)
class GraphOverlay:
    nodes: dict[str, ConversationOverlay]
    edges: tuple[dict[str, Any], ...]
    etag: str
    file_status: str
    raw_document: dict[str, Any] | None = None
    backup_path: Path | None = None


class _UniqueKeyLoader(yaml.SafeLoader):
    pass


def _construct_unique_mapping(
    loader: _UniqueKeyLoader,
    node: yaml.MappingNode,
    deep: bool = False,
) -> dict[Any, Any]:
    mapping: dict[Any, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                f"found duplicate key {key!r}",
                key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


_UniqueKeyLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,
    _construct_unique_mapping,
)


def _raise_graph_write_error(exc: OSError) -> None:
    code = (
        "graph_read_only"
        if exc.errno in {errno.EACCES, errno.EPERM, errno.EROFS}
        else "graph_write_error"
    )
    if code == "graph_read_only":
        message = "Graph overlay 当前不可写，请检查 Project 的写入权限后重试"
    elif exc.errno == errno.ENOSPC:
        message = "无法保存 Graph overlay：磁盘空间不足，请释放空间后重试"
    else:
        message = "无法保存 Graph overlay，请检查 Project 的写入权限后重试"
    raise GraphOverlayError(
        code,
        message,
        details={
            "errno": exc.errno,
            "errnoName": errno.errorcode.get(exc.errno),
            "error": str(exc),
        },
    ) from exc


def _raise_graph_read_error(exc: OSError) -> None:
    code = "graph_read_only" if exc.errno in {errno.EACCES, errno.EPERM, errno.EROFS} else "graph_parse_error"
    message = (
        "无法读取 Graph overlay，请检查文件权限"
        if code == "graph_read_only"
        else "无法读取 Graph overlay"
    )
    raise GraphOverlayError(
        code,
        message,
        details={
            "errno": exc.errno,
            "errnoName": errno.errorcode.get(exc.errno),
            "error": str(exc),
        },
    ) from exc


def read_graph_overlay(project_root: Path) -> GraphOverlay:
    """Read the Project-local sparse Graph overlay without changing it."""

    graph_path = project_root / ".codex" / "graph.yaml"
    try:
        resolved_graph_path = graph_path.resolve(strict=False)
        resolved_root = project_root.resolve(strict=True)
    except (OSError, RuntimeError) as exc:
        raise GraphOverlayError("graph_parse_error", "无法解析 Graph overlay 路径") from exc
    if not _is_relative_to(resolved_graph_path, resolved_root):
        raise GraphOverlayError(
            "graph_parse_error",
            "Graph overlay 路径超出 Project 根目录",
        )

    try:
        graph_exists = graph_path.exists()
        graph_is_file = graph_path.is_file() if graph_exists else False
    except OSError as exc:
        _raise_graph_read_error(exc)
    if not graph_exists:
        return GraphOverlay(nodes={}, edges=(), etag="absent", file_status="absent")
    if not graph_is_file:
        raise GraphOverlayError("graph_parse_error", "Graph overlay 不是文件")

    try:
        raw_bytes = graph_path.read_bytes()
    except OSError as exc:
        _raise_graph_read_error(exc)
    etag = sha256(raw_bytes).hexdigest()
    try:
        document = yaml.load(raw_bytes, Loader=_UniqueKeyLoader)
    except (yaml.YAMLError, TypeError) as exc:
        raise GraphOverlayError("graph_parse_error", "Graph overlay YAML 无法解析") from exc
    if document is None:
        document = {}
    if not isinstance(document, dict):
        raise GraphOverlayError("graph_schema_error", "Graph overlay 根节点必须是映射")

    version = document.get("version")
    if isinstance(version, bool) or not isinstance(version, int) or version < LEGACY_GRAPH_VERSION:
        raise GraphOverlayError("graph_version_error", "Graph overlay version 无法识别")
    file_status = (
        "future"
        if version > CURRENT_GRAPH_VERSION
        else "legacy"
        if version < CURRENT_GRAPH_VERSION
        else "ready"
    )

    if file_status in {"future", "legacy"}:
        try:
            nodes = _parse_nodes(document.get("nodes", {}))
        except GraphOverlayError:
            nodes = {}
        try:
            edges = _parse_edges(document.get("edges", []))
        except GraphOverlayError:
            edges = ()
    else:
        _validate_project(document.get("project"))
        nodes = _parse_nodes(document.get("nodes", {}))
        edges = _parse_edges(document.get("edges", []))
    return GraphOverlay(
        nodes=nodes,
        edges=edges,
        etag=etag,
        file_status=file_status,
        raw_document=document,
    )


def migrate_graph_overlay(
    project_root: Path,
    *,
    expected_etag: str,
    allow_overwrite: bool = False,
    migrator: Callable[[dict[str, Any]], Mapping[str, Any]] | None = None,
) -> GraphOverlay:
    """Migrate a legacy overlay after backing up its exact original bytes."""

    with _graph_lock(project_root):
        overlay = read_graph_overlay(project_root)
        if overlay.file_status == "future":
            raise GraphOverlayError(
                "graph_read_only",
                "Graph overlay 属于更高版本，只能只读打开",
            )
        if overlay.file_status == "ready":
            return overlay
        _check_graph_etag(overlay, expected_etag, allow_overwrite=allow_overwrite)
        if overlay.raw_document is None:
            raise GraphOverlayError("graph_version_error", "Graph overlay 缺少可迁移内容")

        source_document = deepcopy(overlay.raw_document)
        try:
            migrated_value = (
                migrator(source_document)
                if migrator is not None
                else {**source_document, "version": CURRENT_GRAPH_VERSION}
            )
        except GraphOverlayError:
            raise
        except Exception as exc:
            raise GraphOverlayError(
                "graph_schema_error",
                "Graph overlay 迁移失败，原文件未修改",
                details={"error": str(exc)},
            ) from exc
        if not isinstance(migrated_value, Mapping):
            raise GraphOverlayError(
                "graph_schema_error",
                "Graph overlay 迁移结果必须是映射，原文件未修改",
            )
        migrated_document = deepcopy(dict(migrated_value))
        _validate_current_document(migrated_document)
        _serialize_graph_document(migrated_document)

        graph_path, _ = _graph_paths(project_root)
        try:
            original_bytes = graph_path.read_bytes()
        except OSError as exc:
            _raise_graph_write_error(exc)
        if sha256(original_bytes).hexdigest() != overlay.etag:
            raise GraphOverlayError(
                "graph_conflict",
                "Graph overlay 已被其他修改更新，请重新加载后再迁移",
                details={"expectedEtag": overlay.etag, "currentEtag": sha256(original_bytes).hexdigest()},
            )
        backup_path = _create_graph_backup(project_root, original_bytes)
        _write_graph_document(
            project_root,
            _ordered_document(migrated_document),
            lock_held=True,
        )
        migrated = read_graph_overlay(project_root)
        return replace(migrated, backup_path=backup_path)


def _check_graph_etag(
    overlay: GraphOverlay,
    expected_etag: str,
    *,
    allow_overwrite: bool = False,
) -> None:
    if expected_etag == "*" and allow_overwrite:
        return
    if expected_etag == "*" or expected_etag != overlay.etag:
        raise GraphOverlayError(
            "graph_conflict",
            "Graph overlay 已被其他修改更新，请重新加载后再保存",
            details={"expectedEtag": expected_etag, "currentEtag": overlay.etag},
        )


def _validate_current_document(document: Mapping[str, Any]) -> None:
    version = document.get("version")
    if isinstance(version, bool) or version != CURRENT_GRAPH_VERSION:
        raise GraphOverlayError(
            "graph_version_error",
            "Graph overlay 必须是当前版本",
        )
    _validate_project(document.get("project"))
    _parse_nodes(document.get("nodes", {}))
    _parse_edges(document.get("edges", []))


def _serialize_graph_document(document: Mapping[str, Any]) -> bytes:
    try:
        return yaml.safe_dump(
            dict(document),
            allow_unicode=True,
            sort_keys=False,
        ).encode("utf-8")
    except (yaml.YAMLError, TypeError, ValueError) as exc:
        raise GraphOverlayError(
            "graph_write_error",
            "无法序列化 Graph overlay，原文件未修改",
            details={"error": str(exc)},
        ) from exc


def _write_exclusive_file(path: Path, contents: bytes) -> None:
    file_descriptor: int | None = None
    try:
        file_descriptor = os.open(
            path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL,
            0o600,
        )
    except FileExistsError:
        raise
    try:
        with os.fdopen(file_descriptor, "wb") as output_file:
            file_descriptor = None
            output_file.write(contents)
            output_file.flush()
            os.fsync(output_file.fileno())
    except (OSError, ValueError):
        if file_descriptor is not None:
            try:
                os.close(file_descriptor)
            except OSError:
                pass
        try:
            path.unlink()
        except OSError:
            pass
        raise


def _create_graph_backup(project_root: Path, original_bytes: bytes) -> Path:
    graph_path, _ = _graph_paths(project_root)
    timestamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    for _ in range(100):
        backup_path = graph_path.with_name(
            f"graph.yaml.bak.{timestamp}.{uuid.uuid4().hex}"
        )
        try:
            _write_exclusive_file(backup_path, original_bytes)
            return backup_path
        except FileExistsError:
            continue
        except OSError as exc:
            _raise_graph_write_error(exc)
    raise GraphOverlayError(
        "graph_write_error",
        "无法创建不覆盖已有文件的 Graph overlay 备份",
    )


def save_graph_copy(project_root: Path, document: Mapping[str, Any]) -> Path:
    """Save a validated Graph overlay document beside graph.yaml without replacing it."""

    if not isinstance(document, Mapping):
        raise GraphOverlayError("graph_schema_error", "Graph overlay 副本必须是映射")
    copy_document = _copy_document_with_preserved_fields(
        project_root,
        deepcopy(dict(document)),
    )
    _validate_current_document(copy_document)
    serialized = _serialize_graph_document(copy_document)
    graph_path, _ = _prepare_graph_directory(project_root)
    timestamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")

    for _ in range(100):
        copy_path = graph_path.with_name(
            f"graph.yaml.copy.{timestamp}.{uuid.uuid4().hex}"
        )
        try:
            _write_exclusive_file(copy_path, serialized)
            try:
                _fsync_directory(copy_path.parent)
            except OSError:
                pass
            return copy_path
        except FileExistsError:
            continue
        except OSError as exc:
            _raise_graph_write_error(exc)
    raise GraphOverlayError(
        "graph_write_error",
        "无法创建不覆盖已有文件的 Graph overlay 副本",
    )


def _copy_document_with_preserved_fields(
    project_root: Path,
    requested: dict[str, Any],
) -> dict[str, Any]:
    try:
        current = read_graph_overlay(project_root)
    except GraphOverlayError:
        return requested
    if current.raw_document is None:
        return requested

    merged = deepcopy(current.raw_document)
    for key, value in requested.items():
        if key not in {"project", "nodes", "edges"}:
            merged[key] = value

    if "project" in requested and isinstance(requested["project"], dict):
        current_project = current.raw_document.get("project")
        if isinstance(current_project, dict):
            merged["project"] = {**deepcopy(current_project), **requested["project"]}
        else:
            merged["project"] = requested["project"]

    if "nodes" in requested:
        merged["nodes"] = _merge_copy_nodes(current.raw_document.get("nodes"), requested["nodes"])
    if "edges" in requested:
        merged["edges"] = _merge_copy_edges(current.raw_document.get("edges"), requested["edges"])
    return merged


def _merge_copy_nodes(current_nodes: Any, requested_nodes: Any) -> Any:
    if not isinstance(current_nodes, dict) or not isinstance(requested_nodes, dict):
        return requested_nodes
    merged_nodes: dict[Any, Any] = {}
    for conversation_id, requested_node in requested_nodes.items():
        if not isinstance(requested_node, dict):
            merged_nodes[conversation_id] = requested_node
            continue
        current_node = current_nodes.get(conversation_id)
        if isinstance(current_node, dict):
            preserved = deepcopy(current_node)
            for field in GRAPH_NODE_FIELDS:
                preserved.pop(field, None)
            preserved.update(deepcopy(requested_node))
            merged_nodes[conversation_id] = preserved
        else:
            merged_nodes[conversation_id] = deepcopy(requested_node)
    return merged_nodes


def _merge_copy_edges(current_edges: Any, requested_edges: Any) -> Any:
    if not isinstance(current_edges, list) or not isinstance(requested_edges, list):
        return requested_edges
    current_by_id = {
        edge.get("id"): edge
        for edge in current_edges
        if isinstance(edge, dict) and isinstance(edge.get("id"), str)
    }
    known_fields = {"id", "source", "target", "type", "label"}
    merged_edges: list[Any] = []
    for requested_edge in requested_edges:
        if not isinstance(requested_edge, dict):
            merged_edges.append(requested_edge)
            continue
        edge_id = requested_edge.get("id")
        current_edge = current_by_id.get(edge_id)
        if isinstance(current_edge, dict):
            preserved = {
                key: deepcopy(value)
                for key, value in current_edge.items()
                if key not in known_fields
            }
            preserved.update(deepcopy(requested_edge))
            merged_edges.append(preserved)
        else:
            merged_edges.append(deepcopy(requested_edge))
    return merged_edges


def _graph_paths(project_root: Path) -> tuple[Path, Path]:
    graph_path = project_root / ".codex" / "graph.yaml"
    lock_path = project_root / ".codex" / "graph.yaml.lock"
    try:
        resolved_root = project_root.resolve(strict=True)
        resolved_graph_path = graph_path.resolve(strict=False)
        resolved_lock_path = lock_path.resolve(strict=False)
    except (OSError, RuntimeError) as exc:
        raise GraphOverlayError("graph_write_error", "无法解析 Graph overlay 路径") from exc
    if not _is_relative_to(resolved_graph_path, resolved_root):
        raise GraphOverlayError(
            "graph_write_error",
            "Graph overlay 路径超出 Project 根目录",
        )
    if not _is_relative_to(resolved_lock_path, resolved_root):
        raise GraphOverlayError(
            "graph_write_error",
            "Graph overlay 锁路径超出 Project 根目录",
        )
    return graph_path, lock_path


def _prepare_graph_directory(project_root: Path) -> tuple[Path, Path]:
    graph_path, lock_path = _graph_paths(project_root)
    try:
        graph_path.parent.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        _raise_graph_write_error(exc)
    return graph_path, lock_path


@contextmanager
def _graph_lock(project_root: Path):
    _, lock_path = _prepare_graph_directory(project_root)
    lock_fd: int | None = None
    acquired = False
    try:
        lock_fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    except OSError as exc:
        _raise_graph_write_error(exc)
    try:
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            acquired = True
        except OSError as exc:
            if exc.errno in {errno.EACCES, errno.EAGAIN}:
                raise GraphOverlayError(
                    "graph_busy",
                    "Graph overlay 正在被其他写入者保存，请稍后重试",
                    details={"lockPath": str(lock_path)},
                ) from exc
            _raise_graph_write_error(exc)
        yield
    finally:
        if acquired and lock_fd is not None:
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_UN)
            except OSError:
                pass
        if lock_fd is not None:
            try:
                os.close(lock_fd)
            except OSError:
                pass
            finally:
                lock_fd = None


def _mutate_graph_document(
    project_root: Path,
    expected_etag: str,
    build_document: Callable[[GraphOverlay], dict[str, Any]],
    *,
    allow_overwrite: bool = False,
) -> GraphOverlay:
    with _graph_lock(project_root):
        overlay = read_graph_overlay(project_root)
        _check_graph_mutation_etag(
            overlay,
            expected_etag,
            allow_overwrite=allow_overwrite,
        )
        document = build_document(overlay)
        _write_graph_document(project_root, document, lock_held=True)
        return read_graph_overlay(project_root)


def update_graph_node(
    project_root: Path,
    conversation_id: str,
    changes: Mapping[str, Any],
    *,
    expected_etag: str,
    allow_overwrite: bool = False,
) -> GraphOverlay:
    """Persist one sparse node overlay and return the validated new document."""

    def build_document(overlay: GraphOverlay) -> dict[str, Any]:
        if not isinstance(conversation_id, str) or not conversation_id.strip():
            raise GraphOverlayError("graph_schema_error", "Graph 节点 ID 必须是非空字符串")
        if not isinstance(changes, Mapping):
            raise GraphOverlayError("graph_schema_error", "节点更新必须是映射")

        allowed_fields = set(GRAPH_NODE_FIELDS)
        unknown_fields = set(changes) - allowed_fields
        if unknown_fields:
            unknown = ", ".join(sorted(str(field) for field in unknown_fields))
            raise GraphOverlayError("graph_schema_error", f"节点更新包含未知字段：{unknown}")

        current = overlay.nodes.get(conversation_id, ConversationOverlay())
        candidate = {
            "title": current.title,
            "tags": list(current.tags),
            "status": current.status,
            "note": current.note,
            "hidden": current.hidden,
            "layout": deepcopy(current.layout),
        }
        candidate.update(changes)
        if candidate["tags"] is None:
            candidate["tags"] = []
        normalized = _parse_node(conversation_id, candidate)
        return _document_for_update(overlay, conversation_id, normalized)

    return _mutate_graph_document(
        project_root,
        expected_etag,
        build_document,
        allow_overwrite=allow_overwrite,
    )


def create_graph_edge(
    project_root: Path,
    source: str,
    target: str,
    edge_type: str,
    label: str | None = None,
    *,
    expected_etag: str,
    allow_overwrite: bool = False,
) -> GraphOverlay:
    """Persist one artificial relationship and return the validated overlay."""

    def build_document(overlay: GraphOverlay) -> dict[str, Any]:
        edge: dict[str, Any] = {
            "id": uuid.uuid4().hex,
            "source": source,
            "target": target,
            "type": edge_type,
        }
        if label is not None:
            edge["label"] = label

        document = deepcopy(overlay.raw_document) if overlay.raw_document is not None else {}
        document["version"] = CURRENT_GRAPH_VERSION
        raw_edges = document.get("edges", [])
        if not isinstance(raw_edges, list):
            raise GraphOverlayError("graph_schema_error", "Graph overlay edges 必须是列表")
        candidate_edges = [*raw_edges, edge]
        normalized_edges = _parse_edges(candidate_edges, mutation=True)
        candidate_edges[-1] = normalized_edges[-1]
        document["edges"] = candidate_edges
        return _ordered_document(document)

    return _mutate_graph_document(
        project_root,
        expected_etag,
        build_document,
        allow_overwrite=allow_overwrite,
    )


def update_graph_edge(
    project_root: Path,
    edge_id: str,
    changes: Mapping[str, Any],
    *,
    expected_etag: str,
    allow_overwrite: bool = False,
) -> GraphOverlay:
    """Persist changes to one artificial relationship without changing its ID."""

    def build_document(overlay: GraphOverlay) -> dict[str, Any]:
        if not isinstance(edge_id, str) or not edge_id.strip():
            raise GraphOverlayError("graph_schema_error", "Graph edge id 必须是非空字符串")
        if not isinstance(changes, Mapping):
            raise GraphOverlayError("graph_schema_error", "Graph edge 更新必须是映射")
        allowed_fields = {"source", "target", "type", "label"}
        unknown_fields = set(changes) - allowed_fields
        if unknown_fields:
            unknown = ", ".join(sorted(str(field) for field in unknown_fields))
            raise GraphOverlayError("graph_schema_error", f"Graph edge 更新包含未知字段：{unknown}")
        if not changes:
            raise GraphOverlayError("graph_schema_error", "至少需要修改一个 Graph edge 字段")

        document = deepcopy(overlay.raw_document) if overlay.raw_document is not None else {}
        document["version"] = CURRENT_GRAPH_VERSION
        raw_edges = document.get("edges", [])
        if not isinstance(raw_edges, list):
            raise GraphOverlayError("graph_schema_error", "Graph overlay edges 必须是列表")
        edge_index = next(
            (
                index
                for index, raw_edge in enumerate(raw_edges)
                if isinstance(raw_edge, dict) and raw_edge.get("id") == edge_id
            ),
            None,
        )
        if edge_index is None:
            raise GraphOverlayError("edge_not_found", f"找不到 Graph edge：{edge_id}")

        candidate = deepcopy(raw_edges[edge_index])
        for field in ("source", "target", "type"):
            if field in changes:
                candidate[field] = changes[field]
        if "label" in changes:
            if changes["label"] is None:
                candidate.pop("label", None)
            else:
                candidate["label"] = changes["label"]

        candidate_edges = list(raw_edges)
        candidate_edges[edge_index] = candidate
        normalized_edges = _parse_edges(candidate_edges, mutation=True)
        normalized_edge = normalized_edges[edge_index]
        for field in ("source", "target", "type"):
            candidate[field] = normalized_edge[field]
        if "label" in normalized_edge:
            candidate["label"] = normalized_edge["label"]
        else:
            candidate.pop("label", None)
        document["edges"] = candidate_edges
        return _ordered_document(document)

    return _mutate_graph_document(
        project_root,
        expected_etag,
        build_document,
        allow_overwrite=allow_overwrite,
    )


def delete_graph_edge(
    project_root: Path,
    edge_id: str,
    *,
    expected_etag: str,
    allow_overwrite: bool = False,
) -> GraphOverlay:
    """Delete one artificial relationship while leaving all node overlays intact."""

    def build_document(overlay: GraphOverlay) -> dict[str, Any]:
        if not isinstance(edge_id, str) or not edge_id.strip():
            raise GraphOverlayError("graph_schema_error", "Graph edge id 必须是非空字符串")

        document = deepcopy(overlay.raw_document) if overlay.raw_document is not None else {}
        document["version"] = CURRENT_GRAPH_VERSION
        raw_edges = document.get("edges", [])
        if not isinstance(raw_edges, list):
            raise GraphOverlayError("graph_schema_error", "Graph overlay edges 必须是列表")
        if not any(isinstance(raw_edge, dict) and raw_edge.get("id") == edge_id for raw_edge in raw_edges):
            raise GraphOverlayError("edge_not_found", f"找不到 Graph edge：{edge_id}")
        document["edges"] = [
            raw_edge
            for raw_edge in raw_edges
            if not (isinstance(raw_edge, dict) and raw_edge.get("id") == edge_id)
        ]
        return _ordered_document(document)

    return _mutate_graph_document(
        project_root,
        expected_etag,
        build_document,
        allow_overwrite=allow_overwrite,
    )


def _check_graph_mutation_etag(
    overlay: GraphOverlay,
    expected_etag: str,
    *,
    allow_overwrite: bool = False,
) -> None:
    if overlay.file_status == "future":
        raise GraphOverlayError(
            "graph_read_only",
            "Graph overlay 属于更高版本，只能只读打开",
        )
    if overlay.file_status == "legacy":
        raise GraphOverlayError(
            "graph_read_only",
            "Graph overlay 属于旧版本，请先迁移后再保存",
        )
    _check_graph_etag(overlay, expected_etag, allow_overwrite=allow_overwrite)


def _ordered_document(document: dict[str, Any]) -> dict[str, Any]:
    ordered: dict[str, Any] = {}
    for key in ("version", "project", "nodes", "edges"):
        if key in document:
            ordered[key] = document[key]
    for key, value in document.items():
        if key not in ordered:
            ordered[key] = value
    return ordered


def _document_for_update(
    overlay: GraphOverlay,
    conversation_id: str,
    node: ConversationOverlay,
) -> dict[str, Any]:
    document = deepcopy(overlay.raw_document) if overlay.raw_document is not None else {}
    document["version"] = CURRENT_GRAPH_VERSION
    raw_nodes = document.get("nodes", {})
    if not isinstance(raw_nodes, dict):
        raise GraphOverlayError("graph_schema_error", "Graph overlay nodes 必须是映射")
    raw_node = raw_nodes.get(conversation_id, {})
    if not isinstance(raw_node, dict):
        raise GraphOverlayError(
            "graph_schema_error",
            f"Graph 节点 {conversation_id} 必须是映射",
        )
    raw_node = deepcopy(raw_node)
    for field in GRAPH_NODE_FIELDS:
        raw_node.pop(field, None)
    raw_node.update(_node_document(node))
    if raw_node:
        raw_nodes[conversation_id] = raw_node
    else:
        raw_nodes.pop(conversation_id, None)
    document["nodes"] = raw_nodes
    document.setdefault("edges", [])
    return _ordered_document(document)


def _node_document(node: ConversationOverlay) -> dict[str, Any]:
    document: dict[str, Any] = {}
    if node.title is not None:
        document["title"] = node.title
    if node.tags:
        document["tags"] = list(node.tags)
    if node.status != "none":
        document["status"] = node.status
    if node.note is not None:
        document["note"] = node.note
    if node.hidden:
        document["hidden"] = True
    if node.layout is not None:
        document["layout"] = node.layout
    return document


def _write_graph_document(
    project_root: Path,
    document: dict[str, Any],
    *,
    lock_held: bool = False,
) -> None:
    if not lock_held:
        with _graph_lock(project_root):
            _write_graph_document(project_root, document, lock_held=True)
        return

    graph_path = project_root / ".codex" / "graph.yaml"
    graph_directory = graph_path.parent
    temporary_path: str | None = None
    try:
        resolved_graph_path = graph_path.resolve(strict=False)
        resolved_root = project_root.resolve(strict=True)
        if not _is_relative_to(resolved_graph_path, resolved_root):
            raise GraphOverlayError(
                "graph_write_error",
                "Graph overlay 路径超出 Project 根目录",
            )
        graph_directory.mkdir(parents=True, exist_ok=True)
        file_descriptor, temporary_path = tempfile.mkstemp(
            prefix=".graph.yaml.tmp.",
            dir=graph_directory,
        )
        with os.fdopen(file_descriptor, "w", encoding="utf-8") as temporary_file:
            yaml.safe_dump(document, temporary_file, allow_unicode=True, sort_keys=False)
            temporary_file.flush()
            os.fsync(temporary_file.fileno())
        os.replace(temporary_path, graph_path)
        temporary_path = None
        try:
            _fsync_directory(graph_path.parent)
        except OSError:
            # The atomic replacement has already committed; directory fsync is
            # only a durability enhancement on platforms that support it.
            pass
    except GraphOverlayError:
        raise
    except OSError as exc:
        _raise_graph_write_error(exc)
    except (yaml.YAMLError, TypeError, ValueError) as exc:
        raise GraphOverlayError(
            "graph_write_error",
            "无法保存 Graph overlay，请检查 Project 的写入权限后重试",
            details={"error": str(exc)},
        ) from exc
    finally:
        if temporary_path is not None:
            try:
                os.unlink(temporary_path)
            except OSError:
                pass


def _fsync_directory(directory: Path) -> None:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    try:
        directory_fd = os.open(directory, flags)
    except OSError as exc:
        if exc.errno in {errno.EINVAL, errno.ENOTSUP, errno.ENOTTY}:
            return
        raise
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def _validate_project(project: Any) -> None:
    if project is None:
        return
    if not isinstance(project, dict):
        raise GraphOverlayError("graph_schema_error", "Graph overlay project 必须是映射")
    name = project.get("name")
    if name is not None and not isinstance(name, str):
        raise GraphOverlayError("graph_schema_error", "Graph overlay project.name 必须是字符串")


def _parse_nodes(raw_nodes: Any) -> dict[str, ConversationOverlay]:
    if not isinstance(raw_nodes, dict):
        raise GraphOverlayError("graph_schema_error", "Graph overlay nodes 必须是映射")
    nodes: dict[str, ConversationOverlay] = {}
    for conversation_id, raw_node in raw_nodes.items():
        if not isinstance(conversation_id, str) or not conversation_id.strip():
            raise GraphOverlayError("graph_schema_error", "Graph 节点 ID 必须是非空字符串")
        if not isinstance(raw_node, dict):
            raise GraphOverlayError(
                "graph_schema_error",
                f"Graph 节点 {conversation_id} 必须是映射",
            )
        nodes[conversation_id] = _parse_node(conversation_id, raw_node)
    return nodes


def _parse_node(conversation_id: str, raw_node: dict[str, Any]) -> ConversationOverlay:
    title = raw_node.get("title")
    if title is not None and not isinstance(title, str):
        raise GraphOverlayError(
            "graph_schema_error",
            f"Graph 节点 {conversation_id} 的 title 必须是字符串",
        )
    normalized_title = title.strip() if isinstance(title, str) else None
    if normalized_title == "":
        normalized_title = None

    raw_tags = raw_node.get("tags", [])
    if not isinstance(raw_tags, list) or not all(isinstance(tag, str) for tag in raw_tags):
        raise GraphOverlayError(
            "graph_schema_error",
            f"Graph 节点 {conversation_id} 的 tags 必须是字符串列表",
        )
    tags: list[str] = []
    for tag in raw_tags:
        normalized_tag = tag.strip()
        if normalized_tag and normalized_tag not in tags:
            tags.append(normalized_tag)

    status = raw_node.get("status", "none")
    if not isinstance(status, str) or status not in USER_STATUSES:
        raise GraphOverlayError(
            "graph_schema_error",
            f"Graph 节点 {conversation_id} 的 status 无效",
        )

    note = raw_node.get("note")
    if note is not None and not isinstance(note, str):
        raise GraphOverlayError(
            "graph_schema_error",
            f"Graph 节点 {conversation_id} 的 note 必须是字符串",
        )
    if isinstance(note, str) and not note.strip():
        note = None

    hidden = raw_node.get("hidden", False)
    if not isinstance(hidden, bool):
        raise GraphOverlayError(
            "graph_schema_error",
            f"Graph 节点 {conversation_id} 的 hidden 必须是布尔值",
        )

    layout = _parse_layout(conversation_id, raw_node.get("layout"))
    return ConversationOverlay(
        title=normalized_title,
        tags=tuple(tags),
        status=status,
        note=note,
        hidden=hidden,
        layout=layout,
    )


def _parse_layout(conversation_id: str, raw_layout: Any) -> dict[str, float] | None:
    if raw_layout is None:
        return None
    if not isinstance(raw_layout, dict) or set(raw_layout) != {"x", "y"}:
        raise GraphOverlayError(
            "graph_schema_error",
            f"Graph 节点 {conversation_id} 的 layout 必须包含 x 和 y",
        )
    values: dict[str, float] = {}
    for axis in ("x", "y"):
        value = raw_layout[axis]
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise GraphOverlayError(
                "graph_schema_error",
                f"Graph 节点 {conversation_id} 的 layout.{axis} 必须是有限数字",
            )
        try:
            numeric_value = float(value)
        except (OverflowError, ValueError):
            raise GraphOverlayError(
                "graph_schema_error",
                f"Graph 节点 {conversation_id} 的 layout.{axis} 必须是有限数字",
            ) from None
        if not math.isfinite(numeric_value):
            raise GraphOverlayError(
                "graph_schema_error",
                f"Graph 节点 {conversation_id} 的 layout.{axis} 必须是有限数字",
            )
        values[axis] = numeric_value
    return values


def _parse_edges(
    raw_edges: Any,
    *,
    mutation: bool = False,
) -> tuple[dict[str, Any], ...]:
    if not isinstance(raw_edges, list):
        raise GraphOverlayError("graph_schema_error", "Graph overlay edges 必须是列表")

    edges: list[dict[str, Any]] = []
    edge_ids: set[str] = set()
    identities: set[tuple[str, str, str]] = set()
    for raw_edge in raw_edges:
        if not isinstance(raw_edge, dict):
            raise GraphOverlayError("graph_schema_error", "Graph edge 必须是映射")
        edge_id = _required_string(raw_edge, "id", "Graph edge id")
        source = _required_string(raw_edge, "source", "Graph edge source")
        target = _required_string(raw_edge, "target", "Graph edge target")
        edge_type = _required_string(raw_edge, "type", "Graph edge type", strip=True)
        if source == target:
            raise GraphOverlayError(
                "self_edge" if mutation else "graph_schema_error",
                "Graph edge 不允许自环",
            )
        if edge_id in edge_ids:
            raise GraphOverlayError("graph_schema_error", f"Graph edge id 重复：{edge_id}")
        edge_ids.add(edge_id)

        normalized_type = edge_type.lower() if edge_type.lower() in BUILT_IN_EDGE_TYPES else edge_type
        if normalized_type == "related_to" and source > target:
            source, target = target, source
        identity = (source, target, normalized_type)
        if identity in identities:
            raise GraphOverlayError(
                "duplicate_edge" if mutation else "graph_schema_error",
                "Graph overlay 包含重复关系",
            )
        identities.add(identity)

        label = raw_edge.get("label")
        if label is not None and not isinstance(label, str):
            raise GraphOverlayError("graph_schema_error", f"Graph edge {edge_id} 的 label 必须是字符串")
        edge: dict[str, Any] = {
            "id": edge_id,
            "source": source,
            "target": target,
            "type": normalized_type,
        }
        if label is not None:
            edge["label"] = label
        edges.append(edge)
    return tuple(edges)


def _required_string(
    raw: dict[str, Any],
    key: str,
    label: str,
    *,
    strip: bool = False,
) -> str:
    value = raw.get(key)
    if not isinstance(value, str) or not value.strip():
        raise GraphOverlayError("graph_schema_error", f"{label} 必须是非空字符串")
    return value.strip() if strip else value


def _is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True
