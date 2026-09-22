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
  await screen.findByText("Local runtime ready");
  await user.type(screen.getByLabelText("Project root"), project.realPath);
  await user.click(screen.getByRole("button", { name: "Load Project" }));
  await screen.findByRole("table", { name: "Conversation list" });
  return user;
}

async function selectConversation(user: ReturnType<typeof userEvent.setup>, id: string) {
  await user.click(within(screen.getByRole("table", { name: "Conversation list" })).getByText(id));
  return within(screen.getByRole("region", { name: "Conversation detail" }));
}

describe("Graph editing in the page", () => {
  it("retains drafts across selection and lists all of them before refresh", async () => {
    const api = apiDouble();
    const user = await openApp(api);
    let detail = await selectConversation(user, "active-id");
    await user.type(detail.getByLabelText("Custom title"), "First draft");
    detail = await selectConversation(user, "archived-id");
    await user.type(detail.getByLabelText("Custom title"), "Second draft");
    detail = await selectConversation(user, "active-id");
    expect(detail.getByLabelText("Custom title")).toHaveValue("First draft");
    expect(window.dispatchEvent(new Event("beforeunload", { cancelable: true }))).toBe(false);
    await user.click(screen.getByRole("button", { name: "Refresh source" }));
    const dialog = within(screen.getByRole("dialog"));
    expect(dialog.getByRole("list", { name: "Unsaved Graph drafts" }).children).toHaveLength(2);
    expect(api.refresh).not.toHaveBeenCalled();
    await user.click(dialog.getByRole("button", { name: "Discard changes & refresh" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(window.dispatchEvent(new Event("beforeunload", { cancelable: true }))).toBe(true);
    expect(detail.getByLabelText("Custom title")).toHaveValue("");
  });

  it("keeps later input editable and dirty after an earlier save finishes", async () => {
    const response = deferred<DashboardSnapshot>();
    const api = apiDouble({ updateNode: vi.fn().mockReturnValueOnce(response.promise)
      .mockResolvedValueOnce(nodeSnapshot("active-id", { title: "Submitted later" }, "v2")) });
    const user = await openApp(api);
    const detail = await selectConversation(user, "active-id");
    const input = detail.getByLabelText("Custom title");
    await user.type(input, "Submitted");
    await user.click(detail.getByRole("button", { name: "Save changes" }));
    expect(input).toBeEnabled();
    await user.type(input, " later");
    await act(async () => response.resolve(nodeSnapshot("active-id", { title: "Submitted" }, "v1")));
    expect(input).toHaveValue("Submitted later");
    expect(detail.queryByText("Changes saved")).not.toBeInTheDocument();
    expect(screen.getByText("Unsaved Graph changes")).toBeInTheDocument();
    await user.click(detail.getByRole("button", { name: "Save changes" }));
    await screen.findByText("Changes saved");
    expect(screen.getByText("Source sync ready")).toBeInTheDocument();
  });

  it("guards project switching and retains the current project after a save failure", async () => {
    const api = apiDouble({ updateNode: vi.fn().mockRejectedValue(new Error("Disk full")) });
    const user = await openApp(api);
    const detail = await selectConversation(user, "active-id");
    await user.type(detail.getByLabelText("Custom title"), "Keep this draft");
    await user.clear(screen.getByLabelText("Project root"));
    await user.type(screen.getByLabelText("Project root"), "/projects/other");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const dialog = within(screen.getByRole("dialog"));
    await user.click(dialog.getByRole("button", { name: "Save changes & switch Project" }));
    expect(await dialog.findByRole("alert")).toHaveTextContent("Disk full");
    expect(api.selectProject).toHaveBeenCalledTimes(1);
    await user.click(dialog.getByRole("button", { name: "Cancel" }));
    expect(detail.getByLabelText("Custom title")).toHaveValue("Keep this draft");
  });

  it("preserves graph zoom and pan after a source refresh", async () => {
    const api = apiDouble();
    const user = await openApp(api);
    const graph = within(screen.getByRole("region", { name: "Conversation graph" }));
    await user.click(graph.getByRole("button", { name: "Zoom in" }));
    const canvas = graph.getByRole("application", { name: "Graph canvas" });
    fireEvent.mouseDown(canvas, { clientX: 100, clientY: 100, button: 0 });
    fireEvent.mouseMove(canvas, { clientX: 160, clientY: 140 });
    fireEvent.mouseUp(canvas);
    await user.click(screen.getByRole("button", { name: "Refresh source" }));
    await waitFor(() => expect(api.refresh).toHaveBeenCalledTimes(1));
    expect(canvas).toHaveAttribute("data-zoom", "1.1");
    expect(canvas).toHaveAttribute("data-pan-x", "60");
    expect(canvas).toHaveAttribute("data-pan-y", "40");
  });

  it("protects a failed layout through the same refresh decision", async () => {
    const api = apiDouble({ updateNode: vi.fn().mockRejectedValue(new Error("Read only")) });
    const user = await openApp(api);
    const graph = within(screen.getByRole("region", { name: "Conversation graph" }));
    const canvas = graph.getByRole("application", { name: "Graph canvas" });
    const node = graph.getByRole("button", { name: /Map the local runtime/ });
    fireEvent.mouseDown(node, { clientX: 50, clientY: 50, button: 0 });
    fireEvent.mouseMove(canvas, { clientX: 90, clientY: 70 });
    fireEvent.mouseUp(canvas);
    await graph.findByRole("alert");
    await user.click(screen.getByRole("button", { name: "Refresh source" }));
    const dialog = within(screen.getByRole("dialog"));
    expect(dialog.getByRole("list")).toHaveTextContent("layout");
    expect(api.refresh).not.toHaveBeenCalled();
    await user.click(dialog.getByRole("button", { name: "Discard changes & refresh" }));
    await waitFor(() => expect(node).toHaveStyle({ left: "32px", top: "34px" }));
  });

  it("keeps editing the saved relationship instead of resetting later input", async () => {
    const response = deferred<DashboardSnapshot>();
    const edge = { id: "new-edge", source: "active-id", target: "archived-id", type: "continues", label: "Submitted" };
    const api = apiDouble({ createEdge: vi.fn().mockReturnValue(response.promise), updateEdge: vi.fn().mockResolvedValue({
      ...snapshot, graph: { ...snapshot.graph, etag: "v2", edges: [{ ...edge, label: "Submitted later" }] },
    }) });
    const user = await openApp(api);
    const form = within(screen.getByRole("region", { name: "Relationship editor" }));
    await user.type(form.getByLabelText("Source"), "active-id");
    await user.type(form.getByLabelText("Target"), "archived-id");
    await user.type(form.getByLabelText(/Label/), "Submitted");
    await user.click(form.getByRole("button", { name: "Add relationship" }));
    await user.type(form.getByLabelText(/Label/), " later");
    await act(async () => response.resolve({ ...snapshot, graph: { ...snapshot.graph, etag: "v1", edges: [edge] } }));
    expect(form.getByLabelText(/Label/)).toHaveValue("Submitted later");
    await user.click(form.getByRole("button", { name: "Save relationship" }));
    expect(api.createEdge).toHaveBeenCalledTimes(1);
    expect(api.updateEdge).toHaveBeenCalledWith("new-edge", expect.objectContaining({ label: "Submitted later" }), "v1");
  });
});
