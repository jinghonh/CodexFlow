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
vi.mock("./ThreadHistoryView", () => ({ ThreadHistoryView: ({ threadId, updatedAt }: { threadId: string; updatedAt: number }) => <div aria-label="回合事实">{threadId}@{updatedAt}</div> }));
vi.mock("./ProjectThreadDetailsView", () => ({ ProjectThreadDetailsView: ({ thread, hidden }: { thread: { id: string; updatedAt: number }; hidden: boolean }) => <div aria-label="会话详情">{thread.id}@{thread.updatedAt} · {hidden ? "隐藏" : "可见"}</div> }));
vi.mock("./CandidatePreviewView", () => ({ CandidatePreviewView: () => null }));
vi.mock("./ProjectAnalysisView", () => ({ ProjectAnalysisView: ({ onRelationResultsChanged }: { onRelationResultsChanged: () => void }) => <button onClick={onRelationResultsChanged}>模拟分析结果更新</button> }));

let failSearch = false;
afterEach(() => { cleanup(); vi.clearAllMocks(); window.localStorage.clear(); failSearch = false; });

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

test("会话浏览使用查询快照显示行和选中会话", async () => {
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_settings") return { theme: "system", source };
    if (command === "get_runtime_platform") return "macos";
    if (command === "connect_source" || command === "get_source_status") return source;
    if (command === "get_project_catalog") return { projects: [project], selectedProjectId: "project", recentProjectIds: ["project"], unassigned: [], scopes: [] };
    if (command === "get_project_sessions") return { project, workspaces: ["/repo"], threads: [attributed("t1"), attributed("t2")], scopes: [] };
    if (command === "get_latest_index_run" || command === "get_latest_analysis_run") return null;
    if (command === "get_jev_status") return { config: { baseUrl: "https://api.typesafe.ai", model: "jev-latest" }, credentialConfigured: false, credentialError: null };
    if (command === "get_project_workstreams") return { workstreams: [{ id: "w1", name: "实现", members: ["t1"] }], ungroupedThreadIds: ["t2"] };
    if (command === "query_project_threads") {
      if (failSearch) throw { code: "STORAGE_FAILED", message: "项目会话查询失败。", retryable: true, cachePreserved: true, nextStep: "检查数据目录后重试。" };
      const query = (args as { query: { text: string; selectedThreadId: string | null } }).query;
      const rows = [attributed("t1"), attributed("t2")];
      rows[0].thread.title = "查询快照 t1";
      rows[0].thread.updatedAt = 77;
      rows[1].thread.updatedAt = 99;
      const matches = query.text ? [rows[1]] : rows;
      const selected = query.selectedThreadId && !matches.some(({ thread }) => thread.id === query.selectedThreadId)
        ? rows.find(({ thread }) => thread.id === query.selectedThreadId) ?? null : null;
      return { total: 2, matches, selected };
    }
    throw new Error(`未知接口 ${command}`);
  });
  window.localStorage.setItem("codexflow.active-panel", "sessions");
  render(<App />);
  await waitFor(() => expect(document.querySelectorAll(".thread-row")).toHaveLength(2));
  expect(await screen.findByText("查询快照 t1")).toBeTruthy();
  fireEvent.click(await screen.findByRole("button", { name: "查看会话 t1 的历史" }));
  await waitFor(() => expect(screen.getByLabelText("回合事实").textContent).toBe("t1@77"));
  fireEvent.change(screen.getByPlaceholderText("标题、预览、Thread ID 或已生成总结"), { target: { value: "t2" } });
  await waitFor(() => expect(document.querySelectorAll(".thread-row")).toHaveLength(1));
  expect(screen.getByRole("button", { name: "查看会话 t2 的历史" })).toBeTruthy();
  expect(screen.getByLabelText("回合事实").textContent).toBe("t1@77");
  expect(screen.getByText(/当前选择的会话被过滤条件隐藏/)).toBeTruthy();
  expect(vi.mocked(invoke)).toHaveBeenCalledWith("query_project_threads", expect.objectContaining({ projectId: "project" }));
  fireEvent.change(screen.getByLabelText("按工作流过滤"), { target: { value: "w1" } });
  fireEvent.change(screen.getByLabelText("按工作区过滤"), { target: { value: "/repo" } });
  fireEvent.change(screen.getByLabelText("按归档过滤"), { target: { value: "active" } });
  fireEvent.change(screen.getByLabelText("按完整性过滤"), { target: { value: "complete" } });
  await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("query_project_threads", {
    projectId: "project", query: { text: "t2", workstreamId: "w1", workspaceRoot: "/repo", archived: false, complete: true, selectedThreadId: "t1" },
  }));
  failSearch = true;
  fireEvent.change(screen.getByPlaceholderText("标题、预览、Thread ID 或已生成总结"), { target: { value: "failure" } });
  await screen.findByText(/STORAGE_FAILED：项目会话查询失败/);
  expect(screen.getByLabelText("回合事实").textContent).toBe("t1@77");
});

test("大项目会话列表逐页展示且过滤仍覆盖全部会话", async () => {
  const ids = Array.from({ length: 81 }, (_, index) => `thread-${index}`);
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_settings") return { theme: "system", source };
    if (command === "get_runtime_platform") return "macos";
    if (command === "connect_source" || command === "get_source_status") return source;
    if (command === "get_project_catalog") return { projects: [project], selectedProjectId: "project", recentProjectIds: ["project"], unassigned: [], scopes: [] };
    if (command === "get_project_sessions") return { project, workspaces: ["/repo"], threads: ids.map(attributed), scopes: [] };
    if (command === "get_latest_index_run" || command === "get_latest_analysis_run") return null;
    if (command === "get_jev_status") return { config: { baseUrl: "https://api.typesafe.ai", model: "jev-latest" }, credentialConfigured: false, credentialError: null };
    if (command === "get_project_workstreams") return { workstreams: [], ungroupedThreadIds: ids, revision: 1 };
    if (command === "query_project_threads") {
      const needle = (args as { query: { text: string } }).query.text;
      return { total: ids.length, matches: ids.filter((id) => id.includes(needle)).map(attributed), selected: null };
    }
    throw new Error(`未知接口 ${command}`);
  });
  window.localStorage.setItem("codexflow.active-panel", "sessions");
  render(<App />);
  await waitFor(() => expect(document.querySelectorAll(".thread-row")).toHaveLength(40));
  expect(screen.getByRole("button", { name: "显示更多会话（40 / 81）" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "显示更多会话（40 / 81）" }));
  expect(document.querySelectorAll(".thread-row")).toHaveLength(80);
  fireEvent.change(screen.getByPlaceholderText("标题、预览、Thread ID 或已生成总结"), { target: { value: "thread-80" } });
  await waitFor(() => expect(document.querySelectorAll(".thread-row")).toHaveLength(1));
  expect(screen.getByRole("button", { name: "查看会话 thread-80 的历史" })).toBeTruthy();
});

test("Windows 来源设置只自动查找 PATH 并忽略已保存的手动路径", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_settings") return { theme: "system", source: { ...source, selectedBinary: "C:\\legacy\\codex.cmd" } };
    if (command === "get_runtime_platform") return "windows";
    if (command === "connect_source" || command === "get_source_status") return source;
    if (command === "get_project_catalog") return { projects: [project], selectedProjectId: "project", recentProjectIds: ["project"], unassigned: [], scopes: [] };
    if (command === "get_project_sessions") return { project, workspaces: ["/repo"], threads: [], scopes: [] };
    if (command === "get_latest_index_run" || command === "get_latest_analysis_run") return null;
    if (command === "get_jev_status") return { config: { baseUrl: "https://api.typesafe.ai", model: "jev-latest" }, credentialConfigured: false, credentialError: null };
    if (command === "get_text_status") return { config: { baseUrl: "https://api.openai.com", model: "test" }, credentialConfigured: false, credentialError: null };
    throw new Error(`未知接口 ${command}`);
  });
  window.localStorage.setItem("codexflow.active-panel", "connections");
  render(<App />);
  const choosePanel = await screen.findByRole("heading", { name: "Codex 可执行文件" });
  await waitFor(() => expect(screen.getByRole("button", { name: /从 PATH 查找并诊断/ })).toBeTruthy());
  const panel = choosePanel.closest("section")!;
  expect(panel.querySelector("#binary")).toBeNull();
  expect(panel.querySelector(".browse-button")).toBeNull();
  expect(vi.mocked(invoke)).toHaveBeenCalledWith("connect_source", { selectedBinary: null });
});

function mockWorkspace() {
  let selected = project;
  const second = { ...project, id: "second", name: "另一个项目", root: "/second" };
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_settings") return { theme: "light", source };
    if (command === "get_runtime_platform") return "macos";
    if (command === "connect_source" || command === "get_source_status") return source;
    if (command === "choose_existing_project") selected = (args as { projectId: string }).projectId === "second" ? second : project;
    if (command === "get_project_catalog" || command === "choose_existing_project") return { projects: [project, second], selectedProjectId: selected.id, recentProjectIds: [selected.id], unassigned: [attributed("orphan")], scopes: [] };
    if (command === "get_project_sessions") return { project: selected, workspaces: [selected.root], threads: [attributed("t1"), attributed("t2")], scopes: [] };
    if (command === "get_model_runs") return { runs: [] };
    if (command === "get_latest_index_run" || command === "get_latest_analysis_run") return null;
    if (command === "get_jev_status" || command === "get_text_status" || command === "get_embedding_status") return { config: { baseUrl: "https://model.example", model: "test" }, credentialConfigured: false, credentialError: null };
    if (command === "get_project_workstreams") return { workstreams: [], ungroupedThreadIds: ["t1", "t2"], revision: 1 };
    if (command === "query_project_threads") {
      const query = (args as { query: { text: string; selectedThreadId: string | null } }).query;
      const rows = [attributed("t1"), attributed("t2")];
      return { total: 2, matches: rows.filter(({ thread }) => thread.id.includes(query.text)), selected: rows.find(({ thread }) => thread.id === query.selectedThreadId) ?? null };
    }
    throw new Error(`未模拟接口 ${command}`);
  });
}

test("同项目视图切换保留会话筛选和选择，时间线独立显示", async () => {
  mockWorkspace();
  window.localStorage.setItem("codexflow.active-panel", "sessions");
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "查看会话 t1 的历史" }));
  fireEvent.change(screen.getByLabelText("查找会话"), { target: { value: "t2" } });
  await waitFor(() => expect(document.querySelectorAll(".thread-row")).toHaveLength(1));
  fireEvent.click(screen.getByRole("button", { name: "关系" }));
  expect(screen.getByLabelText("关系图").textContent).toContain("选择：t1");
  fireEvent.click(screen.getByRole("button", { name: "时间线" }));
  expect(screen.getByLabelText("时间线").textContent).toContain("选择：t1");
  expect(document.querySelector("#sessions")?.closest(".workspace-view")?.hasAttribute("hidden")).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: /^会话/ }));
  expect((screen.getByLabelText("查找会话") as HTMLInputElement).value).toBe("t2");
  expect(await screen.findByText(/当前选择的会话被过滤条件隐藏/)).toBeTruthy();
  expect(screen.getByLabelText("回合事实").textContent).toBe("t1@2");
});

test("侧栏选项目直接进入工作区，记住各项目视图并清除跨项目筛选", async () => {
  mockWorkspace();
  window.localStorage.setItem("codexflow.active-panel", "sessions");
  window.localStorage.setItem("codexflow.project-panel:second", "relations");
  render(<App />);
  await screen.findByRole("button", { name: "查看会话 t1 的历史" });
  fireEvent.change(screen.getByLabelText("查找会话"), { target: { value: "t1" } });
  fireEvent.click(screen.getByRole("button", { name: "时间线" }));
  fireEvent.click(screen.getByRole("button", { name: /另一个项目/ }));
  await waitFor(() => expect(screen.getByRole("button", { name: "关系" }).getAttribute("aria-current")).toBe("page"));
  expect(screen.getByRole("heading", { name: "另一个项目" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /项目.*\/repo/ }));
  await waitFor(() => expect(screen.getByRole("button", { name: "时间线" }).getAttribute("aria-current")).toBe("page"));
  fireEvent.click(screen.getByRole("button", { name: /^会话/ }));
  expect((screen.getByLabelText("查找会话") as HTMLInputElement).value).toBe("");
});

test("分析通过抽屉按需打开，关闭后返回原视图", async () => {
  mockWorkspace();
  const showModal = vi.fn(function (this: HTMLDialogElement) { this.setAttribute("open", ""); });
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", { configurable: true, value: showModal });
  const close = vi.fn(function (this: HTMLDialogElement) { this.removeAttribute("open"); });
  Object.defineProperty(HTMLDialogElement.prototype, "close", { configurable: true, value: close });
  window.localStorage.setItem("codexflow.active-panel", "sessions");
  render(<App />);
  await screen.findByRole("button", { name: "查看会话 t1 的历史" });
  expect(screen.queryByRole("button", { name: "模拟分析结果更新" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "分析" }));
  expect(screen.getByRole("dialog", { name: "项目分析" })).toBeTruthy();
  expect(showModal).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: "关闭项目分析" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(close).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("button", { name: /^会话/ }).getAttribute("aria-current")).toBe("page");
  Reflect.deleteProperty(HTMLDialogElement.prototype, "showModal");
  Reflect.deleteProperty(HTMLDialogElement.prototype, "close");
});
