"""Playwright acceptance test for source refresh and dirty Graph drafts.

Run through the local Vite dev server and the fixture-backed HTTP server.
"""

from __future__ import annotations

import os
from pathlib import Path

from playwright.sync_api import Page, expect, sync_playwright


def prepare_project() -> Path:
    project = Path(os.environ.get("CODEXFLOW_BROWSER_PROJECT_ROOT", "/tmp/codexflow-t9-project"))
    graph_directory = project / ".codex"
    graph_directory.mkdir(parents=True, exist_ok=True)
    (graph_directory / "graph.yaml").write_text("version: 1\n", encoding="utf-8")
    return project


def load_project(page: Page, project: Path) -> None:
    page.goto("http://127.0.0.1:5173", wait_until="networkidle")
    page.get_by_label("项目目录").fill(str(project))
    page.get_by_role("button", name="加载项目").click()
    page.get_by_role("table", name="任务列表").get_by_text(
        "Fixture active conversation"
    ).wait_for()


def main() -> None:
    project = prepare_project()
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1100})
        load_project(page, project)

        page.get_by_role("table", name="任务列表").get_by_text(
            "Fixture active conversation"
        ).click()
        detail = page.get_by_role("region", name="任务详情")
        title = detail.get_by_label("自定义标题")
        title.fill("Keep this draft")

        page.get_by_role("button", name="刷新来源").click()
        dialog = page.get_by_role("dialog", name="如何处理未保存的修改？")
        expect(dialog).to_contain_text("共 1 项未保存修改")
        dialog.get_by_role("button", name="取消").click()
        expect(title).to_have_value("Keep this draft")

        conversations = page.get_by_role("table", name="任务列表")
        conversations.get_by_text("fixture-archived-thread", exact=True).click()
        detail.get_by_label("自定义标题").fill("Keep another draft")
        conversations.get_by_text("fixture-active-thread", exact=True).click()
        expect(detail.get_by_label("自定义标题")).to_have_value("Keep this draft")

        page.get_by_role("button", name="刷新来源").click()
        expect(dialog).to_contain_text("共 2 项未保存修改")
        expect(dialog.get_by_role("list", name="未保存修改").get_by_role("listitem")).to_have_count(2)
        refresh_button = page.get_by_role("button", name="刷新来源")
        with page.expect_response(lambda response: response.url.split("?", 1)[0].endswith("/api/refresh")):
            page.get_by_role("dialog", name="如何处理未保存的修改？").get_by_role(
                "button", name="丢弃修改并刷新"
            ).click()
        expect(refresh_button).to_be_enabled()
        detail = page.get_by_role("region", name="任务详情")
        title = detail.get_by_label("自定义标题")
        expect(title).to_have_value("")

        title.fill("Saved by refresh gate")
        refresh_button.click()
        page.get_by_role("dialog", name="如何处理未保存的修改？").get_by_role(
            "button", name="保存并刷新"
        ).click()
        expect(page.get_by_text("Saved by refresh gate").first).to_be_visible()
        expect(page.get_by_role("dialog", name="如何处理未保存的修改？")).not_to_be_visible()

        graph = page.get_by_role("region", name="任务关系图")
        graph.get_by_role("button", name="放大").click()
        canvas = graph.get_by_role("application", name="关系画布")
        zoom = canvas.get_attribute("data-zoom")
        refresh_button.click()
        expect(refresh_button).to_be_enabled()
        expect(canvas).to_have_attribute("data-zoom", zoom)

        print("browser refresh and dirty-draft acceptance test passed")
        browser.close()


if __name__ == "__main__":
    main()
