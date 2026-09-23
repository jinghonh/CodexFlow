"""浅色工作区的布局、几何和交互浏览器验收；仅使用脱敏数据。"""
from __future__ import annotations
import copy
import json
import os
from pathlib import Path
from urllib.request import urlopen
from playwright.sync_api import sync_playwright


def main() -> None:
    output = Path(os.environ.get("CODEXFLOW_SCREENSHOTS", "/tmp/codexflow-workspace"))
    output.mkdir(parents=True, exist_ok=True)
    base = json.load(urlopen("http://127.0.0.1:8000/api/snapshot"))
    snapshot = copy.deepcopy(base)
    snapshot["conversations"] = []
    snapshot["graph"]["nodes"] = []
    snapshot["graph"]["edges"] = []
    snapshot["timeline"]["ranges"] = []
    titles = ["梳理项目需求与边界", "实现本地任务读取", "设计任务关系模型", "修复时间线网格错位", "检查工作区交互", "完成前端可视化验收"]
    for i in range(18):
        task = copy.deepcopy(base["conversations"][0])
        task.update(id=f"demo-{i}", displayTitle=titles[i % len(titles)] + ("：验证长标题在分屏与窄屏下的完整阅读" if i == 0 else f" · {i + 1}"))
        task["overlay"].update(status=["active", "done", "blocked"][i % 3], tags=["前端", "可视化"], layout=None)
        snapshot["conversations"].append(task)
        snapshot["graph"]["nodes"].append(dict(id=task["id"], displayTitle=task["displayTitle"], missing=False, hidden=False, layout=None))
        interval = copy.deepcopy(base["timeline"]["ranges"][0])
        interval["conversationId"] = task["id"]
        snapshot["timeline"]["ranges"].append(interval)
        if i % 6:
            snapshot["graph"]["edges"].append(dict(id=f"edge-{i}", source=f"demo-{i-1}", target=f"demo-{i}", type="depends_on", label="依赖前序工作"))
    snapshot["graph"]["edges"].append(dict(id="parallel", source="demo-0", target="demo-1", type="references", label="参考实现"))
    errors = []
    with sync_playwright() as runner:
        browser = runner.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1600, "height": 1100}, reduced_motion="reduce")
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.route("**/api/snapshot*", lambda route: route.fulfill(json=snapshot))
        saved = []
        def save_node(route):
            payload = route.request.post_data_json
            saved.append(payload)
            result = copy.deepcopy(snapshot)
            result["graph"]["etag"] = "browser-saved"
            route.fulfill(json=result)
        page.route("**/api/graph/nodes/*", save_node)
        page.goto("http://127.0.0.1:5173", wait_until="networkidle")
        page.get_by_label("项目目录", exact=True).fill("/tmp/codexflow-fixture-project")
        page.get_by_role("button", name="加载项目", exact=True).click()
        page.locator(".graph-node").first.wait_for()
        page.get_by_label("布局预设", exact=True).select_option("compare")
        page.get_by_role("button", name="显示全部", exact=True).click()
        page.locator(".graph-node").first.click()
        assert page.get_by_role("region", name="任务详情", exact=True).is_visible()
        assert page.locator(".graph-edge-label").count() == 16
        paths = page.locator("path.graph-edge").evaluate_all("els => els.map(e => e.getAttribute('d'))")
        assert len(set(paths)) == len(paths)
        assert page.locator(".timeline-grid").evaluate("el => getComputedStyle(el).display") == "grid"
        page.screenshot(path=str(output / "desktop.png"), full_page=True)
        page.get_by_role("button", name="关闭详情 ×", exact=True).click()
        # 网格表头与任务轨道起点对齐；时间条位于轨道内。
        first_bucket = page.locator(".timeline-bucket").first.bounding_box()
        track = page.locator(".timeline-track").first.bounding_box()
        bar = page.locator(".timeline-bar").first.bounding_box()
        assert abs(first_bucket["x"] - track["x"]) < 1
        assert track["y"] <= bar["y"] < track["y"] + track["height"]
        page.get_by_role("button", name="关系图后移", exact=True).click()
        page.reload(wait_until="networkidle")
        page.get_by_label("项目目录", exact=True).fill("/tmp/codexflow-fixture-project")
        page.get_by_role("button", name="加载项目", exact=True).click()
        page.locator(".graph-node").first.wait_for()
        assert page.locator(".panel-handle > strong").first.inner_text() == "时间线"
        page.set_viewport_size({"width": 390, "height": 844})
        page.get_by_role("button", name="显示全部", exact=True).click()
        page.screenshot(path=str(output / "mobile.png"), full_page=True)
        assert page.evaluate("document.documentElement.scrollWidth <= window.innerWidth"), "窄屏页面出现整体横向溢出"
        page.get_by_role("button", name="手动布局", exact=True).click()
        canvas = page.get_by_role("application", name="关系画布", exact=True)
        node = page.locator(".graph-node").first
        node.scroll_into_view_if_needed()
        box = node.bounding_box()
        x, y = box["x"] + box["width"] / 2, box["y"] + box["height"] / 2
        session = page.context.new_cdp_session(page)
        session.send("Input.dispatchTouchEvent", {"type": "touchStart", "touchPoints": [{"x": x, "y": y}]})
        session.send("Input.dispatchTouchEvent", {"type": "touchMove", "touchPoints": [{"x": x + 30, "y": y + 20}]})
        with page.expect_response(lambda response: "/api/graph/nodes/" in response.url):
            session.send("Input.dispatchTouchEvent", {"type": "touchEnd", "touchPoints": []})
        assert saved and "layout" in saved[-1], "触屏拖动未触发位置保存"
        assert not errors, errors
        browser.close()
    print(f"工作区浏览器验收通过，截图：{output}")


if __name__ == "__main__":
    main()
