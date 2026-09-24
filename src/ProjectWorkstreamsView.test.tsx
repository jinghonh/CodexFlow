// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ProjectWorkstreamsView } from "./ProjectWorkstreamsView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

test("工作流导航保留未分组会话、跨组边和命名失败的分组", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_project_workstreams") return {
      workstreams: [{ id: "stream", name: "工作流 1234", members: ["a", "b"], relationIds: ["ab"],
        nameInputVersion: null, nameError: "命名失败" }],
      ungroupedThreadIds: ["c"], crossRelationIds: ["bc"],
    };
    if (command === "get_project_graph") return {
      nodes: [{ id: "a", title: "起点" }, { id: "b", title: "实现" }, { id: "c", title: "旁支" }],
      relations: [{ id: "ab", fromThreadId: "a", toThreadId: "b", kind: "FORKED_FROM", source: "observed" }],
      derivedRelations: [{ id: "bc", fromThreadId: "b", toThreadId: "c", kind: "SHARED_FILE", source: "derived", basis: "共同修改文件" }],
      inferredRelations: [],
    };
    throw new Error(`未知命令 ${command}`);
  });
  const onSelectThread = vi.fn();
  render(<ProjectWorkstreamsView projectId="project" refreshVersion={1}
    onSelectThread={onSelectThread} onSelectEvidence={vi.fn()} />);
  expect(await screen.findByText(/命名未完成：命名失败/)).toBeTruthy();
  expect(screen.getByRole("heading", { name: "未分组会话 · 1" })).toBeTruthy();
  expect(screen.getByRole("heading", { name: "跨工作流关系" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /旁支/ }));
  expect(onSelectThread).toHaveBeenCalledWith("c");
  expect(screen.getByText(/观察关系 · FORKED_FROM/)).toBeTruthy();
});

test("改名冲突要求刷新且保留输入，随后可以保存和移动未分组会话", async () => {
  let revision = 1;
  let name = "自动名称";
  let members = ["a", "b"];
  let ungrouped = ["c"];
  let conflict = true;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_project_workstreams") return {
      revision, workstreams: [{ id: "stream", name, members, relationIds: [], nameInputVersion: "v1", nameError: null }],
      ungroupedThreadIds: ungrouped, crossRelationIds: [],
      manuallyNamedWorkstreamIds: name === "人工名称" ? ["stream"] : [],
      manuallyAssignedThreadIds: members.includes("c") ? ["c"] : [],
    };
    if (command === "get_project_graph") return {
      nodes: [{ id: "a", title: "起点" }, { id: "b", title: "实现" }, { id: "c", title: "旁支" }],
      relations: [], derivedRelations: [], inferredRelations: [],
    };
    if (command === "rename_workstream") {
      if (conflict) { conflict = false; revision = 2; throw { code: "CONCURRENT_MODIFICATION" }; }
      expect(args).toMatchObject({ expectedRevision: 2, name: "人工名称" });
      name = "人工名称"; revision = 3; return revision;
    }
    if (command === "move_thread_to_workstream") {
      expect(args).toMatchObject({ expectedRevision: 3, threadId: "c", targetId: "stream" });
      members = ["a", "b", "c"]; ungrouped = []; revision = 4; return revision;
    }
    if (command === "restore_workstream_name") {
      expect(args).toMatchObject({ expectedRevision: 4, workstreamId: "stream" });
      name = "自动名称"; revision = 5; return revision;
    }
    if (command === "restore_thread_workstream") {
      expect(args).toMatchObject({ expectedRevision: 5, threadId: "c" });
      members = ["a", "b"]; ungrouped = ["c"]; revision = 6; return revision;
    }
    throw new Error(`未知命令 ${command}`);
  });
  render(<ProjectWorkstreamsView projectId="project" refreshVersion={1}
    onSelectThread={vi.fn()} onSelectEvidence={vi.fn()} />);
  const input = await screen.findByLabelText("工作流名称") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "人工名称" } });
  fireEvent.click(screen.getByRole("button", { name: "保存名称" }));
  expect(await screen.findByText(/工作流已被更新。请刷新工作流后重试/)).toBeTruthy();
  expect(input.value).toBe("人工名称");
  expect(screen.queryByText("已保存。")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "刷新工作流" }));
  await waitFor(() => expect(screen.queryByText(/工作流已被更新/)).toBeNull());
  expect(input.value).toBe("人工名称");
  fireEvent.click(screen.getByRole("button", { name: "保存名称" }));
  expect(await screen.findByRole("button", { name: "恢复自动名称" })).toBeTruthy();
  fireEvent.change(screen.getByLabelText("旁支的主要工作流"), { target: { value: "stream" } });
  fireEvent.click(screen.getAllByRole("button", { name: "保存归属" }).find((button) => !button.hasAttribute("disabled"))!);
  await waitFor(() => expect(screen.getByRole("heading", { name: "未分组会话 · 0" })).toBeTruthy());
  expect(screen.getByRole("button", { name: "恢复自动归属" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "恢复自动名称" }));
  await waitFor(() => expect(screen.queryByRole("button", { name: "恢复自动名称" })).toBeNull());
  expect(screen.getByRole("heading", { name: "自动名称" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "恢复自动归属" }));
  await waitFor(() => expect(screen.getByRole("heading", { name: "未分组会话 · 1" })).toBeTruthy());
});

test("大工作流的成员与关系列表逐页显示且可继续查看", async () => {
  const members = Array.from({ length: 81 }, (_, index) => `thread-${index}`);
  const relations = Array.from({ length: 81 }, (_, index) => ({
    id: `relation-${index}`, fromThreadId: members[index], toThreadId: members[(index + 1) % members.length],
    kind: "FORKED_FROM", source: "observed",
  }));
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_project_workstreams") return {
      workstreams: [{ id: "stream", name: "大型工作流", members, relationIds: relations.map((item) => item.id),
        nameInputVersion: null, nameError: null }],
      ungroupedThreadIds: [], crossRelationIds: [], revision: 1,
      manuallyNamedWorkstreamIds: [], manuallyAssignedThreadIds: [],
    };
    if (command === "get_project_graph") return {
      nodes: members.map((id) => ({ id, title: id, referenceOnly: false })),
      relations, derivedRelations: [], inferredRelations: [],
    };
    throw new Error(`未知命令 ${command}`);
  });
  render(<ProjectWorkstreamsView projectId="project" refreshVersion={1}
    onSelectThread={vi.fn()} onSelectEvidence={vi.fn()} />);
  await screen.findByRole("heading", { name: "大型工作流" });
  expect(document.querySelectorAll(".workstream-thread")).toHaveLength(40);
  expect(document.querySelectorAll(".workstream-relations > div")).toHaveLength(40);
  fireEvent.click(screen.getByRole("button", { name: "显示更多成员（40 / 81）" }));
  fireEvent.click(screen.getByRole("button", { name: "显示更多内部关系（40 / 81）" }));
  expect(document.querySelectorAll(".workstream-thread")).toHaveLength(80);
  expect(document.querySelectorAll(".workstream-relations > div")).toHaveLength(80);
  fireEvent.click(screen.getByRole("button", { name: "显示更多成员（80 / 81）" }));
  fireEvent.click(screen.getByRole("button", { name: "显示更多内部关系（80 / 81）" }));
  expect(document.querySelectorAll(".workstream-thread")).toHaveLength(81);
  expect(document.querySelectorAll(".workstream-relations > div")).toHaveLength(81);
});
