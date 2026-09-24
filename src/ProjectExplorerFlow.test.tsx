// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { App } from "./main";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("./ProjectTimelineView", () => ({ ProjectTimelineView: ({ selectedThreadId, onSelectThread, visibleThreadIds }: {
  selectedThreadId: string | null; onSelectThread: (id: string) => void; visibleThreadIds: Set<string>;
}) => <div aria-label="时间线"><span>选择：{selectedThreadId ?? "无"}</span><span>可见：{[...visibleThreadIds].join(",")}</span><button onClick={() => onSelectThread("t1")}>选中时间线会话</button></div> }));
vi.mock("./ProjectGraphView", () => ({ ProjectGraphView: ({ selectedThreadId, onSelectThread, visibleThreadIds }: {
  selectedThreadId: string | null; onSelectThread: (id: string) => void; visibleThreadIds: Set<string>;
}) => <div aria-label="关系图"><span>选择：{selectedThreadId ?? "无"}</span><span>可见：{[...visibleThreadIds].join(",")}</span><button onClick={() => onSelectThread("t2")}>选中图会话</button></div> }));
vi.mock("./ProjectWorkstreamsView", () => ({ ProjectWorkstreamsView: () => null }));
vi.mock("./ThreadHistoryView", () => ({ ThreadHistoryView: ({ threadId }: { threadId: string }) => <div aria-label="回合事实">{threadId}</div> }));
vi.mock("./ProjectThreadDetailsView", () => ({ ProjectThreadDetailsView: ({ thread, hidden }: { thread: { id: string }; hidden: boolean }) => <div aria-label="会话详情">{thread.id} · {hidden ? "隐藏" : "可见"}</div> }));
vi.mock("./CandidatePreviewView", () => ({ CandidatePreviewView: () => null }));
vi.mock("./ProjectAnalysisView", () => ({ ProjectAnalysisView: () => null }));

afterEach(() => { cleanup(); vi.clearAllMocks(); });

const thread = (id: string) => ({ id, sessionId: `session-${id}`, title: id, preview: `预览 ${id}`, cwd: "/repo",
  projectId: "project", sourceKind: "cli", sourceDetail: null, threadSource: null, parentThreadId: null,
  forkedFromId: null, git: null, createdAt: 1, updatedAt: 2, archived: false,
  metadataComplete: true, turnsComplete: true, itemsComplete: true, missingFromSource: false,
  contentComplete: true, readError: null, observedAtUnixMs: 3 });
const attributed = (id: string) => ({ thread: thread(id), attribution: { threadId: id, projectId: "project",
  workspaceRoot: "/repo", basis: "git", detail: "Git", diagnostic: null, sourceProjectId: null } });
const project = { id: "project", name: "项目", root: "/repo", gitCommonDir: null };
const source = { selectedBinary: null, resolvedBinary: null, version: null, connection: "failed", checkedAtUnixMs: null,
  error: null, capabilities: Object.fromEntries(["metadata", "history", "experimentalHistory", "codexSummary", "codexNaming"]
    .map((key) => [key, { state: "unavailable", detail: "未连接" }])) };

test("通过公开应用查询联动时间线、关系图和详情，过滤后保留选择", async () => {
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_settings") return { theme: "system", source };
    if (command === "connect_source" || command === "get_source_status") return source;
    if (command === "get_project_catalog") return { projects: [project], selectedProjectId: "project", recentProjectIds: ["project"], unassigned: [], scopes: [] };
    if (command === "get_project_sessions") return { project, workspaces: ["/repo"], threads: [attributed("t1"), attributed("t2")], scopes: [] };
    if (command === "get_latest_index_run" || command === "get_latest_analysis_run") return null;
    if (command === "get_jev_status") return { config: { baseUrl: "https://api.typesafe.ai", model: "jev-latest" }, credentialConfigured: false, credentialError: null };
    if (command === "get_project_workstreams") return { workstreams: [{ id: "w1", name: "实现", members: ["t1"] }], ungroupedThreadIds: ["t2"] };
    if (command === "query_project_threads") {
      const query = (args as { query: { text: string } }).query;
      return { total: 2, matches: (query.text ? ["t2"] : ["t1", "t2"]).map((threadId) => ({ threadId, summary: null })) };
    }
    throw new Error(`未知接口 ${command}`);
  });
  render(<App />);
  await waitFor(() => expect(screen.getByLabelText("时间线").textContent).toContain("可见：t1,t2"));
  fireEvent.click(await screen.findByRole("button", { name: "选中时间线会话" }));
  expect(screen.getByLabelText("会话详情").textContent).toContain("t1 · 可见");
  fireEvent.click(screen.getByRole("button", { name: "关系图" }));
  expect(screen.getByLabelText("关系图").textContent).toContain("选择：t1");
  fireEvent.change(screen.getByPlaceholderText("标题、预览、Thread ID 或已生成总结"), { target: { value: "t2" } });
  await waitFor(() => expect(screen.getByLabelText("关系图").textContent).toContain("可见：t2"));
  expect(screen.getByLabelText("会话详情").textContent).toContain("t1 · 隐藏");
  expect(screen.getByText(/当前选择的会话被过滤条件隐藏/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "选中图会话" }));
  expect(screen.getByLabelText("会话详情").textContent).toContain("t2 · 可见");
  expect(vi.mocked(invoke)).toHaveBeenCalledWith("query_project_threads", expect.objectContaining({ projectId: "project" }));
  fireEvent.change(screen.getByLabelText("按工作流过滤"), { target: { value: "w1" } });
  fireEvent.change(screen.getByLabelText("按工作区过滤"), { target: { value: "/repo" } });
  fireEvent.change(screen.getByLabelText("按归档过滤"), { target: { value: "active" } });
  fireEvent.change(screen.getByLabelText("按完整性过滤"), { target: { value: "complete" } });
  await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("query_project_threads", {
    projectId: "project", query: { text: "t2", workstreamId: "w1", workspaceRoot: "/repo", archived: false, complete: true },
  }));
});
