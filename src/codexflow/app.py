from __future__ import annotations

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from .models import DashboardSnapshot, ProjectView
from .project import (
    ProjectGraphService,
    ProjectInvalidError,
    ProjectNotSelectedError,
    SourceUnavailableError,
)
from .source import CodexThreadSource


class ProjectSelectRequest(BaseModel):
    path: str


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
            details={"errors": exc.errors()},
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
            selected = service.select_project(payload.path)
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
        return {
            "project": selected.to_dict(),
            "source": _source_summary(codex_source),
        }

    @app.get("/api/project")
    def get_project() -> dict[str, Any]:
        try:
            return {"project": service.project_view().to_dict()}
        except ProjectNotSelectedError as exc:
            raise ApiFailure(404, "project_not_selected", str(exc)) from exc

    @app.get("/api/snapshot")
    def get_snapshot() -> dict[str, Any]:
        return _snapshot_payload(service)

    @app.post("/api/refresh")
    def refresh() -> dict[str, Any]:
        return _snapshot_payload(service)

    static_directories = (
        Path(__file__).resolve().parents[2] / "frontend" / "dist",
        Path(__file__).resolve().parent / "frontend" / "dist",
    )
    static_directory = next((path for path in static_directories if path.is_dir()), None)
    if static_directory is not None:
        from fastapi.staticfiles import StaticFiles

        app.mount("/", StaticFiles(directory=static_directory, html=True), name="frontend")
    return app


def _snapshot_payload(service: ProjectGraphService) -> dict[str, Any]:
    try:
        snapshot: DashboardSnapshot = service.snapshot()
    except ProjectNotSelectedError as exc:
        raise ApiFailure(404, "project_not_selected", str(exc)) from exc
    except SourceUnavailableError as exc:
        result = exc.result
        code = "source_incompatible" if result.status == "incompatible" else "source_unavailable"
        message = (
            result.error.message
            if result.error
            else "Codex 来源不可用，尚未形成完整 source snapshot"
        )
        raise ApiFailure(
            503,
            code,
            message,
            details={
                "sourceStatus": result.status,
                "userAgent": result.user_agent,
                "hasCompleteSnapshot": result.has_complete_snapshot,
            },
            retryable=result.error.retryable if result.error else True,
        ) from exc
    return snapshot.to_dict()


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
