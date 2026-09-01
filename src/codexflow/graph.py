from __future__ import annotations

from dataclasses import dataclass
from hashlib import sha256
import math
from pathlib import Path
from typing import Any

import yaml

from .models import ConversationOverlay


CURRENT_GRAPH_VERSION = 1
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

    if not graph_path.exists():
        return GraphOverlay(nodes={}, edges=(), etag="absent", file_status="absent")
    if not graph_path.is_file():
        raise GraphOverlayError("graph_parse_error", "Graph overlay 不是文件")

    try:
        raw_bytes = graph_path.read_bytes()
    except OSError as exc:
        raise GraphOverlayError("graph_parse_error", "无法读取 Graph overlay") from exc
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
    if isinstance(version, bool) or not isinstance(version, int) or version < 1:
        raise GraphOverlayError("graph_version_error", "Graph overlay version 必须是正整数")
    file_status = "future" if version > CURRENT_GRAPH_VERSION else "ready"

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
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
            raise GraphOverlayError(
                "graph_schema_error",
                f"Graph 节点 {conversation_id} 的 layout.{axis} 必须是有限数字",
            )
        values[axis] = float(value)
    return values


def _parse_edges(raw_edges: Any) -> tuple[dict[str, Any], ...]:
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
            raise GraphOverlayError("graph_schema_error", "Graph edge 不允许自环")
        if edge_id in edge_ids:
            raise GraphOverlayError("graph_schema_error", f"Graph edge id 重复：{edge_id}")
        edge_ids.add(edge_id)

        normalized_type = edge_type.lower() if edge_type.lower() in BUILT_IN_EDGE_TYPES else edge_type
        if normalized_type == "related_to" and source > target:
            source, target = target, source
        identity = (source, target, normalized_type)
        if identity in identities:
            raise GraphOverlayError("graph_schema_error", "Graph overlay 包含重复关系")
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
