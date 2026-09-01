from __future__ import annotations

import math
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any, Literal

from fastapi import FastAPI, Header, Request, Response
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from pydantic import BaseModel, ConfigDict, StrictBool, StrictStr, field_validator

from .graph import GraphOverlayError
from .models import DashboardSnapshot, ProjectView
from .project import (
    ProjectGraphService,
    ProjectInvalidError,
    ProjectNotSelectedError,
    SourceUnavailableError,
)
from .source import CodexThreadSource
from .timeline import TimelineInputError


class ProjectSelectRequest(BaseModel):
    path: str


class GraphNodeLayoutRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    x: float
    y: float

    @field_validator("x", "y", mode="before")
    @classmethod
    def validate_coordinate(cls, value: object) -> float:
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValueError("布局坐标必须是数字")  # noqa: TRY004
        try:
            numeric_value = float(value)
        except (OverflowError, ValueError):
            raise ValueError("布局坐标必须是有限数字") from None
        if not math.isfinite(numeric_value):
            raise ValueError("布局坐标必须是有限数字")
        return numeric_value


class GraphNodePatchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    title: StrictStr | None = None
    tags: list[StrictStr] | None = None
    status: Literal["none", "active", "done", "blocked"] | None = None
    note: StrictStr | None = None
    hidden: StrictBool | None = None
    layout: GraphNodeLayoutRequest | None = None


class GraphEdgeCreateRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    source: StrictStr
    target: StrictStr
    type: StrictStr
    label: StrictStr | None = None


class GraphEdgePatchRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    source: StrictStr | None = None
    target: StrictStr | None = None
    type: StrictStr | None = None
    label: StrictStr | None = None


class GraphCopyRequest(BaseModel):
    document: dict[str, Any]


class ApiFailure(Exception):
    def __init__(
        self,
        status_code: int,
        code: str,
        message: str,
        *,
        details: dict[str, Any] | None = None,
        retryable: bool = False,
    ) -> None:
        super().__init__(message)
        self.status_code = status_code
        self.code = code
        self.message = message
        self.details = details
        self.retryable = retryable


def create_app(*, source: object | None = None) -> FastAPI:
    codex_source = source if source is not None else CodexThreadSource()
    service = ProjectGraphService(codex_source)

    @asynccontextmanager
    async def lifespan(_: FastAPI) -> AsyncIterator[None]:
        try:
            yield
        finally:
            close = getattr(codex_source, "close", None)
            if callable(close):
                close()

    app = FastAPI(title="CodexFlow", version="0.1.0", lifespan=lifespan)
    app.state.source = codex_source
    app.state.project_service = service

    @app.exception_handler(ApiFailure)
    async def api_failure_handler(_: Request, exc: ApiFailure) -> JSONResponse:
        return _error_response(
            exc.status_code,
            exc.code,
            exc.message,
            details=exc.details,
            retryable=exc.retryable,
        )

    @app.exception_handler(RequestValidationError)
    async def request_validation_handler(_: Request, exc: RequestValidationError) -> JSONResponse:
        return _error_response(
            400,
            "invalid_request",
            "请求字段无效",
            details={
                "errors": [
                    {key: value for key, value in error.items() if key != "ctx"}
                    for error in exc.errors()
                ]
            },
        )

    @app.get("/api/health")
    def health() -> dict[str, Any]:
        project_view = _current_project_view(service)
        return {
            "status": "ok",
            "listenHost": "127.0.0.1",
            "source": _source_summary(codex_source),
            "project": project_view.to_dict() if project_view else None,
        }

    @app.post("/api/project/select")
    def select_project(payload: ProjectSelectRequest) -> dict[str, Any]:
        try:
            service.select_project(payload.path)
        except ProjectInvalidError as exc:
            raise ApiFailure(
                422,
                "project_invalid",
                exc.message,
                details=exc.details if isinstance(exc.details, dict) else None,
            ) from exc
        try:
            service.snapshot()
        except SourceUnavailableError:
            # A valid Project remains selectable while the UI presents the
            # source-specific unavailable state and offers another load.
            pass
        except GraphOverlayError as exc:
            raise _graph_api_failure(exc) from exc
        return {
            "project": service.project_view().to_dict(),
            "source": _source_summary(codex_source),
        }

    @app.get("/api/project")
    def get_project() -> dict[str, Any]:
        try:
            return {"project": service.project_view().to_dict()}
        except ProjectNotSelectedError as exc:
            raise ApiFailure(404, "project_not_selected", str(exc)) from exc

    @app.post("/api/graph/migrate")
    def migrate_graph(
        response: Response,
        if_match: str | None = Header(default=None, alias="If-Match"),
        overwrite: bool = False,
    ) -> dict[str, Any]:
        expected_etag = _require_if_match(if_match)
        try:
            outcome = service.migrate_graph(
                expected_etag=expected_etag,
                allow_overwrite=overwrite,
            )
        except ProjectNotSelectedError as exc:
            raise ApiFailure(404, "project_not_selected", str(exc)) from exc
        except SourceUnavailableError as exc:
            raise _source_api_failure(exc) from exc
        except GraphOverlayError as exc:
            raise _graph_api_failure(exc) from exc
        response.headers["ETag"] = _quote_etag(outcome.snapshot.graph.etag)
        payload = outcome.snapshot.to_dict()
        payload["backupPath"] = str(outcome.backup_path) if outcome.backup_path else None
        return payload

    @app.post("/api/graph/copy")
    def copy_graph(payload: GraphCopyRequest) -> dict[str, Any]:
        try:
            copy_path = service.save_graph_copy(payload.document)
        except ProjectNotSelectedError as exc:
            raise ApiFailure(404, "project_not_selected", str(exc)) from exc
        except GraphOverlayError as exc:
            raise _graph_api_failure(exc) from exc
        return {"copyPath": str(copy_path)}

    @app.get("/api/snapshot")
    def get_snapshot(
        response: Response,
        granularity: str = "day",
        timezone: str | None = None,
    ) -> dict[str, Any]:
        return _snapshot_payload(
            service,
            response=response,
            granularity=granularity,
            timezone=timezone,
        )

    @app.patch("/api/graph/nodes/{conversation_id}")
    def patch_graph_node(
        conversation_id: str,
        payload: GraphNodePatchRequest,
        response: Response,
        if_match: str | None = Header(default=None, alias="If-Match"),
        overwrite: bool = False,
    ) -> dict[str, Any]:
        if if_match is None or not if_match.strip():
            raise ApiFailure(
                400,
                "invalid_request",
                "保存 Graph 节点需要 If-Match 版本标记",
            )
        changes = payload.model_dump(exclude_unset=True)
        if not changes:
            raise ApiFailure(400, "invalid_request", "至少需要修改一个 Graph 节点字段")
        if "layout" in changes and changes["layout"] is not None:
            changes["layout"] = dict(changes["layout"])
        try:
            snapshot = service.update_node_overlay(
                conversation_id,
                changes,
                expected_etag=_unquote_etag(if_match),
                allow_overwrite=overwrite,
            )
        except ProjectNotSelectedError as exc:
            raise ApiFailure(404, "project_not_selected", str(exc)) from exc
        except SourceUnavailableError as exc:
            raise _source_api_failure(exc) from exc
        except GraphOverlayError as exc:
            raise _graph_api_failure(exc) from exc
        response.headers["ETag"] = _quote_etag(snapshot.graph.etag)
        return snapshot.to_dict()

    @app.post("/api/graph/edges")
    def create_graph_edge_endpoint(
        payload: GraphEdgeCreateRequest,
        response: Response,
        if_match: str | None = Header(default=None, alias="If-Match"),
        overwrite: bool = False,
    ) -> dict[str, Any]:
        expected_etag = _require_if_match(if_match)
        try:
            snapshot = service.create_edge(
                payload.source,
                payload.target,
                payload.type,
                payload.label,
                expected_etag=expected_etag,
                allow_overwrite=overwrite,
            )
        except ProjectNotSelectedError as exc:
            raise ApiFailure(404, "project_not_selected", str(exc)) from exc
        except SourceUnavailableError as exc:
            raise _source_api_failure(exc) from exc
        except GraphOverlayError as exc:
            raise _graph_api_failure(exc) from exc
        response.headers["ETag"] = _quote_etag(snapshot.graph.etag)
        result = snapshot.to_dict()
        result["edge"] = snapshot.graph.edges[-1]
        return result

    @app.patch("/api/graph/edges/{edge_id}")
    def patch_graph_edge(
        edge_id: str,
        payload: GraphEdgePatchRequest,
        response: Response,
        if_match: str | None = Header(default=None, alias="If-Match"),
        overwrite: bool = False,
    ) -> dict[str, Any]:
        expected_etag = _require_if_match(if_match)
        changes = payload.model_dump(exclude_unset=True)
        if not changes:
            raise ApiFailure(400, "invalid_request", "至少需要修改一个 Graph edge 字段")
        try:
            snapshot = service.update_edge(
                edge_id,
                changes,
                expected_etag=expected_etag,
                allow_overwrite=overwrite,
            )
        except ProjectNotSelectedError as exc:
            raise ApiFailure(404, "project_not_selected", str(exc)) from exc
        except SourceUnavailableError as exc:
            raise _source_api_failure(exc) from exc
        except GraphOverlayError as exc:
            raise _graph_api_failure(exc) from exc
        response.headers["ETag"] = _quote_etag(snapshot.graph.etag)
        return snapshot.to_dict()

    @app.delete("/api/graph/edges/{edge_id}")
    def delete_graph_edge_endpoint(
        edge_id: str,
        response: Response,
        if_match: str | None = Header(default=None, alias="If-Match"),
        overwrite: bool = False,
    ) -> dict[str, Any]:
        expected_etag = _require_if_match(if_match)
        try:
            snapshot = service.delete_edge(
                edge_id,
                expected_etag=expected_etag,
                allow_overwrite=overwrite,
            )
        except ProjectNotSelectedError as exc:
            raise ApiFailure(404, "project_not_selected", str(exc)) from exc
        except SourceUnavailableError as exc:
            raise _source_api_failure(exc) from exc
        except GraphOverlayError as exc:
            raise _graph_api_failure(exc) from exc
        response.headers["ETag"] = _quote_etag(snapshot.graph.etag)
        return snapshot.to_dict()

    @app.post("/api/refresh")
    def refresh(
        response: Response,
        granularity: str = "day",
        timezone: str | None = None,
    ) -> dict[str, Any]:
        return _snapshot_payload(
            service,
            response=response,
            granularity=granularity,
            timezone=timezone,
        )

    static_directories = (
        Path(__file__).resolve().parents[2] / "frontend" / "dist",
        Path(__file__).resolve().parent / "frontend" / "dist",
    )
    static_directory = next((path for path in static_directories if path.is_dir()), None)
    if static_directory is not None:
        from fastapi.staticfiles import StaticFiles

        app.mount("/", StaticFiles(directory=static_directory, html=True), name="frontend")
    return app


def _snapshot_payload(
    service: ProjectGraphService,
    *,
    response: Response | None = None,
    granularity: str = "day",
    timezone: str | None = None,
) -> dict[str, Any]:
    try:
        snapshot: DashboardSnapshot = service.snapshot(
            granularity=granularity,
            timezone=timezone,
        )
    except ProjectNotSelectedError as exc:
        raise ApiFailure(404, "project_not_selected", str(exc)) from exc
    except SourceUnavailableError as exc:
        raise _source_api_failure(exc) from exc
    except GraphOverlayError as exc:
        raise _graph_api_failure(exc) from exc
    except TimelineInputError as exc:
        raise ApiFailure(400, "invalid_request", str(exc)) from exc
    if response is not None:
        response.headers["ETag"] = _quote_etag(snapshot.graph.etag)
    return snapshot.to_dict()


def _graph_api_failure(exc: GraphOverlayError) -> ApiFailure:
    status_code = {
        "edge_not_found": 404,
        "graph_conflict": 412,
        "graph_busy": 409,
        "duplicate_edge": 409,
        "self_edge": 409,
        "graph_read_only": 423,
    }.get(exc.code, 422 if exc.code in {
        "graph_parse_error",
        "graph_schema_error",
        "graph_version_error",
    } else 500)
    return ApiFailure(
        status_code,
        exc.code,
        exc.message,
        details=exc.details,
        retryable=exc.code in {"graph_conflict", "graph_busy", "graph_write_error"},
    )


def _require_if_match(if_match: str | None) -> str:
    if if_match is None or not if_match.strip():
        raise ApiFailure(
            400,
            "invalid_request",
            "保存 Graph edge 需要 If-Match 版本标记",
        )
    return _unquote_etag(if_match)


def _source_api_failure(exc: SourceUnavailableError) -> ApiFailure:
    result = exc.result
    code = "source_incompatible" if result.status == "incompatible" else "source_unavailable"
    return ApiFailure(
        503,
        code,
        result.error.message if result.error else "Codex 来源不可用，尚未形成完整 source snapshot",
        details={
            "sourceStatus": result.status,
            "userAgent": result.user_agent,
            "hasCompleteSnapshot": result.has_complete_snapshot,
        },
        retryable=result.error.retryable if result.error else True,
    )


def _unquote_etag(value: str) -> str:
    stripped = value.strip()
    if len(stripped) >= 2 and stripped[0] == '"' and stripped[-1] == '"':
        return stripped[1:-1]
    return stripped


def _quote_etag(value: str) -> str:
    return f'"{value}"'


def _current_project_view(service: ProjectGraphService) -> ProjectView | None:
    try:
        return service.project_view()
    except ProjectNotSelectedError:
        return None


def _source_summary(source: object) -> dict[str, Any]:
    status = getattr(source, "status", "unavailable")
    user_agent = getattr(source, "user_agent", None)
    failure = getattr(source, "last_error", None)
    return {
        "status": status,
        "userAgent": user_agent,
        "error": _source_failure_payload(failure),
    }


def _source_failure_payload(failure: object | None) -> dict[str, Any] | None:
    if failure is None:
        return None
    return {
        "code": getattr(failure, "code", "source_unavailable"),
        "message": getattr(failure, "message", str(failure)),
        "details": getattr(failure, "details", None),
        "retryable": getattr(failure, "retryable", True),
    }


def _error_response(
    status_code: int,
    code: str,
    message: str,
    *,
    details: dict[str, Any] | None = None,
    retryable: bool = False,
) -> JSONResponse:
    return JSONResponse(
        status_code=status_code,
        content={
            "error": {
                "code": code,
                "message": message,
                "details": details,
                "retryable": retryable,
            }
        },
    )


app = create_app()
