// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ProjectAnalysisView } from "./ProjectAnalysisView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const limits = { callLimit: 100, concurrencyLimit: 2, timeoutSeconds: 180, retryLimit: 2, inputCharacterLimit: 40000 };
const preview = { projectId: "project", inputVersion: "v1", cachedSummaries: 1, unavailableSummaries: 1,
  maximumCandidates: 2, evidenceSelectionCallLimit: 4, pendingGroups: null, limits, jevConfigured: false,
  stages: [
    { stage: "summary", service: "Codex", model: "test-model", sendScope: "单条会话来源", pendingItems: 2, maximumCalls: 6, available: true, note: "本批可执行" },
    { stage: "relation", service: "Jev", model: "jev-latest", sendScope: "发送到配置的 Jev 服务", pendingItems: 2, maximumCalls: 7, available: false, note: "尚未接入" },
    { stage: "evidenceSelection", service: "Jev", model: "jev-latest", sendScope: "候选证据", pendingItems: 2, maximumCalls: 12, available: false, note: "最多两次请求" },
    { stage: "naming", service: "Codex", model: "test-model", sendScope: "分组事实", pendingItems: 0, maximumCalls: 0, available: false, note: "待命名数量未知" },
  ] };
const run = { id: "analysis-1", projectId: "project", state: "running", pauseReason: null,
  batchNumber: 1, batchCalls: 1, totalCalls: 1, totalQuestions: 0, inputTokens: null, outputTokens: null,
  processed: 0, succeeded: 0, failed: 0, pending: 2, interrupted: false, error: null, limits, units: [] };

test("预览明确未接入阶段，手动启动后可暂停和继续", async () => {
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
  expect(screen.getAllByText(/尚不可执行/)).toHaveLength(3);
  expect(screen.getByText(/Jev 未配置/)).toBeTruthy();
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("start_project_analysis", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "启动总结批次" }));
  await waitFor(() => expect(screen.getByText(/分析执行中/)).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "暂停" }));
  expect(await screen.findByText("用户暂停")).toBeTruthy();
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
  expect(screen.getByRole("button", { name: "启动总结批次" }).hasAttribute("disabled")).toBe(true);
  fireEvent.change(screen.getByLabelText("本批调用上限"), { target: { value: "5" } });
  await screen.findByText("2 条待总结");
  fireEvent.click(screen.getByRole("button", { name: "启动总结批次" }));
  expect(await screen.findByText("来源配置已变化")).toBeTruthy();
  expect(screen.getByText(/候选最多 2 对/)).toBeTruthy();
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
  const { rerender } = render(<ProjectAnalysisView projectId="project" refreshVersion="1" settingsRevision={0} />);
  expect(await screen.findByText("发送到 https://old.example")).toBeTruthy();
  rerender(<ProjectAnalysisView projectId="project" refreshVersion="1" settingsRevision={1} />);
  expect(screen.getByRole("button", { name: "启动总结批次" }).hasAttribute("disabled")).toBe(true);
  expect(screen.queryByText("发送到 https://old.example")).toBeNull();
  resolveNew?.({ ...preview, stages: preview.stages.map((stage) => stage.stage === "relation"
    ? { ...stage, model: "jev-new", sendScope: "发送到 https://new.example" } : stage) });
  expect(await screen.findByText("发送到 https://new.example")).toBeTruthy();
  expect(screen.getByText("Jev / jev-new")).toBeTruthy();
  expect(screen.getByRole("button", { name: "启动总结批次" }).hasAttribute("disabled")).toBe(false);
});
