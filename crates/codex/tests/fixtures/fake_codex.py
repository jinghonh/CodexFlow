#!/usr/bin/env python3
import json
import pathlib
import sys

mode = pathlib.Path(__file__).stem
if sys.argv[1:] == ["--version"]:
    print("codex-cli test")
    raise SystemExit(0)
if sys.argv[1:4] == ["app-server", "generate-json-schema", "--experimental"]:
    out = pathlib.Path(sys.argv[-1]) / "v2"
    out.mkdir(parents=True)
    (out / "ThreadStartParams.json").write_text(json.dumps({"properties": {"ephemeral": {"type": "boolean"}}}))
    (out / "TurnStartParams.json").write_text(json.dumps({"properties": {"outputSchema": {"type": "object"}}}))
    raise SystemExit(0)
if sys.argv[1:] != ["app-server"]:
    raise SystemExit(2)

initialized = False
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
        response = {"id": request["id"], "result": {"data": [], "nextCursor": None}}
    elif method in ("thread/read", "thread/turns/list", "thread/items/list"):
        assert request["params"]["threadId"] == "00000000-0000-4000-8000-000000000000"
        response = {"id": request["id"], "error": {"code": -32001, "message": "not found"}}
    else:
        raise AssertionError("unexpected method: " + method)
    print(json.dumps(response), flush=True)
