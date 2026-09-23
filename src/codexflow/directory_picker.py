from __future__ import annotations

import platform
import shutil
import subprocess


class DirectoryPickerUnavailableError(Exception):
    pass


def pick_directory() -> str | None:
    """Ask the local desktop to choose a directory; return None on cancellation."""
    system = platform.system()
    if system == "Darwin":
        command = ["osascript", "-e", 'POSIX path of (choose folder with prompt "选择项目目录")']
    elif system == "Linux" and shutil.which("zenity"):
        command = ["zenity", "--file-selection", "--directory", "--title=选择项目目录"]
    else:
        raise DirectoryPickerUnavailableError("此系统没有可用的目录选择窗口，请粘贴项目路径。")

    try:
        result = subprocess.run(command, capture_output=True, text=True, check=False, timeout=300)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise DirectoryPickerUnavailableError("无法打开目录选择窗口，请粘贴项目路径。") from exc
    if result.returncode != 0:
        if (system == "Darwin" and "-128" in result.stderr) or (system == "Linux" and result.returncode == 1):
            return None
        raise DirectoryPickerUnavailableError("目录选择窗口未能完成，请粘贴项目路径。")
    return result.stdout.strip() or None
