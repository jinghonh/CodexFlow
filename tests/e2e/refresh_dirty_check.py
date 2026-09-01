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
    page.get_by_label("Project root").fill(str(project))
    page.get_by_role("button", name="Load Project").click()
    page.get_by_role("table", name="Conversation list").get_by_text(
        "Fixture active conversation"
    ).wait_for()


def main() -> None:
    project = prepare_project()
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1100})
        load_project(page, project)

        page.get_by_role("table", name="Conversation list").get_by_text(
            "Fixture active conversation"
        ).click()
        detail = page.get_by_role("region", name="Conversation detail")
        title = detail.get_by_label("Custom title")
        title.fill("Keep this draft")

        page.get_by_role("button", name="Refresh source").click()
        dialog = page.get_by_role("dialog", name="Save your Graph changes first?")
        expect(dialog).to_contain_text("One unsaved Graph draft is open")
        dialog.get_by_role("button", name="Cancel").click()
        expect(title).to_have_value("Keep this draft")

        page.get_by_role("button", name="Refresh source").click()
        refresh_button = page.get_by_role("button", name="Refresh source")
        with page.expect_response(lambda response: response.url.split("?", 1)[0].endswith("/api/refresh")):
            page.get_by_role("dialog", name="Save your Graph changes first?").get_by_role(
                "button", name="Discard changes & refresh"
            ).click()
        expect(refresh_button).to_be_enabled()
        detail = page.get_by_role("region", name="Conversation detail")
        title = detail.get_by_label("Custom title")
        expect(title).to_have_value("")

        title.fill("Saved by refresh gate")
        refresh_button.click()
        page.get_by_role("dialog", name="Save your Graph changes first?").get_by_role(
            "button", name="Save changes & refresh"
        ).click()
        expect(page.get_by_text("Saved by refresh gate").first).to_be_visible()
        expect(page.get_by_role("dialog", name="Save your Graph changes first?")).not_to_be_visible()

        print("browser refresh and dirty-draft acceptance test passed")
        browser.close()


if __name__ == "__main__":
    main()
