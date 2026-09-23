"""Playwright smoke test for the fixture-backed Dashboard."""

from __future__ import annotations

import os
from pathlib import Path

from playwright.sync_api import sync_playwright


def main() -> None:
    project_root = os.environ.get("CODEXFLOW_BROWSER_PROJECT_ROOT", str(Path.cwd()))
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1000})
        page.goto("http://127.0.0.1:5173", wait_until="networkidle")
        assert page.get_by_text("本地服务已就绪").is_visible()
        page.get_by_label("项目目录").fill(project_root)
        page.get_by_role("button", name="加载项目").click()
        conversation_list = page.get_by_role("table", name="任务列表")
        conversation_list.get_by_text("Fixture active conversation").wait_for()
        assert conversation_list.get_by_text("fixture-active-thread").is_visible()
        assert conversation_list.get_by_text("fixture-archived-thread").is_visible()
        assert conversation_list.get_by_text("已归档", exact=True).is_visible()
        page.screenshot(path="/tmp/codexflow-dashboard-fixture.png", full_page=True)
        print("browser fixture smoke test passed")
        browser.close()


if __name__ == "__main__":
    main()
