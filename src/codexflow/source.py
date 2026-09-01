from __future__ import annotations

import json
import subprocess
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any, Literal

SourceStatus = Literal["ready", "stale", "unavailable", "incompatible"]
ALLOWED_SOURCE_KINDS = ("cli", "vscode", "appServer")
REQUIRED_METHODS = ("initialize", "thread/list", "thread/read")


class SourceFailure(Exception):
    """A failure at the CodexThreadSource boundary."""

    def __init__(
        self,
        code: str,
        message: str,
        *,
        retryable: bool = True,
        incompatible: bool = False,
        details: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable
        self.incompatible = incompatible
        self.details = details


@dataclass(frozen=True)
class CodexThread:
    id: str
    title: str | None
    preview: str
    created_at: str | None
    updated_at: str | None
    recency_at: str | None
    cwd: str
    source: str
    archived: bool
    history_mode: str | None
    status: str
    project_id: str | None
    git_info: dict[str, Any] | None
    observation_error: str | None = None


@dataclass(frozen=True)
class SourceReadResult:
    status: SourceStatus
    threads: tuple[CodexThread, ...]
    generated_at: str | None
    user_agent: str | None
    error: SourceFailure | None = None

    @property
    def has_complete_snapshot(self) -> bool:
        return self.status in ("ready", "stale")


ProcessFactory = Callable[[], Any]


class _JsonRpcError(SourceFailure):
    def __init__(self, code: Any, message: str) -> None:
        numeric_code = code if isinstance(code, int) else None
        method_missing = numeric_code == -32601 or "method not found" in message.lower()
        super().__init__(
            "source_incompatible" if method_missing else "source_unavailable",
            message,
            incompatible=method_missing,
            details={"rpcCode": code},
        )
        self.rpc_code = code


class CodexThreadSource:
    """Read local Codex Threads through the app-server stdio JSON-RPC boundary."""

    def __init__(
        self,
        *,
        command: Sequence[str] | None = None,
        process_factory: ProcessFactory | None = None,
        client_name: str = "codexflow",
        client_version: str = "0.1.0",
    ) -> None:
        self._command = tuple(command or ("codex", "app-server", "--stdio"))
        self._process_factory = process_factory or self._default_process
        self._client_name = client_name
        self._client_version = client_version
        self._process: Any | None = None
        self._request_id = 0
        self._initialized = False
        self._last_complete: tuple[CodexThread, ...] | None = None
        self._last_generated_at: str | None = None
        self._last_status: SourceStatus = "unavailable"
        self._last_error: SourceFailure | None = None
        self._user_agent: str | None = None

    @property
    def status(self) -> SourceStatus:
        return self._last_status

    @property
    def last_error(self) -> SourceFailure | None:
        return self._last_error

    @property
    def user_agent(self) -> str | None:
        return self._user_agent

    def read_snapshot(self) -> SourceReadResult:
        """Read every active and archived page before publishing a snapshot."""
        try:
            initialize_result = self._ensure_initialized()
            self._validate_advertised_capabilities(initialize_result)
            self._probe_thread_read()

            threads_by_id: dict[str, CodexThread] = {}
            for archived in (False, True):
                for raw_thread in self._read_all_pages(archived=archived):
                    thread = _normalize_thread(raw_thread, archived=archived)
                    if thread.source not in ALLOWED_SOURCE_KINDS:
                        continue
                    previous = threads_by_id.get(thread.id)
                    if previous is None or thread.archived:
                        threads_by_id[thread.id] = thread

            threads = tuple(threads_by_id.values())
            generated_at = _utc_now()
            self._last_complete = threads
            self._last_generated_at = generated_at
            self._last_status = "ready"
            self._last_error = None
            return SourceReadResult(
                status="ready",
                threads=threads,
                generated_at=generated_at,
                user_agent=self._user_agent,
            )
        except SourceFailure as failure:
            self._close_process()
            self._last_error = failure
            self._last_status = "stale" if self._last_complete is not None else (
                "incompatible" if failure.incompatible else "unavailable"
            )
            return SourceReadResult(
                status=self._last_status,
                threads=self._last_complete or (),
                generated_at=self._last_generated_at,
                user_agent=self._user_agent,
                error=failure,
            )

    def read_thread(self, thread_id: str) -> CodexThread:
        """Read one thread without making a failed read look like a missing thread."""
        if not thread_id.strip():
            raise ValueError("thread_id must not be empty")
        try:
            self._ensure_initialized()
            self._probe_thread_read()
            result = self._request(
                "thread/read",
                {"threadId": thread_id, "includeTurns": False},
            )
            if not isinstance(result, dict):
                raise SourceFailure(
                    "source_unavailable",
                    "app-server returned an invalid thread/read response",
                )
            raw_thread = result.get("thread", result)
            if not isinstance(raw_thread, dict):
                raise SourceFailure(
                    "source_unavailable",
                    "app-server returned no thread in thread/read response",
                )
            return _normalize_thread(raw_thread, archived=False)
        except SourceFailure:
            raise
        except Exception as exc:
            raise SourceFailure("source_unavailable", str(exc)) from exc

    def close(self) -> None:
        self._close_process()

    def _default_process(self) -> Any:
        try:
            return subprocess.Popen(
                self._command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
                bufsize=1,
            )
        except OSError as exc:
            raise SourceFailure(
                "source_unavailable",
                f"无法启动 Codex app-server：{exc}",
                details={"command": list(self._command)},
            ) from exc

    def _ensure_initialized(self) -> dict[str, Any]:
        if self._initialized and self._process is not None:
            return {}
        try:
            self._process = self._process_factory()
        except SourceFailure:
            raise
        except Exception as exc:
            raise SourceFailure("source_unavailable", f"无法启动 Codex app-server：{exc}") from exc
        if self._process is None:
            raise SourceFailure("source_unavailable", "Codex app-server 未返回进程")
        try:
            result = self._request(
                "initialize",
                {
                    "clientInfo": {
                        "name": self._client_name,
                        "version": self._client_version,
                    },
                    "capabilities": {"experimentalApi": False},
                },
            )
            if not isinstance(result, dict):
                raise SourceFailure(
                    "source_incompatible",
                    "Codex app-server initialize 返回了无效结果",
                    incompatible=True,
                )
            user_agent = result.get("userAgent")
            if not isinstance(user_agent, str) or not user_agent:
                raise SourceFailure(
                    "source_incompatible",
                    "Codex app-server initialize 缺少 userAgent",
                    incompatible=True,
                )
            self._user_agent = user_agent
            self._notify("initialized")
            self._initialized = True
            return result
        except SourceFailure:
            self._close_process()
            raise

    def _validate_advertised_capabilities(self, initialize_result: dict[str, Any]) -> None:
        capabilities = initialize_result.get("capabilities")
        if capabilities is None:
            return
        advertised: set[str] = set()
        has_method_advertisement = False
        if isinstance(capabilities, list):
            advertised = {item for item in capabilities if isinstance(item, str)}
            has_method_advertisement = True
        elif isinstance(capabilities, dict):
            methods = capabilities.get("methods")
            if isinstance(methods, list):
                advertised = {item for item in methods if isinstance(item, str)}
                has_method_advertisement = True
            elif isinstance(methods, dict):
                advertised = {
                    item
                    for item, enabled in methods.items()
                    if isinstance(item, str) and enabled is not False
                }
                has_method_advertisement = True
            else:
                advertised_names = set(capabilities).intersection(REQUIRED_METHODS)
                if advertised_names:
                    advertised = {
                        item for item in advertised_names if capabilities.get(item) is not False
                    }
                    has_method_advertisement = True
        if has_method_advertisement:
            missing = sorted(set(REQUIRED_METHODS) - advertised)
            if missing:
                raise SourceFailure(
                    "source_incompatible",
                    f"Codex app-server 缺少最低能力：{', '.join(missing)}",
                    retryable=False,
                    incompatible=True,
                    details={"missingMethods": missing},
                )

    def _probe_thread_read(self) -> None:
        try:
            self._request(
                "thread/read",
                {"threadId": "__codexflow_capability_probe__", "includeTurns": False},
            )
        except _JsonRpcError as failure:
            if failure.incompatible:
                raise SourceFailure(
                    "source_incompatible",
                    "Codex app-server 缺少最低能力：thread/read",
                    retryable=False,
                    incompatible=True,
                    details={"missingMethods": ["thread/read"]},
                ) from failure
        except SourceFailure:
            # An application-level not-found or validation error proves that the
            # method exists; transport and protocol errors remain unavailable.
            raise

    def _read_all_pages(self, *, archived: bool) -> list[dict[str, Any]]:
        cursor: str | None = None
        seen_cursors: set[str] = set()
        threads: list[dict[str, Any]] = []
        while True:
            params: dict[str, Any] = {
                "archived": archived,
                "sourceKinds": list(ALLOWED_SOURCE_KINDS),
            }
            if cursor is not None:
                params["cursor"] = cursor
            result = self._request("thread/list", params)
            page_threads, next_cursor = _parse_thread_list_result(result)
            threads.extend(page_threads)
            if next_cursor is None:
                return threads
            if next_cursor in seen_cursors or next_cursor == cursor:
                raise SourceFailure(
                    "source_unavailable",
                    "Codex app-server 返回了重复的分页游标",
                    details={"cursor": next_cursor, "archived": archived},
                )
            seen_cursors.add(next_cursor)
            cursor = next_cursor

    def _request(self, method: str, params: dict[str, Any]) -> Any:
        if self._process is None:
            raise SourceFailure("source_unavailable", "Codex app-server 未连接")
        self._request_id += 1
        request_id = self._request_id
        self._write({"id": request_id, "method": method, "params": params})
        while True:
            line = self._readline()
            if not line:
                raise SourceFailure(
                    "source_unavailable",
                    "Codex app-server 在响应前退出",
                )
            try:
                message = json.loads(line)
            except json.JSONDecodeError as exc:
                raise SourceFailure(
                    "source_unavailable",
                    "Codex app-server 返回了无效 JSON-RPC 消息",
                ) from exc
            if not isinstance(message, dict) or message.get("id") != request_id:
                continue
            if "error" in message:
                error = message["error"]
                if isinstance(error, dict):
                    raise _JsonRpcError(error.get("code"), str(error.get("message", "未知 JSON-RPC 错误")))
                raise _JsonRpcError(None, "Codex app-server 返回了未知 JSON-RPC 错误")
            if "result" not in message:
                raise SourceFailure(
                    "source_unavailable",
                    "Codex app-server 响应缺少 result",
                )
            return message["result"]

    def _notify(self, method: str) -> None:
        self._write({"method": method})

    def _write(self, message: dict[str, Any]) -> None:
        if self._process is None or getattr(self._process, "stdin", None) is None:
            raise SourceFailure("source_unavailable", "Codex app-server stdin 不可用")
        try:
            stream = self._process.stdin
            payload = json.dumps(message, separators=(",", ":")) + "\n"
            stream.write(payload)
            stream.flush()
        except (BrokenPipeError, OSError) as exc:
            raise SourceFailure("source_unavailable", "无法写入 Codex app-server", details={"error": str(exc)}) from exc

    def _readline(self) -> str:
        if self._process is None or getattr(self._process, "stdout", None) is None:
            raise SourceFailure("source_unavailable", "Codex app-server stdout 不可用")
        try:
            line = self._process.stdout.readline()
        except (BrokenPipeError, OSError) as exc:
            raise SourceFailure("source_unavailable", "无法读取 Codex app-server", details={"error": str(exc)}) from exc
        if isinstance(line, bytes):
            line = line.decode("utf-8", errors="replace")
        return str(line).strip()

    def _close_process(self) -> None:
        process, self._process = self._process, None
        self._initialized = False
        if process is None:
            return
        try:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=1)
        except (OSError, subprocess.SubprocessError):
            try:
                process.kill()
            except OSError:
                pass


def _parse_thread_list_result(result: Any) -> tuple[list[dict[str, Any]], str | None]:
    if not isinstance(result, dict):
        raise SourceFailure("source_unavailable", "thread/list 返回了无效结果")
    data = result.get("data")
    if isinstance(data, dict):
        items = data.get("items")
    else:
        items = data
    if items is None:
        items = result.get("threads")
    if not isinstance(items, list) or not all(isinstance(item, dict) for item in items):
        raise SourceFailure("source_unavailable", "thread/list 返回了无效 items")
    if "nextCursor" in result:
        next_cursor = result["nextCursor"]
    else:
        next_cursor = result.get("next_cursor")
    if next_cursor is not None and not isinstance(next_cursor, str):
        raise SourceFailure("source_unavailable", "thread/list 返回了无效 nextCursor")
    return items, next_cursor


def _normalize_thread(raw: dict[str, Any], *, archived: bool) -> CodexThread:
    thread_id = raw.get("id")
    cwd = raw.get("cwd")
    source = raw.get("source")
    if not isinstance(thread_id, str) or not thread_id.strip():
        raise SourceFailure("source_unavailable", "thread/list 返回的 Thread 缺少有效 id")
    if not isinstance(cwd, str) or not cwd.strip():
        raise SourceFailure("source_unavailable", f"Thread {thread_id} 缺少有效 cwd")
    if not isinstance(source, str):
        source = "unknown"

    title = raw.get("name", raw.get("title"))
    if title is not None and not isinstance(title, str):
        raise SourceFailure("source_unavailable", f"Thread {thread_id} 的标题无效")
    preview = raw.get("preview", "")
    if not isinstance(preview, str):
        raise SourceFailure("source_unavailable", f"Thread {thread_id} 的 preview 无效")

    created_at, created_error = _parse_observation_timestamp(
        raw.get("createdAt", raw.get("created_at")), "createdAt"
    )
    updated_at, updated_error = _parse_observation_timestamp(
        raw.get("updatedAt", raw.get("updated_at")), "updatedAt"
    )
    recency_value = raw.get("recencyAt", raw.get("recency_at"))
    recency_at, recency_error = (
        (None, None)
        if recency_value is None
        else _parse_observation_timestamp(recency_value, "recencyAt")
    )

    status_value = raw.get("status", "unknown")
    if isinstance(status_value, dict):
        status_value = status_value.get("type", "unknown")
    status = status_value if isinstance(status_value, str) else "unknown"
    if status not in {"notLoaded", "idle", "systemError", "active"}:
        status = "unknown"

    history_mode = raw.get("historyMode", raw.get("history_mode"))
    if history_mode not in {None, "legacy", "paginated"}:
        history_mode = None

    project_id = raw.get("projectId", raw.get("project_id"))
    if project_id is not None and not isinstance(project_id, str):
        project_id = None
    git_info = raw.get("gitInfo", raw.get("git_info"))
    if git_info is not None and not isinstance(git_info, dict):
        git_info = None

    archived_value = raw.get("archived")
    archived_value = archived_value if isinstance(archived_value, bool) else archived
    return CodexThread(
        id=thread_id,
        title=title,
        preview=preview,
        created_at=created_at,
        updated_at=updated_at,
        recency_at=recency_at,
        cwd=cwd,
        source=source,
        archived=archived_value,
        history_mode=history_mode,
        status=status,
        project_id=project_id,
        git_info=git_info,
        observation_error="; ".join(
            error for error in (created_error, updated_error, recency_error) if error
        )
        or None,
    )


def _parse_observation_timestamp(
    value: Any,
    field: str,
) -> tuple[str | None, str | None]:
    if isinstance(value, bool) or not isinstance(value, int):
        return None, f"{field} 无效"
    try:
        return datetime.fromtimestamp(value, tz=UTC).isoformat().replace("+00:00", "Z"), None
    except (OverflowError, OSError, ValueError):
        return None, f"{field} 无效"


def _utc_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")
