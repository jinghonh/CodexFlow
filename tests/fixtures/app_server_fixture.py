"""脱敏的 app-server stdio fixture，用于验证 JSON-RPC 读取接缝。"""

from __future__ import annotations

import json
import os
import sys
from typing import Any

MODE = os.environ.get("CODEXFLOW_FIXTURE_MODE", "success")
FIXTURE_CWD = os.environ.get("CODEXFLOW_FIXTURE_CWD", "/tmp/codexflow-fixture-project")

ACTIVE_THREAD = {
    "id": "fixture-active-thread",
    "name": "Fixture active conversation",
    "preview": "Inspect the local project",
    "createdAt": 1704067200,
    "updatedAt": 1704070800,
    "recencyAt": 1704070800,
    "cwd": FIXTURE_CWD,
    "source": "cli",
    "status": {"type": "idle"},
    "projectId": "fixture-project",
    "gitInfo": None,
    "historyMode": "paginated",
}
ARCHIVED_THREAD = {
    "id": "fixture-archived-thread",
    "name": None,
    "preview": "Review the archived result",
    "createdAt": 1704153600,
    "updatedAt": 1704153600,
    "recencyAt": None,
    "cwd": FIXTURE_CWD,
    "source": "vscode",
    "status": {"type": "notLoaded"},
    "projectId": None,
    "gitInfo": {"branch": "main"},
    "historyMode": "legacy",
}


def send(message: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def error(request_id: Any, code: int, message: str) -> None:
    send({"id": request_id, "error": {"code": code, "message": message}})


def main() -> None:
    if MODE == "startup-failure":
        return
    for line in sys.stdin:
        try:
            request = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = request.get("method")
        request_id = request.get("id")
        if method == "initialize":
            result: dict[str, Any] = {
                "userAgent": "Codex Desktop/0.150.1 fixture",
                "codexHome": "/tmp/codexflow-fixture-codex-home",
                "platformFamily": "unix",
                "platformOs": "test",
            }
            if MODE == "missing-capability":
                result["capabilities"] = {"methods": ["initialize", "thread/list"]}
            send({"id": request_id, "result": result})
        elif method == "initialized":
            continue
        elif method == "thread/read":
            if MODE == "missing-thread-read":
                error(request_id, -32601, "Method not found")
            elif request.get("params", {}).get("threadId") == "fixture-active-thread":
                send({"id": request_id, "result": {"thread": ACTIVE_THREAD}})
            else:
                error(request_id, -32004, "thread not found")
        elif method == "thread/list":
            params = request.get("params", {})
            archived = params.get("archived")
            cursor = params.get("cursor")
            if MODE == "malformed" and archived is False and cursor is None:
                sys.stdout.write("not-json-rpc\n")
                sys.stdout.flush()
            elif MODE == "pagination-failure" and archived is False and cursor == "active-next":
                error(request_id, -32001, "fixture page failed")
            elif MODE == "cursor-loop" and archived is False and cursor == "active-next":
                send({"id": request_id, "result": {"data": {"items": []}, "nextCursor": "active-next"}})
            elif archived is False and cursor is None:
                items = [] if MODE == "empty" else [ACTIVE_THREAD]
                next_cursor = None if MODE == "empty" else "active-next"
                send({"id": request_id, "result": {"data": {"items": items}, "nextCursor": next_cursor}})
            elif archived is False and cursor == "active-next":
                send({"id": request_id, "result": {"data": {"items": []}, "nextCursor": None}})
            elif archived is True and cursor is None:
                items = [] if MODE == "empty" else [ARCHIVED_THREAD]
                send({"id": request_id, "result": {"data": {"items": items}, "nextCursor": None}})
            else:
                error(request_id, -32602, "invalid thread/list cursor")


if __name__ == "__main__":
    main()
