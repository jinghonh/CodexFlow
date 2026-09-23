"""Playwright smoke test for the unavailable-source state."""

from __future__ import annotations

import os
from pathlib import Path

from playwright.sync_api import sync_playwright


def main() -> None:
    project_root = os.environ.get("CODEXFLOW_BROWSER_PROJECT_ROOT", str(Path.cwd()))
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        page.goto("http://127.0.0.1:5173", wait_until="networkidle")
        page.get_by_label("项目目录").fill(project_root)
        page.get_by_role("button", name="加载项目").click()
        alert = page.get_by_role("alert")
        alert.wait_for()
        assert alert.get_by_text("来源不可用", exact=True).is_visible()
        browser.close()
        print("browser unavailable-source smoke test passed")


if __name__ == "__main__":
    main()
