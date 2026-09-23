"""Playwright acceptance test for shared selection and filter projections.

Run through webapp-testing/scripts/with_server.py with
tests/fixtures/http_fixture_server.py; the server injects the redacted
app-server fixture and the browser never connects to a real Codex source.
"""

from __future__ import annotations

import os
from pathlib import Path

from playwright.sync_api import Page, expect, sync_playwright


def prepare_project() -> Path:
    project = Path(os.environ.get("CODEXFLOW_BROWSER_PROJECT_ROOT", "/tmp/codexflow-t7-project"))
    graph_directory = project / ".codex"
    graph_directory.mkdir(parents=True, exist_ok=True)
    (graph_directory / "graph.yaml").write_text(
        """
version: 1
nodes:
  fixture-active-thread:
    tags: [focus]
    status: done
    hidden: true
  fixture-archived-thread:
    tags: [archive]
    status: active
  fixture-missing-thread:
    title: Orphaned fixture work
    tags: [history]
edges:
  - id: fixture-edge
    source: fixture-active-thread
    target: fixture-missing-thread
    type: references
""",
        encoding="utf-8",
    )
    return project


def load_project(page: Page, project: Path) -> None:
    page.goto("http://127.0.0.1:5173", wait_until="networkidle")
    page.get_by_label("项目目录").fill(str(project))
    page.get_by_role("button", name="加载项目").click()
    page.get_by_role("table", name="任务列表").get_by_text(
        "fixture-active-thread"
    ).wait_for()


def select_and_assert(page: Page, conversation_id: str, title: str) -> None:
    list_view = page.get_by_role("table", name="任务列表")
    graph_view = page.get_by_role("region", name="任务关系图")
    timeline_view = page.get_by_role("region", name="任务时间线")
    list_row = list_view.locator(f'[data-conversation-id="{conversation_id}"]')
    graph_node = graph_view.locator(f'[data-conversation-id="{conversation_id}"]').first
    timeline_row = timeline_view.locator(f'[data-conversation-id="{conversation_id}"]').first

    list_view.get_by_text(title).click()
    expect(list_row).to_have_attribute("aria-selected", "true")
    if graph_node.count() > 0:
        expect(graph_node).to_have_attribute("aria-pressed", "true")
    else:
        expect(graph_view.locator(f'[data-conversation-id="{conversation_id}"]')).to_have_count(0)
    expect(timeline_row).to_have_attribute("aria-pressed", "true")


def main() -> None:
    project = prepare_project()
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1100})
        load_project(page, project)

        filters = page.get_by_role("region", name="任务筛选")
        list_view = page.get_by_role("table", name="任务列表")
        graph_view = page.get_by_role("region", name="任务关系图")
        timeline_view = page.get_by_role("region", name="任务时间线")

        select_and_assert(page, "fixture-active-thread", "Fixture active conversation")

        archived_node = graph_view.locator('[data-conversation-id="fixture-archived-thread"]').first
        archived_node.click()
        expect(list_view.locator('[data-conversation-id="fixture-archived-thread"]')).to_have_attribute(
            "aria-selected", "true"
        )
        expect(timeline_view.locator('[data-conversation-id="fixture-archived-thread"]').first).to_have_attribute(
            "aria-pressed", "true"
        )

        search = filters.get_by_role("searchbox", name="搜索任务")
        search.fill("archived")
        expect(list_view.get_by_text("fixture-archived-thread")).to_be_visible()
        expect(list_view.get_by_text("fixture-active-thread")).not_to_be_visible()
        expect(graph_view.locator('[data-conversation-id="fixture-archived-thread"]').first).to_be_visible()
        expect(graph_view.locator('[data-conversation-id="fixture-active-thread"]')).to_have_count(0)
        expect(timeline_view.locator('[data-conversation-id="fixture-archived-thread"]').first).to_be_visible()
        expect(timeline_view.locator('[data-conversation-id="fixture-active-thread"]')).to_have_count(0)

        filters.get_by_role("button", name="清空筛选").click()
        filters.get_by_label("标签").select_option("archive")
        expect(list_view.get_by_text("fixture-archived-thread")).to_be_visible()
        expect(list_view.get_by_text("fixture-active-thread")).not_to_be_visible()

        filters.get_by_role("button", name="清空筛选").click()
        filters.get_by_label("来源可用性").select_option("missing")
        expect(list_view.get_by_text("fixture-missing-thread")).to_be_visible()
        expect(graph_view.get_by_text("来源缺失")).to_be_visible()
        expect(timeline_view.locator('[data-conversation-id="fixture-missing-thread"]')).to_have_count(0)

        filters.get_by_role("button", name="清空筛选").click()
        filters.get_by_label("关系状态").select_option("unlinked")
        expect(list_view.get_by_text("fixture-archived-thread")).to_be_visible()
        expect(list_view.get_by_text("fixture-active-thread")).not_to_be_visible()
        expect(list_view.get_by_text("fixture-missing-thread")).not_to_be_visible()

        filters.get_by_role("button", name="清空筛选").click()
        filters.get_by_label("图中可见性").select_option("hidden")
        expect(list_view.get_by_text("fixture-active-thread")).to_be_visible()
        expect(timeline_view.locator('[data-conversation-id="fixture-active-thread"]').first).to_be_visible()
        expect(graph_view.locator('[data-conversation-id="fixture-active-thread"]')).to_have_count(0)

        page.screenshot(path="/tmp/codexflow-t7-selection-filter.png", full_page=True)
        print("browser selection and filter acceptance test passed")
        browser.close()


if __name__ == "__main__":
    main()
