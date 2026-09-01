"""为浏览器测试提供注入脱敏 app-server fixture 的本地 HTTP 服务。"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import uvicorn

from codexflow.app import create_app
from codexflow.source import CodexThreadSource

FIXTURE = Path(__file__).with_name("app_server_fixture.py")


def start_fixture() -> subprocess.Popen[str]:
    return subprocess.Popen(
        [sys.executable, str(FIXTURE)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        bufsize=1,
        env=os.environ.copy(),
    )

if __name__ == "__main__":
    uvicorn.run(
        create_app(source=CodexThreadSource(process_factory=start_fixture)),
        host="127.0.0.1",
        port=8000,
        log_level="warning",
    )
