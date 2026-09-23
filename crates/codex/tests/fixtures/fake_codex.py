#!/usr/bin/env python3
import json
import os
import pathlib
import sys
import time

mode = pathlib.Path(__file__).stem
if sys.argv[1:] == ["--version"]:
    if mode.endswith("slow-aux"):
        with (pathlib.Path(__file__).parent / "auxiliary-pids.txt").open("a") as marker:
            marker.write(f"version {os.getpid()}\n")
        time.sleep(60)
    print("codex-cli test")
    raise SystemExit(0)
if sys.argv[1:4] == ["app-server", "generate-json-schema", "--experimental"]:
    if mode.endswith("slow-aux"):
        with (pathlib.Path(__file__).parent / "auxiliary-pids.txt").open("a") as marker:
            marker.write(f"schema {os.getpid()}\n")
        time.sleep(60)
    out = pathlib.Path(sys.argv[-1]) / "v2"
    out.mkdir(parents=True)
    (out / "ThreadStartParams.json").write_text(json.dumps({"properties": {"ephemeral": {"type": "boolean"}}}))
    (out / "TurnStartParams.json").write_text(json.dumps({"properties": {"outputSchema": {"type": "object"}}}))
    raise SystemExit(0)
if sys.argv[1:] != ["app-server"]:
    raise SystemExit(2)

initialized = False

def thread(thread_id, source, archived=False):
    return {
        "id": thread_id,
        "sessionId": "session-" + thread_id,
        "name": "会话 " + thread_id,
        "preview": "示例工作",
        "cwd": "/tmp/example-project",
        "projectId": "project-one",
        "source": source,
        "threadSource": "test",
        "parentThreadId": "thread-a" if thread_id == "thread-b" else None,
        "forkedFromId": "thread-a" if thread_id == "thread-c" else None,
        "gitInfo": {"branch": "main", "sha": "abc", "originUrl": "example"},
        "createdAt": 100,
        "updatedAt": 200,
        "ephemeral": False,
        "cliVersion": "test",
        "modelProvider": "openai",
        "status": {"type": "notLoaded"},
        "turns": [],
    }

for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    if method == "initialized":
        initialized = True
        if mode.endswith("exit"):
            raise SystemExit(0)
        continue
    if method == "initialize":
        response = {"id": request["id"], "result": {"codexHome": "/tmp", "platformFamily": "unix", "platformOs": "macos", "userAgent": "fake-codex"}}
    elif not initialized:
        response = {"id": request["id"], "error": {"code": -32000, "message": "initialized missing"}}
    elif method == "thread/list" and mode.endswith("missing-list"):
        response = {"id": request["id"], "error": {"code": -32601, "message": "method missing"}}
    elif method == "thread/list" and mode.endswith("unknown-method"):
        response = {"id": request["id"], "error": {"code": -32600, "message": "Invalid request: unknown variant `thread/list`"}}
    elif method == "thread/list":
        assert request["params"]["sourceKinds"] == ["cli", "vscode", "exec", "appServer", "subAgent", "subAgentReview", "subAgentCompact", "subAgentThreadSpawn", "subAgentOther", "unknown"]
        params = request["params"]
        if mode.endswith("list-partial") and params.get("cursor") == "next-live":
            response = {"id": request["id"], "error": {"code": -32000, "message": "page failed"}}
        elif mode.endswith(("list-rich", "list-partial", "list-moved")):
            if mode.endswith("list-moved") and params["archived"]:
                data = [thread("thread-a", "cli", True), thread("thread-b", {"subAgent": "review"}, True), thread("thread-d", "exec", True)]
                next_cursor = None
            elif mode.endswith("list-moved"):
                data = [thread("thread-c", "appServer")]
                next_cursor = None
            elif params["archived"]:
                data = [thread("thread-a", "cli", True), thread("thread-d", "exec", True)]
                next_cursor = None
            elif params.get("cursor") == "next-live":
                data = [thread("thread-b", {"subAgent": "review"}), thread("thread-c", "appServer")]
                next_cursor = None
            else:
                data = [thread("thread-a", "cli"), thread("thread-b", {"subAgent": "review"})]
                next_cursor = "next-live"
            response = {"id": request["id"], "result": {"data": data, "nextCursor": next_cursor}}
        else:
            response = {"id": request["id"], "result": {"data": [], "nextCursor": None}}
    elif method in ("thread/read", "thread/turns/list", "thread/items/list"):
        thread_id = request["params"]["threadId"]
        if thread_id == "thread-b":
            response = {"id": request["id"], "error": {"code": -32000, "message": "single read failed"}}
        elif thread_id == "00000000-0000-4000-8000-000000000000":
            response = {"id": request["id"], "error": {"code": -32001, "message": "not found"}}
        else:
            response = {"id": request["id"], "result": {"thread": thread(thread_id, "cli")}}
    else:
        raise AssertionError("unexpected method: " + method)
    print(json.dumps(response), flush=True)
