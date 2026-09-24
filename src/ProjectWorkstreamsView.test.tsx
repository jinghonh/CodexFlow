// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
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
