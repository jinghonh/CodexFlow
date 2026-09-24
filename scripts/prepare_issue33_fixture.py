#!/usr/bin/env python3
"""Create disposable Git and Codex protocol fixtures for #33 desktop acceptance."""

import argparse
import os
import pathlib
import shutil
import subprocess


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=pathlib.Path, help="new, empty fixture directory")
    args = parser.parse_args()

    root = args.root.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=False)
    git_executable = shutil.which("git", path=os.defpath)
    if git_executable is None:
        raise RuntimeError("system Git is required to prepare the #33 fixture")
    git_home = root / "git-home"
    template = root / "empty-template"
    hooks = root / "empty-hooks"
    for directory in (git_home, git_home / "config", template, hooks, root / "tmp"):
        directory.mkdir()
    git_env = {
        "PATH": os.defpath,
        "HOME": str(git_home),
        "XDG_CONFIG_HOME": str(git_home / "config"),
        "TMPDIR": str(root / "tmp"),
        "LC_ALL": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TEMPLATE_DIR": str(template),
    }

    def git(*command: str) -> None:
        subprocess.run(
            [git_executable, "-c", f"core.hooksPath={hooks}", *command],
            check=True, stdout=subprocess.DEVNULL, env=git_env,
        )

    project = root / "project"
    worktree = root / "worktree"
    project.mkdir()
    git("init", "-q", str(project))
    git("-C", str(project), "-c", "user.name=CodexFlow Acceptance",
        "-c", "user.email=acceptance@example.invalid", "commit", "-q", "--allow-empty", "-m", "fixture")
    git("-C", str(project), "worktree", "add", "-q", "-b", "acceptance-worktree", str(worktree))

    source = pathlib.Path(__file__).resolve().parents[1] / "crates/codex/tests/fixtures/fake_codex.py"
    fixture = source.read_text()
    original_path = '"cwd": "/tmp/example-project",'
    history_mode = 'mode.startswith("fake-history-") or mode.startswith("fake-analysis-")'
    if fixture.count(original_path) != 1 or fixture.count(history_mode) != 1:
        raise RuntimeError("Codex fixture contract changed; inspect it before preparing #33 data")
    fixture = fixture.replace(
        original_path,
        f'"cwd": {str(worktree)!r} if thread_id in ("thread-b", "thread-c") else {str(project)!r},',
    ).replace(history_mode, history_mode + ' or mode.endswith("list-rich")')
    binary = root / "fake-list-rich"
    binary.write_text(fixture)
    binary.chmod(0o700)
    (root / "codex-home").mkdir()
    print(f"项目：{project}\n工作树：{worktree}\n合成 Codex 二进制：{binary}\n独立 CODEX_HOME：{root / 'codex-home'}")


if __name__ == "__main__":
    main()
