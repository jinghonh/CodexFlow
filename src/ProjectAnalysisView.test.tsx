// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ProjectAnalysisView } from "./ProjectAnalysisView";
import { ProjectQueryCacheProvider } from "./projectQueryCache";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const limits = { callLimit: 100, concurrencyLimit: 2, timeoutSeconds: 180, retryLimit: 2, inputCharacterLimit: 40000 };
const preview = { projectId: "project", inputVersion: "v1", cachedSummaries: 1, unavailableSummaries: 1,
  maximumCandidates: 2, evidenceSelectionCallLimit: 4, pendingGroups: null, limits, jevConfigured: false,
  stages: [
    { stage: "summary", service: "文本服务 https://text.example/v1", model: "test-model", sendScope: "单条会话来源", pendingItems: 2, maximumCalls: 6, available: true, note: "本批可执行" },
    { stage: "relation", service: "Jev", model: "jev-latest", sendScope: "发送到配置的 Jev 服务", pendingItems: 2, maximumCalls: 7, available: false, note: "尚未接入" },
    { stage: "evidenceSelection", service: "Jev", model: "jev-latest", sendScope: "候选证据", pendingItems: 2, maximumCalls: 12, available: false, note: "最多两次请求" },
    { stage: "naming", service: "文本服务 https://text.example/v1", model: "test-model", sendScope: "分组事实", pendingItems: 0, maximumCalls: 0, available: false, note: "待命名数量未知" },
  ] };
const run = { id: "analysis-1", projectId: "project", state: "running", pauseReason: null,
  batchNumber: 1, batchCalls: 1, totalCalls: 1, totalQuestions: 0, inputTokens: null, outputTokens: null,
  processed: 0, succeeded: 0, failed: 0, pending: 2, interrupted: false, error: null, limits, units: [] };

test("配置变化后取消旧批次并刷新预览，由用户启动当前配置的新批次", async () => {
  let refreshed = false;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return { ...preview, stages: preview.stages.map((item) =>
      item.stage === "summary" && refreshed ? { ...item, model: "current-model" } : item) };
    if (command === "get_latest_analysis_run") return { ...run, state: "paused" };
    if (command === "continue_analysis_run") throw {
      code: "ANALYSIS_CONFIG_CHANGED", message: "来源或分析服务配置已变化；请取消旧运行并启动新批次。",
      retryable: false, cachePreserved: true, nextStep: "刷新分析预览并使用当前配置重新运行。",
    };
    if (command === "cancel_analysis_run") { refreshed = true; return { ...run, state: "cancelled" }; }
    if (command === "start_project_analysis") return { ...run, id: "analysis-2" };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectQueryCacheProvider><ProjectAnalysisView projectId="project" refreshVersion="1" /></ProjectQueryCacheProvider>);
  fireEvent.click(await screen.findByRole("button", { name: "继续未完成项" }));
  await screen.findByRole("alert");
  expect(screen.queryByRole("button", { name: "继续未完成项" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "取消旧批次并刷新预览" }));
  await screen.findByText(/current-model/);
  expect(screen.queryByRole("button", { name: "继续未完成项" })).toBeNull();
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("start_project_analysis", expect.anything());
  const start = screen.getByRole("button", { name: "启动会话总结" });
  await waitFor(() => expect(start.hasAttribute("disabled")).toBe(false));
  fireEvent.click(start);
  await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("start_project_analysis", expect.objectContaining({ projectId: "project" })));
});

test("已保存的配置变化错误直接提供恢复入口，取消失败时保留旧运行并允许重试", async () => {
  let attempts = 0;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return preview;
    if (command === "get_latest_analysis_run") return { ...run, state: "paused", error: {
      code: "ANALYSIS_CONFIG_CHANGED", message: "请取消旧运行并启动新批次。", retryable: false, cachePreserved: true,
    } };
    if (command === "cancel_analysis_run") {
      attempts += 1;
      if (attempts === 1) throw { message: "暂时无法取消，请重试。" };
      return { ...run, state: "cancelled" };
    }
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" />);
  fireEvent.click(await screen.findByRole("button", { name: "取消旧批次并刷新预览" }));
  await screen.findByText("暂时无法取消，请重试。");
  expect(screen.getByRole("button", { name: "启动会话总结" }).hasAttribute("disabled")).toBe(true);
  expect(screen.queryByRole("button", { name: "继续未完成项" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "取消旧批次并刷新预览" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "启动会话总结" }).hasAttribute("disabled")).toBe(false));
  expect(attempts).toBe(2);
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("start_project_analysis", expect.anything());
});

test("已保存的关系分析结果通知项目图刷新", async () => {
  const onRelationResultsChanged = vi.fn();
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return preview;
    if (command === "get_latest_analysis_run") return { ...run, state: "complete", units: [
      { id: "summary-1", stage: "summary", state: "succeeded", attempts: 1, actualModel: "codex", error: null },
      { id: "relation-1", stage: "relation", state: "succeeded", attempts: 1, actualModel: "jev", error: null },
    ] };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" stage="relations" onRelationResultsChanged={onRelationResultsChanged} />);
  await waitFor(() => expect(onRelationResultsChanged).toHaveBeenCalledTimes(1));
});

test("预览明确未配置阶段，手动启动后可暂停和继续", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return preview;
    if (command === "get_latest_analysis_run") return null;
    if (command === "start_project_analysis") return run;
    if (command === "pause_analysis_run") return { ...run, state: "paused", pauseReason: "用户暂停", processed: 1, succeeded: 1, pending: 1 };
    if (command === "continue_analysis_run") return { ...run, state: "complete", batchNumber: 2, batchCalls: 1, totalCalls: 2, processed: 2, succeeded: 2, pending: 0 };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" />);
  expect(await screen.findByText("2 条待总结")).toBeTruthy();
  expect(screen.queryByText(/尚不可执行/)).toBeNull();
  expect(screen.getByText("100 次")).toBeTruthy();
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("start_project_analysis", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "启动会话总结" }));
  await waitFor(() => expect(screen.getByText(/分析执行中/)).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "暂停" }));
  expect((await screen.findAllByText("用户暂停")).length).toBeGreaterThan(0);
  fireEvent.click(screen.getByRole("button", { name: "继续未完成项" }));
  expect(await screen.findByText(/分析完成/)).toBeTruthy();
  expect(vi.mocked(invoke)).toHaveBeenCalledWith("continue_analysis_run", { runId: "analysis-1", callLimit: 100 });
});

test("配置错误保留预览并允许修正预算后启动", async () => {
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_analysis_preview") return { ...preview, limits: (args as { limits: typeof limits }).limits };
    if (command === "get_latest_analysis_run") return null;
    if (command === "start_project_analysis") throw { message: "来源配置已变化" };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" />);
  await screen.findByText("2 条待总结");
  fireEvent.change(screen.getByLabelText("本批调用上限"), { target: { value: "0" } });
  expect(screen.getByRole("button", { name: "启动会话总结" }).hasAttribute("disabled")).toBe(true);
  fireEvent.change(screen.getByLabelText("本批调用上限"), { target: { value: "5" } });
  fireEvent.click(screen.getByRole("button", { name: "应用" }));
  await screen.findByText("2 条待总结");
  fireEvent.click(screen.getByRole("button", { name: "启动会话总结" }));
  expect(await screen.findByText("来源配置已变化")).toBeTruthy();
  expect(screen.getByText("2 条待总结")).toBeTruthy();
});

test("应用预算、并发和超时不重读预览，新批次使用应用后的参数", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return preview;
    if (command === "get_latest_analysis_run") return null;
    if (command === "start_project_analysis") return run;
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" />);
  await screen.findByText("2 条待总结");
  for (const [label, value] of [["本批调用上限", "5"], ["本批并发上限", "4"], ["单次超时", "90"]]) {
    fireEvent.change(screen.getByLabelText(label), { target: { value } });
    expect(screen.getByRole("button", { name: "启动会话总结" }).hasAttribute("disabled")).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "应用" }));
    expect(screen.getByText("2 条待总结")).toBeTruthy();
    expect(screen.queryByText("正在计算本阶段预览…")).toBeNull();
  }
  expect(screen.getByText("5 次")).toBeTruthy();
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "get_analysis_preview")).toHaveLength(1);
  fireEvent.click(screen.getByRole("button", { name: "启动会话总结" }));
  await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("start_project_analysis", expect.objectContaining({
    limits: { ...limits, callLimit: 5, concurrencyLimit: 4, timeoutSeconds: 90 },
  })));
});

test.each([
  { panel: "summary", stage: "summary", calls: 6 },
  { panel: "relations", stage: "relation", calls: 9 },
  { panel: "naming", stage: "naming", calls: 3 },
] as const)("$panel 修改重试次数只换算调用上界，保留额外探测调用", async ({ panel, stage, calls }) => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return { ...preview, stages: preview.stages
      .filter((item) => item.stage === stage).map((item) => ({ ...item, pendingItems: 2, maximumCalls: calls })) };
    if (command === "get_latest_analysis_run") return null;
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" stage={panel} />);
  await screen.findByText(`待处理 2；调用上界 ${calls}`);
  for (const retries of [0, 5, 2]) {
    fireEvent.change(screen.getByLabelText("自动重试次数"), { target: { value: String(retries) } });
    fireEvent.click(screen.getByRole("button", { name: "应用" }));
    expect(screen.getByText(`待处理 2；调用上界 ${calls / 3 * (retries + 1)}`)).toBeTruthy();
  }
  expect(vi.mocked(invoke).mock.calls.filter(([command]) => command === "get_analysis_preview")).toHaveLength(1);
});

test("输入范围变化重新检查缓存，等待期间修改重试次数仍按最新参数展示", async () => {
  let resolvePreview: ((value: typeof preview) => void) | undefined;
  const nextPreview = new Promise<typeof preview>((resolve) => { resolvePreview = resolve; });
  let reads = 0;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return ++reads === 1 ? preview : nextPreview;
    if (command === "get_latest_analysis_run") return null;
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" />);
  await screen.findByText("2 条待总结");
  fireEvent.change(screen.getByLabelText("单次输入字符上限"), { target: { value: "8000" } });
  fireEvent.click(screen.getByRole("button", { name: "应用" }));
  expect(screen.queryByText("2 条待总结")).toBeNull();
  expect(screen.getByRole("button", { name: "启动会话总结" }).hasAttribute("disabled")).toBe(true);
  fireEvent.change(screen.getByLabelText("自动重试次数"), { target: { value: "0" } });
  fireEvent.click(screen.getByRole("button", { name: "应用" }));
  resolvePreview?.({ ...preview, cachedSummaries: 0, limits: { ...limits, inputCharacterLimit: 8000 },
    stages: preview.stages.map((item) => item.stage === "summary" ? { ...item, pendingItems: 3, maximumCalls: 9 } : item) });
  await screen.findByText("3 条待总结");
  expect(screen.getByText("0 条有效缓存")).toBeTruthy();
  expect(screen.getByText("待处理 3；调用上界 3")).toBeTruthy();
  expect(screen.getByRole("button", { name: "启动会话总结" }).hasAttribute("disabled")).toBe(false);
  expect(reads).toBe(2);
  expect(vi.mocked(invoke)).toHaveBeenCalledWith("get_analysis_preview", expect.objectContaining({
    limits: { ...limits, inputCharacterLimit: 8000 },
  }));
});

test("暂停运行的继续请求出错后仍可重试", async () => {
  let attempts = 0;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return preview;
    if (command === "get_latest_analysis_run") return { ...run, state: "paused", pauseReason: "额度已用尽", pending: 1 };
    if (command === "continue_analysis_run") {
      attempts += 1;
      if (attempts === 1) throw { message: "配置仍不可用" };
      return { ...run, state: "complete", batchNumber: 2, processed: 2, succeeded: 2, pending: 0 };
    }
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" />);
  fireEvent.click(await screen.findByRole("button", { name: "继续未完成项" }));
  expect(await screen.findByText("配置仍不可用")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "继续未完成项" }));
  expect(await screen.findByText(/分析完成/)).toBeTruthy();
  expect(attempts).toBe(2);
});

test("Jev 设置版本变化后清除旧预览，取得新服务范围前不可启动", async () => {
  let resolveNew: ((value: typeof preview) => void) | undefined;
  const newPreview = new Promise<typeof preview>((resolve) => { resolveNew = resolve; });
  let reads = 0;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") {
      reads += 1;
      return reads === 1 ? { ...preview, stages: preview.stages.map((stage) => stage.stage === "relation"
        ? { ...stage, model: "jev-old", sendScope: "发送到 https://old.example" } : stage) } : newPreview;
    }
    if (command === "get_latest_analysis_run") return null;
    if (command === "start_project_analysis") return run;
    throw new Error(`Unexpected command ${command}`);
  });
  const { rerender } = render(<ProjectAnalysisView projectId="project" refreshVersion="1" settingsRevision={0} stage="relations" />);
  expect(await screen.findByText("发送到 https://old.example")).toBeTruthy();
  rerender(<ProjectAnalysisView projectId="project" refreshVersion="1" settingsRevision={1} stage="relations" />);
  expect(screen.getByRole("button", { name: "启动候选关系判断" }).hasAttribute("disabled")).toBe(true);
  expect(screen.queryByText("发送到 https://old.example")).toBeNull();
  resolveNew?.({ ...preview, stages: preview.stages.map((stage) => stage.stage === "relation"
    ? { ...stage, available: true, model: "jev-new", sendScope: "发送到 https://new.example" } : stage) });
  expect(await screen.findByText("发送到 https://new.example")).toBeTruthy();
  expect(screen.getByText("Jev / jev-new")).toBeTruthy();
  expect(screen.getByRole("button", { name: "启动候选关系判断" }).hasAttribute("disabled")).toBe(false);
});

test("分析进度展示别名探测后固定的实际模型版本", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_analysis_preview") return preview;
    if (command === "get_latest_analysis_run") return { ...run, state: "paused", jevPinnedModel: "jev-1.13.0", jevProbeAttempts: 2 };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ProjectAnalysisView projectId="project" refreshVersion="1" />);
  expect(await screen.findByText("Jev 本批固定版本：jev-1.13.0（合成探测 2 次）")).toBeTruthy();
});
