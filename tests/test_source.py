from __future__ import annotations

import json
from collections import deque
from typing import Any

from codexflow.source import CodexThreadSource


class JsonRpcFixtureProcess:
    """A process-shaped fixture that responds to the public JSON-RPC seam."""

    def __init__(self, *, mode: str = "success", capabilities: Any = None) -> None:
        self.mode = mode
        self.capabilities = capabilities
        self.requests: list[dict[str, Any]] = []
        self._responses: deque[str] = deque()
        self.stdin = self
        self.stdout = self
        self._returncode: int | None = None

    def write(self, payload: str) -> int:
        message = json.loads(payload)
        self.requests.append(message)
        self._respond(message)
        return len(payload)

    def flush(self) -> None:
        return None

    def readline(self) -> str:
        if self._responses:
            return self._responses.popleft()
        return ""

    def poll(self) -> int | None:
        return self._returncode

    def terminate(self) -> None:
        self._returncode = 0

    def wait(self, timeout: float | None = None) -> int:
        self._returncode = 0
        return 0

    def _queue(self, message: dict[str, Any]) -> None:
        self._responses.append(json.dumps(message) + "\n")

    def _respond(self, message: dict[str, Any]) -> None:
        method = message.get("method")
        if method == "initialize":
            self._queue(
                {
                    "id": message["id"],
                    "result": {
                        "userAgent": "Codex Desktop/0.150.1 fixture",
                        "codexHome": "/tmp/codex-home",
                        "platformFamily": "unix",
                        "platformOs": "test",
                        **(
                            {"capabilities": self.capabilities}
                            if self.capabilities is not None
                            else {}
                        ),
                    },
                }
            )
        elif method == "thread/read":
            if self.mode == "missing-thread-read":
                self._queue(
                    {
                        "id": message["id"],
                        "error": {"code": -32601, "message": "Method not found"},
                    }
                )
                return
            self._queue(
                {
                    "id": message["id"],
                    "error": {"code": -32004, "message": "thread not found"},
                }
            )
        elif method == "initialized":
            return
        elif method == "thread/list":
            archived = message["params"]["archived"]
            cursor = message["params"].get("cursor")
            if archived is False and cursor is None:
                self._queue(
                    {
                        "id": message["id"],
                        "result": {
                            "data": {
                                "items": [
                                    {
                                        "id": "active-thread",
                                        "name": "Active work",
                                        "preview": "First active prompt",
                                        "createdAt": "not-an-integer" if self.mode == "invalid-time" else 1704067200,
                                        "updatedAt": 1704070800,
                                        "recencyAt": 1704070800,
                                        "cwd": "/projects/demo",
                                        "source": "cli",
                                        "status": {"type": "idle"},
                                        "projectId": "codex-project",
                                        "gitInfo": None,
                                        "historyMode": "paginated",
                                    }
                                ]
                            },
                            "nextCursor": "active-cursor",
                        },
                    }
                )
            elif archived is False and cursor == "active-cursor":
                if self.mode == "pagination-failure":
                    self._queue(
                        {
                            "id": message["id"],
                            "error": {"code": -32001, "message": "fixture page failed"},
                        }
                    )
                    return
                self._queue(
                    {
                        "id": message["id"],
                        "result": {"data": {"items": []}, "nextCursor": None},
                    }
                )
            elif archived is True and cursor is None:
                self._queue(
                    {
                        "id": message["id"],
                        "result": {
                            "data": {
                                "items": [
                                    {
                                        "id": "archived-thread",
                                        "name": None,
                                        "preview": "Archived prompt",
                                        "createdAt": 1704153600,
                                        "updatedAt": 1704153600,
                                        "recencyAt": None,
                                        "cwd": "/projects/demo",
                                        "source": "vscode",
                                        "status": {"type": "notLoaded"},
                                        "projectId": None,
                                        "gitInfo": {"branch": "main"},
                                    }
                                ]
                            },
                            "nextCursor": None,
                        },
                    }
                )
            else:
                raise AssertionError(f"unexpected thread/list request: {message}")
        else:
            raise AssertionError(f"unexpected JSON-RPC method: {method}")


def test_source_reads_both_thread_lists_with_minimum_capability_handshake() -> None:
    process = JsonRpcFixtureProcess()
    source = CodexThreadSource(process_factory=lambda: process)

    result = source.read_snapshot()

    assert result.status == "ready"
    assert [thread.id for thread in result.threads] == [
        "active-thread",
        "archived-thread",
    ]
    assert result.threads[0].created_at == "2024-01-01T00:00:00Z"
    assert result.threads[1].archived is True
    assert result.user_agent == "Codex Desktop/0.150.1 fixture"

    outbound = [message for message in process.requests if "method" in message]
    assert [message["method"] for message in outbound] == [
        "initialize",
        "initialized",
        "thread/read",
        "thread/list",
        "thread/list",
        "thread/list",
    ]
    list_requests = [message for message in outbound if message["method"] == "thread/list"]
    assert all(
        request["params"]["sourceKinds"] == ["cli", "vscode", "appServer"]
        for request in list_requests
    )
    assert list_requests[0]["params"]["archived"] is False
    assert list_requests[1]["params"]["cursor"] == "active-cursor"
    assert list_requests[2]["params"]["archived"] is True


def test_source_does_not_publish_partial_pages_and_reuses_last_complete_snapshot() -> None:
    first_process = JsonRpcFixtureProcess()
    source = CodexThreadSource(process_factory=lambda: first_process)

    first = source.read_snapshot()
    first_process.mode = "pagination-failure"
    second = source.read_snapshot()

    assert first.status == "ready"
    assert second.status == "stale"
    assert [thread.id for thread in second.threads] == [
        "active-thread",
        "archived-thread",
    ]
    assert second.error is not None
    assert second.error.code == "source_unavailable"
    assert "fixture page failed" in second.error.message


def test_source_reports_incompatible_when_a_minimum_method_is_missing() -> None:
    process = JsonRpcFixtureProcess(mode="missing-thread-read")
    source = CodexThreadSource(process_factory=lambda: process)

    result = source.read_snapshot()

    assert result.status == "incompatible"
    assert result.threads == ()
    assert result.error is not None
    assert result.error.code == "source_incompatible"
    assert result.error.retryable is False


def test_source_converts_process_factory_failure_to_source_unavailable() -> None:
    def failing_factory() -> Any:
        raise OSError("codex executable missing")

    result = CodexThreadSource(process_factory=failing_factory).read_snapshot()

    assert result.status == "unavailable"
    assert result.error is not None
    assert result.error.code == "source_unavailable"
    assert "codex executable missing" in result.error.message


def test_source_keeps_the_last_snapshot_when_a_later_probe_is_incompatible() -> None:
    process = JsonRpcFixtureProcess()
    source = CodexThreadSource(process_factory=lambda: process)

    first = source.read_snapshot()
    process.mode = "missing-thread-read"
    second = source.read_snapshot()

    assert first.status == "ready"
    assert second.status == "stale"
    assert [thread.id for thread in second.threads] == [
        "active-thread",
        "archived-thread",
    ]
    assert second.error is not None
    assert second.error.code == "source_incompatible"


def test_source_honors_a_mapping_shaped_capability_advertisement() -> None:
    process = JsonRpcFixtureProcess(
        capabilities={"initialize": True, "thread/list": True, "thread/read": False}
    )
    source = CodexThreadSource(process_factory=lambda: process)

    result = source.read_snapshot()

    assert result.status == "incompatible"
    assert result.error is not None
    assert result.error.details == {"missingMethods": ["thread/read"]}


def test_source_keeps_a_thread_when_its_observation_time_is_invalid() -> None:
    process = JsonRpcFixtureProcess(mode="invalid-time")
    source = CodexThreadSource(process_factory=lambda: process)

    result = source.read_snapshot()

    assert result.status == "ready"
    assert [thread.id for thread in result.threads] == [
        "active-thread",
        "archived-thread",
    ]
    assert result.threads[0].created_at is None
    assert result.threads[0].observation_error == "createdAt 无效"
