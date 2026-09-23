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
list_requests = 0
analysis_thread = "temporary-analysis-thread"
analysis_turn = "temporary-analysis-turn"

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

def history_turn(turn_id, items=None, view="full"):
    return {"id": turn_id, "status": "completed", "startedAt": 100, "completedAt": 102,
            "durationMs": 2000, "itemsView": view, "items": items or []}

def history_item(item_id, kind="agentMessage"):
    return {"id": item_id, "type": kind, "text": "完整正文 " + item_id}

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
        list_requests += 1
        if mode.endswith("cancel-partial") and list_requests == 3:
            encoded = json.dumps({"id": request["id"], "result": {"marker": "cancelled-response"}})
            split = len(encoded) // 2
            sys.stdout.write(encoded[:split])
            sys.stdout.flush()
            marker_dir = pathlib.Path(__file__).parent
            time.sleep(0.05)
            (marker_dir / "partial-response-started").touch()
            while not (marker_dir / "resume-partial-response").exists():
                time.sleep(0.01)
            sys.stdout.write(encoded[split:] + "\n")
            sys.stdout.flush()
            continue
        if mode.startswith("fake-analysis-"):
            data = [thread("thread-h", "cli")]
            if mode.endswith("list-pollution"):
                data.append(thread(analysis_thread, "appServer"))
            response = {"id": request["id"], "result": {"data": data if not request["params"]["archived"] else [], "nextCursor": None}}
            print(json.dumps(response), flush=True)
            continue
        if mode.endswith("cancel-partial"):
            response = {"id": request["id"], "result": {"marker": f"request-{list_requests}", "data": [], "nextCursor": None}}
            print(json.dumps(response), flush=True)
            continue
        if mode.endswith("list-rich-exit-second") and request["params"].get("cursor") == "next-live":
            raise SystemExit(0)
        if mode.startswith("fake-gated-"):
            pause = pathlib.Path(__file__).parent / "pause-refresh"
            if pause.exists():
                pause.with_name("refresh-paused").touch()
                while pause.exists():
                    time.sleep(0.01)
        if mode.endswith("list-rich-second-gate") and request["params"].get("cursor") == "next-live":
            pause = pathlib.Path(__file__).parent / "pause-refresh"
            pause.with_name("refresh-paused").touch()
            while pause.exists():
                time.sleep(0.01)
        assert request["params"]["sourceKinds"] == ["cli", "vscode", "exec", "appServer", "subAgent", "subAgentReview", "subAgentCompact", "subAgentThreadSpawn", "subAgentOther", "unknown"]
        params = request["params"]
        if mode.endswith("list-partial") and params.get("cursor") == "next-live":
            response = {"id": request["id"], "error": {"code": -32000, "message": "page failed"}}
        elif mode.endswith(("list-rich", "list-partial", "list-moved", "list-rich-second-gate", "list-rich-exit-second")):
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
    elif (mode.startswith("fake-history-") or mode.startswith("fake-analysis-")) and method in ("thread/read", "thread/turns/list", "thread/items/list") and request["params"]["threadId"] != "00000000-0000-4000-8000-000000000000":
        params = request["params"]
        thread_id = params["threadId"]
        if method == "thread/read" and params.get("includeTurns") is False:
            response = {"id": request["id"], "result": {"thread": thread(thread_id, "cli")}}
        elif thread_id == "thread-bad" or mode.endswith("both-fail"):
            response = {"id": request["id"], "error": {"code": -32601, "message": "not supported yet"}}
        elif method == "thread/turns/list":
            if mode.endswith(("legacy", "summary")):
                response = {"id": request["id"], "error": {"code": -32601, "message": "not supported yet"}}
            else:
                assert params["itemsView"] == "notLoaded"
                assert params["sortDirection"] == "asc"
                assert params.get("cursor") in (None, "opaque-turn-2")
                turn_id = "turn-1" if not params.get("cursor") else "turn-2"
                response = {"id": request["id"], "result": {"data": [history_turn(turn_id, [history_item("summary-only")], "summary")],
                    "nextCursor": "opaque-turn-2" if turn_id == "turn-1" else None}}
        elif method == "thread/items/list":
            if mode.endswith("legacy"):
                response = {"id": request["id"], "error": {"code": -32601, "message": "not supported yet"}}
            elif mode.endswith("partial") and params.get("cursor") == "opaque-item-2":
                response = {"id": request["id"], "error": {"code": -32000, "message": "page failed"}}
            else:
                assert params["sortDirection"] == "asc"
                assert params.get("cursor") in (None, "opaque-item-2")
                if params.get("cursor"):
                    data = [{"turnId": "turn-2", "item": history_item("item-1") if mode.endswith("duplicate-paged") else {"id": "unknown-2", "type": "futureWidget", "payload": "opaque"}}]
                else:
                    data = [{"turnId": "turn-1", "item": history_item("item-1")}]
                response = {"id": request["id"], "result": {"data": data,
                    "nextCursor": "opaque-item-2" if not params.get("cursor") else None}}
        elif mode.endswith("partial"):
            response = {"id": request["id"], "error": {"code": -32000, "message": "read failed"}}
        else:
            legacy = thread(thread_id, "cli")
            if mode.endswith("summary"):
                legacy["turns"] = [history_turn("turn-1", [history_item("summary-only")], "summary")]
            else:
                legacy["turns"] = [history_turn("turn-1", [history_item("item-1")]),
                                   history_turn("turn-2", [history_item("item-1" if mode.endswith("duplicate-legacy") else "item-2")])]
            assert params["includeTurns"] is True
            response = {"id": request["id"], "result": {"thread": legacy}}
    elif mode.startswith("fake-analysis-") and method == "config/read":
        home = pathlib.Path(os.environ["CODEX_HOME"])
        assert pathlib.Path.cwd() != home
        assert not (home / "auth.json").exists()
        keys = ("shell_tool", "unified_exec", "apps", "hooks", "multi_agent", "remote_plugin", "plugins", "view_image",
                "browser_use", "browser_use_external", "browser_use_full_cdp_access", "computer_use",
                "in_app_browser", "in_app_local_automation", "image_generation", "shell_snapshot", "skill_search",
                "skill_mcp_dependency_install", "tool_call_mcp_elicitation", "tool_suggest", "workspace_dependencies", "goals")
        features = {key: False for key in keys}
        if mode.endswith("unsafe-config"):
            features["shell_tool"] = True
        if mode.endswith("unified-exec-on"):
            features["unified_exec"] = True
        response = {"id":request["id"], "result":{"config":{"features":features,"web_search":"disabled",
            "cli_auth_credentials_store":"keyring","mcp_servers":{}}}}
    elif mode.startswith("fake-analysis-") and method == "thread/start":
        if mode.endswith(("unsafe-config", "unified-exec-on")):
            (pathlib.Path(__file__).parent / "unexpected-thread-start").touch()
        assert request["params"]["ephemeral"] is True
        assert request["params"]["sandbox"] == "read-only"
        assert request["params"]["approvalPolicy"] == "never"
        assert pathlib.Path(os.environ["CODEX_HOME"]) != pathlib.Path.home() / ".codex"
        if mode.endswith("model-fail"):
            assert request["params"]["model"] == "unsupported"
        sandbox = {"type":"readOnly","networkAccess":False}
        if mode.endswith("network-missing"):
            sandbox.pop("networkAccess")
        elif mode.endswith("network-null"):
            sandbox["networkAccess"] = None
        elif mode.endswith("network-string"):
            sandbox["networkAccess"] = "false"
        elif mode.endswith("network-true"):
            sandbox["networkAccess"] = True
        response = {"id": request["id"], "result": {"thread": {"id": analysis_thread, "ephemeral": True}, "sandbox": sandbox, "approvalPolicy": "never", "model": "test-model"}}
    elif mode.startswith("fake-analysis-") and method == "turn/start":
        if mode.endswith(("network-missing", "network-null", "network-string", "network-true")):
            (pathlib.Path(__file__).parent / "unexpected-turn-start").touch()
        assert request["params"]["threadId"] == analysis_thread
        assert request["params"]["outputSchema"]["required"] == ["goal", "activity", "outcome", "decisions", "issues", "evidenceIds"]
        response = {"id": request["id"], "result": {"turn": {"id": analysis_turn, "status": "inProgress", "items": []}}}
        print(json.dumps(response), flush=True)
        if mode.endswith("flaky") and not (pathlib.Path(__file__).parent / "first-failure").exists():
            (pathlib.Path(__file__).parent / "first-failure").touch()
            raise SystemExit(0)
        if mode.endswith("internal-retry"):
            print(json.dumps({"method":"error","params":{"threadId":analysis_thread,"turnId":analysis_turn,
                "willRetry":True,"error":{"message":"temporary","codexErrorInfo":"serverOverloaded"}}}), flush=True)
            continue
        if mode.endswith("slow-success"):
            time.sleep(0.2)
        if mode.endswith(("cancel", "late")) and not (mode.endswith("once-late") and (pathlib.Path(__file__).parent / "once-cancelled").exists()):
            continue
        if mode.endswith("model-fail"):
            message = json.dumps({"error":{"message":"The 'unsupported' model is not supported when using Codex with a ChatGPT account."}})
            print(json.dumps({"method":"turn/completed","params":{"threadId":analysis_thread,"turn":{"id":analysis_turn,"status":"failed","items":[],"error":{"message":message}}}}), flush=True)
            continue
        if mode.endswith("quota"):
            print(json.dumps({"method":"turn/completed","params":{"threadId":analysis_thread,"turn":{"id":analysis_turn,"status":"failed","items":[],"error":{"message":"quota","codexErrorInfo":"usageLimitExceeded"}}}}), flush=True)
            continue
        if mode.endswith("tool"):
            print(json.dumps({"method":"item/started","params":{"threadId":analysis_thread,"turnId":analysis_turn,"item":{"type":"commandExecution","id":"cmd-1"}}}), flush=True)
            continue
        content = "{}" if mode.endswith("invalid") else json.dumps({"goal":"实现测试", "activity":"运行检查", "outcome":"通过", "decisions":"采用临时会话", "issues":"未知", "evidenceIds":["hallucinated" if mode.endswith("bad-evidence") else "item:turn-1:item-1"]}, ensure_ascii=False)
        print(json.dumps({"method":"item/completed","params":{"threadId":analysis_thread,"turnId":analysis_turn,"item":{"type":"agentMessage","id":"msg-1","phase":"final_answer","text":content}}}), flush=True)
        print(json.dumps({"method":"turn/completed","params":{"threadId":analysis_thread,"turn":{"id":analysis_turn,"status":"completed","items":[]}}}), flush=True)
        continue
    elif mode.startswith("fake-analysis-") and method == "turn/interrupt":
        assert request["params"] == {"threadId":analysis_thread,"turnId":analysis_turn}
        if mode.endswith("internal-retry"):
            (pathlib.Path(__file__).parent / "retry-interrupted").touch()
        if mode.endswith("once-late"):
            (pathlib.Path(__file__).parent / "once-cancelled").touch()
        print(json.dumps({"id":request["id"],"result":{}}), flush=True)
        if mode.endswith("late"):
            print(json.dumps({"method":"item/completed","params":{"threadId":analysis_thread,"turnId":analysis_turn,"item":{"type":"agentMessage","id":"late","phase":"final_answer","text":json.dumps({"goal":"迟到","activity":"迟到","outcome":"迟到","decisions":"迟到","issues":"迟到","evidenceIds":["item:turn-1:item-1"]},ensure_ascii=False)}}}), flush=True)
            state = "completed"
        else:
            state = "interrupted"
        print(json.dumps({"method":"turn/completed","params":{"threadId":analysis_thread,"turn":{"id":analysis_turn,"status":state,"items":[]}}}), flush=True)
        continue
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
