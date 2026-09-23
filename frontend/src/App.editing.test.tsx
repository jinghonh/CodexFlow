import { StrictMode } from "react";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { App } from "./App";
import type { DashboardApi } from "./api";
import type { DashboardSnapshot } from "./types";
import { apiDouble, deferred, nodeSnapshot, project, snapshot } from "./test/dashboard";

async function openApp(api: DashboardApi) {
  const user = userEvent.setup();
  render(<StrictMode><App api={api} /></StrictMode>);
  await screen.findByText("本地服务已就绪");
  await user.type(screen.getByLabelText("项目目录"), project.realPath);
  await user.click(screen.getByRole("button", { name: "加载项目" }));
  await screen.findByRole("table", { name: "任务列表" });
  return user;
}

async function selectConversation(user: ReturnType<typeof userEvent.setup>, id: string) {
  await user.click(within(screen.getByRole("table", { name: "任务列表" })).getByText(id));
  return within(screen.getByRole("region", { name: "任务详情" }));
}

describe("Graph editing in the page", () => {
  it("retains drafts across selection and lists all of them before refresh", async () => {
    const api = apiDouble();
    const user = await openApp(api);
    let detail = await selectConversation(user, "active-id");
    await user.type(detail.getByLabelText("自定义标题"), "First draft");
    detail = await selectConversation(user, "archived-id");
    await user.type(detail.getByLabelText("自定义标题"), "Second draft");
    detail = await selectConversation(user, "active-id");
    expect(detail.getByLabelText("自定义标题")).toHaveValue("First draft");
    expect(window.dispatchEvent(new Event("beforeunload", { cancelable: true }))).toBe(false);
    await user.click(screen.getByRole("button", { name: "刷新来源" }));
    const dialog = within(screen.getByRole("dialog"));
    expect(dialog.getByRole("list", { name: "未保存修改" }).children).toHaveLength(2);
    expect(api.refresh).not.toHaveBeenCalled();
    await user.click(dialog.getByRole("button", { name: "丢弃修改并刷新" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(window.dispatchEvent(new Event("beforeunload", { cancelable: true }))).toBe(true);
    expect(detail.getByLabelText("自定义标题")).toHaveValue("");
  });

  it("keeps later input editable and dirty after an earlier save finishes", async () => {
    const response = deferred<DashboardSnapshot>();
    const api = apiDouble({ updateNode: vi.fn().mockReturnValueOnce(response.promise)
      .mockResolvedValueOnce(nodeSnapshot("active-id", { title: "Submitted later" }, "v2")) });
    const user = await openApp(api);
    const detail = await selectConversation(user, "active-id");
    const input = detail.getByLabelText("自定义标题");
    await user.type(input, "Submitted");
    await user.click(detail.getByRole("button", { name: "保存修改" }));
    expect(input).toBeEnabled();
    await user.type(input, " later");
    await act(async () => response.resolve(nodeSnapshot("active-id", { title: "Submitted" }, "v1")));
    expect(input).toHaveValue("Submitted later");
    expect(detail.queryByText("修改已保存")).not.toBeInTheDocument();
    expect(screen.getByText("存在未保存修改")).toBeInTheDocument();
    await user.click(detail.getByRole("button", { name: "保存修改" }));
    await screen.findByText("修改已保存");
    expect(screen.getByText("来源同步就绪")).toBeInTheDocument();
  });

  it("guards project switching and retains the current project after a save failure", async () => {
    const api = apiDouble({ updateNode: vi.fn().mockRejectedValue(new Error("Disk full")) });
    const user = await openApp(api);
    const detail = await selectConversation(user, "active-id");
    await user.type(detail.getByLabelText("自定义标题"), "Keep this draft");
    await user.clear(screen.getByLabelText("项目目录"));
    await user.type(screen.getByLabelText("项目目录"), "/projects/other");
    await user.click(screen.getByRole("button", { name: "加载项目" }));
    const dialog = within(screen.getByRole("dialog"));
    await user.click(dialog.getByRole("button", { name: "保存并切换项目" }));
    expect(await dialog.findByRole("alert")).toHaveTextContent("Disk full");
    expect(api.selectProject).toHaveBeenCalledTimes(1);
    await user.click(dialog.getByRole("button", { name: "取消" }));
    expect(detail.getByLabelText("自定义标题")).toHaveValue("Keep this draft");
  });

  it("preserves graph zoom and pan after a source refresh", async () => {
    const api = apiDouble();
    const user = await openApp(api);
    const graph = within(screen.getByRole("region", { name: "任务关系图" }));
    await user.click(graph.getByRole("button", { name: "放大" }));
    const canvas = graph.getByRole("application", { name: "关系画布" });
    fireEvent.pointerDown(canvas, { clientX: 100, clientY: 100, button: 0 });
    fireEvent.pointerMove(canvas, { clientX: 160, clientY: 140 });
    fireEvent.pointerUp(canvas);
    await user.click(screen.getByRole("button", { name: "刷新来源" }));
    await waitFor(() => expect(api.refresh).toHaveBeenCalledTimes(1));
    expect(canvas).toHaveAttribute("data-zoom", "1.1");
    expect(canvas).toHaveAttribute("data-pan-x", "60");
    expect(canvas).toHaveAttribute("data-pan-y", "40");
  });

  it("protects a failed layout through the same refresh decision", async () => {
    const api = apiDouble({ updateNode: vi.fn().mockRejectedValue(new Error("Read only")) });
    const user = await openApp(api);
    const graph = within(screen.getByRole("region", { name: "任务关系图" }));
    const canvas = graph.getByRole("application", { name: "关系画布" });
    const node = graph.getByRole("button", { name: /Map the local runtime/ });
    await user.click(screen.getByRole("button", { name: "手动布局" }));
    fireEvent.pointerDown(node, { clientX: 50, clientY: 50, button: 0 });
    fireEvent.pointerMove(canvas, { clientX: 90, clientY: 70 });
    fireEvent.pointerUp(canvas);
    await graph.findByRole("alert");
    await user.click(screen.getByRole("button", { name: "刷新来源" }));
    const dialog = within(screen.getByRole("dialog"));
    expect(dialog.getByRole("list")).toHaveTextContent("节点位置");
    expect(api.refresh).not.toHaveBeenCalled();
    await user.click(dialog.getByRole("button", { name: "丢弃修改并刷新" }));
    await waitFor(() => expect(node).toHaveStyle({ left: "32px", top: "34px" }));
  });

  it("keeps editing the saved relationship instead of resetting later input", async () => {
    const response = deferred<DashboardSnapshot>();
    const edge = { id: "new-edge", source: "active-id", target: "archived-id", type: "continues", label: "Submitted" };
    const api = apiDouble({ createEdge: vi.fn().mockReturnValue(response.promise), updateEdge: vi.fn().mockResolvedValue({
      ...snapshot, graph: { ...snapshot.graph, etag: "v2", edges: [{ ...edge, label: "Submitted later" }] },
    }) });
    const user = await openApp(api);
    const form = within(screen.getByRole("region", { name: "关系编辑器" }));
    await user.type(form.getByLabelText("来源"), "active-id");
    await user.type(form.getByLabelText("目标"), "archived-id");
    await user.type(form.getByLabelText(/说明/), "Submitted");
    await user.click(form.getByRole("button", { name: "添加关系" }));
    await user.type(form.getByLabelText(/说明/), " later");
    await act(async () => response.resolve({ ...snapshot, graph: { ...snapshot.graph, etag: "v1", edges: [edge] } }));
    expect(form.getByLabelText(/说明/)).toHaveValue("Submitted later");
    await user.click(form.getByRole("button", { name: "保存关系" }));
    expect(api.createEdge).toHaveBeenCalledTimes(1);
    expect(api.updateEdge).toHaveBeenCalledWith("new-edge", expect.objectContaining({ label: "Submitted later" }), "v1");
  });
});
