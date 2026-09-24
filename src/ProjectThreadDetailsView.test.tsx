// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ProjectThreadDetailsView } from "./ProjectThreadDetailsView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const graph = {
  nodes: [{ id: "thread-a", referenceOnly: false }, { id: "thread-b", referenceOnly: false }],
  relations: [{ id: "relation", source: "observed", kind: "FORKED_FROM", fromThreadId: "thread-a", toThreadId: "thread-b" }],
  derivedRelations: [], inferredRelations: [],
};
const thread = {
  id: "thread-a", title: "主会话", preview: "预览", sessionId: "session-a", cwd: "/repo", sourceKind: "cli",
  sourceDetail: null, threadSource: null, parentThreadId: null, forkedFromId: null, createdAt: 1, updatedAt: 2,
  archived: false, metadataComplete: true, turnsComplete: true, itemsComplete: true, contentComplete: true,
  missingFromSource: false, readError: null, git: null,
};
const props = { projectId: "project", thread, attribution: { workspaceRoot: "/repo", detail: "Git", diagnostic: null, sourceProjectId: null },
  hidden: false, onSelectThread: vi.fn(), onSelectEvidence: vi.fn() };

test("同一项目读取关系失败时保留会话详情中的已有关系", async () => {
  vi.mocked(invoke).mockResolvedValueOnce(graph).mockRejectedValueOnce({
    code: "STORAGE_FAILED", message: "读取项目关系失败。", retryable: true,
    cachePreserved: true, nextStep: "检查应用数据目录后重试。",
  });
  const view = render(<ProjectThreadDetailsView {...props} refreshVersion={0} />);
  await screen.findByText("观察 · FORKED_FROM");
  view.rerender(<ProjectThreadDetailsView {...props} refreshVersion={1} />);
  await screen.findByText(/STORAGE_FAILED：读取项目关系失败/);
  expect(screen.getByText("观察 · FORKED_FROM")).toBeTruthy();
});
