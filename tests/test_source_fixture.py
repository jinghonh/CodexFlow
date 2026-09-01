from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

from codexflow.source import CodexThreadSource

FIXTURE = Path(__file__).parent / "fixtures" / "app_server_fixture.py"


def fixture_source(mode: str) -> CodexThreadSource:
    def start() -> subprocess.Popen[str]:
        environment = os.environ.copy()
        environment["CODEXFLOW_FIXTURE_MODE"] = mode
        return subprocess.Popen(
            [sys.executable, str(FIXTURE)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
            env=environment,
        )

    return CodexThreadSource(process_factory=start)


def test_fixture_provides_real_stdio_active_and_archived_pages() -> None:
    result = fixture_source("success").read_snapshot()

    assert result.status == "ready"
    assert [thread.id for thread in result.threads] == [
        "fixture-active-thread",
        "fixture-archived-thread",
    ]


def test_fixture_startup_failure_is_source_unavailable() -> None:
    result = fixture_source("startup-failure").read_snapshot()

    assert result.status == "unavailable"
    assert result.threads == ()
    assert result.error is not None
    assert result.error.code == "source_unavailable"


def test_fixture_pagination_failure_does_not_publish_a_partial_snapshot() -> None:
    result = fixture_source("pagination-failure").read_snapshot()

    assert result.status == "unavailable"
    assert result.threads == ()
    assert result.error is not None
    assert result.error.code == "source_unavailable"


def test_fixture_missing_minimum_capability_is_incompatible() -> None:
    result = fixture_source("missing-capability").read_snapshot()

    assert result.status == "incompatible"
    assert result.threads == ()
    assert result.error is not None
    assert result.error.code == "source_incompatible"


def test_fixture_empty_lists_are_a_valid_complete_snapshot() -> None:
    result = fixture_source("empty").read_snapshot()

    assert result.status == "ready"
    assert result.threads == ()


def test_fixture_repeated_opaque_cursor_is_unavailable() -> None:
    result = fixture_source("cursor-loop").read_snapshot()

    assert result.status == "unavailable"
    assert result.threads == ()
    assert result.error is not None
    assert result.error.code == "source_unavailable"


def test_fixture_malformed_json_is_unavailable() -> None:
    result = fixture_source("malformed").read_snapshot()

    assert result.status == "unavailable"
    assert result.threads == ()
    assert result.error is not None
    assert result.error.code == "source_unavailable"
