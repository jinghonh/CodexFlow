// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { act, cleanup, render, screen, within } from "@testing-library/react";
import { AnalysisTimingView, SummaryTimingView, elapsedTime, formatElapsed } from "./AnalysisTimingView";
import type { ProjectRun, SummaryRun, ExecutionTiming } from "./analysisProgress";

afterEach(() => { cleanup(); vi.useRealTimers(); });
const timing = (elapsedMs: number, activeSinceUnixMs: number | null = null): ExecutionTiming =>
  ({ elapsedMs, activeSinceUnixMs, incomplete: false });
const run: ProjectRun = {
  id: "run", projectId: "project", state: "running", pauseReason: null,
  batchNumber: 1, batchCalls: 1, totalCalls: 1, totalQuestions: 0, inputTokens: null, outputTokens: null,
  processed: 0, succeeded: 0, failed: 0, pending: 1, interrupted: false, error: null,
  limits: { callLimit: 100 }, startedAtUnixMs: 88000, finishedAtUnixMs: null,
  timing: timing(12000, 100000), stageTimings: [{ stage: "summary", timing: timing(12000, 100000) }],
  units: [{ id: "thread-1", stage: "summary", state: "running", attempts: 1, actualModel: null,
    error: null, timing: timing(8000, 100000), requestTiming: timing(5000, 100000) }],
};

test("执行时实时更新整体与单项耗时，暂停后停止计时", () => {
  vi.useFakeTimers(); vi.setSystemTime(100000);
  const { rerender } = render(<AnalysisTimingView run={run} />);
  expect(screen.getByText("12 秒")).toBeTruthy();
  const row = screen.getByText("thread-1").closest("tr")!;
  expect(within(row).getByText("8 秒")).toBeTruthy();
  expect(within(row).getByText("5 秒")).toBeTruthy();
  expect(screen.getByText("耗时明细").closest("details")?.open).toBe(false);
  act(() => { vi.advanceTimersByTime(3000); });
  expect(screen.getByText("15 秒")).toBeTruthy();
  expect(within(row).getByText("11 秒")).toBeTruthy();
  expect(within(row).getByText("8 秒")).toBeTruthy();
  rerender(<AnalysisTimingView run={{ ...run, state: "paused", timing: timing(15000),
    stageTimings: [{ stage: "summary", timing: timing(15000) }],
    units: [{ ...run.units[0], state: "pending", timing: timing(11000), requestTiming: timing(8000) }],
  }} />);
  act(() => { vi.advanceTimersByTime(60000); });
  expect(screen.getByText("15 秒")).toBeTruthy();
  expect(within(row).getByText("11 秒")).toBeTruthy();
});

test("崩溃后计时标注下界，旧任务不以起止时间伪造执行耗时", () => {
  render(<AnalysisTimingView run={{ ...run, state: "complete", timing: { ...timing(9000), incomplete: true },
    stageTimings: [], units: [{ ...run.units[0], timing: null, requestTiming: null }],
  }} />);
  expect(screen.getByText("至少 9 秒")).toBeTruthy();
  expect(screen.getAllByText("未记录")).toHaveLength(2);
});

test("单条会话总结展示总耗时和服务调用耗时", () => {
  const summary: SummaryRun = {
    id: "summary", threadId: "thread", state: "complete", model: "model", temporaryThreadId: null,
    turnId: null, reusedCache: false, error: null, startedAtUnixMs: 1000, finishedAtUnixMs: 64000,
    timing: timing(63000), requestTiming: timing(58000),
  };
  render(<SummaryTimingView run={summary} />);
  expect(screen.getByText("1 分 3 秒")).toBeTruthy();
  expect(screen.getByText("服务调用 · 58 秒")).toBeTruthy();
  expect(screen.getByText(/暂无法拆分模型与网络耗时/)).toBeTruthy();
});

test("时间格式支持小时、未知值及系统时钟回退", () => {
  expect(formatElapsed(null)).toBe("未记录");
  expect(formatElapsed(500)).toBe("500 毫秒");
  expect(formatElapsed(3661000)).toBe("1 小时 1 分 1 秒");
  expect(elapsedTime(timing(5000, 10000), 9000)).toBe(5000);
});

test("没有请求的已完成任务显示复用缓存，历史请求缺少计时仍显示未记录", () => {
  render(<AnalysisTimingView run={{ ...run, state: "complete", timing: timing(19000), stageTimings: [],
    units: [
      { ...run.units[0], id: "cached-pair", stage: "relation", state: "succeeded", attempts: 0,
        actualModel: "jev-1.13.0", timing: timing(9554), requestTiming: null },
      { ...run.units[0], id: "old-request", state: "succeeded", attempts: 1,
        actualModel: "model", timing: timing(9591), requestTiming: null },
    ],
  }} />);
  expect(within(screen.getByText("cached-pair").closest("tr")!).getByText("复用缓存")).toBeTruthy();
  expect(within(screen.getByText("old-request").closest("tr")!).getByText("未记录")).toBeTruthy();
});

test("显示运行实际采用的并发上限与当前执行数量", () => {
  render(<AnalysisTimingView run={{ ...run, limits: { callLimit: 100, concurrencyLimit: 3 } }} />);
  expect(screen.getByText("执行中 1 项 · 本批并发上限 3")).toBeTruthy();
});
