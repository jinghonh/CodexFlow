// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ThreadSummaryView } from "./ThreadSummaryView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

test("手动启动且显示输入覆盖和已保存的五项总结", async () => {
  const preview = { model: "Codex 默认模型（启动后确认）", characterLimit: 40000, characterCount: 1200,
    totalFacts: 6, includedFacts: 4, totalMessages: 3, includedMessages: 2,
    truncated: true, turnsComplete: true, itemsComplete: false, sourceCurrent: true,
    contentAvailable: true, cacheCurrent: false, cachedSummary: null, analysisBlockedReason: null };
  const run = { id: "run-1", state: "complete", model: "test-model", temporaryThreadId: "tmp",
    turnId: "turn", reusedCache: false, error: null };
  let generated = false;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_summary_preview") return { ...preview, cachedSummary: generated ? {
      content: { goal: "目标内容", activity: "活动内容", outcome: "结果内容", decisions: "决定内容", issues: "问题内容" },
      evidenceIds: ["item:turn-1:item-1"], model: "test-model", createdAtUnixMs: 1000, sourceUpdatedAt: 200 } : null };
    if (command === "get_latest_summary_run") return null;
    if (command === "start_thread_summary") {
      generated = true;
      return run;
    }
    if (command === "inspect_summary_evidence") return { id: "item:turn-1:item-1", state: "valid",
      message: "证据有效，可定位到来源条目。", itemId: "item-1",
      location: { turnId: "turn-1", turnOffset: 0, offset: 0 }, excerpt: "目标正文" };
    throw new Error(`Unexpected command ${command}`);
  });
  const onLocate = vi.fn(async () => {});
  render(<ThreadSummaryView threadId="thread-h" connected revision={0} onLocate={onLocate} />);
  expect(await screen.findByText("已截断或抽样，部分内容未发送")).toBeTruthy();
  expect(screen.getByText("来源历史不完整，总结只能覆盖已取得内容")).toBeTruthy();
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("start_thread_summary", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "手动生成" }));
  await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("start_thread_summary", { threadId: "thread-h" }));
  expect(await screen.findByText("目标内容")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "检查并定位" }));
  await waitFor(() => expect(onLocate).toHaveBeenCalledWith({ turnId: "turn-1", turnOffset: 0, offset: 0 }, "item-1"));
  expect(await screen.findByText("目标正文")).toBeTruthy();
});

test("过期总结证据展示原因且不跳转", async () => {
  const onLocate = vi.fn(async () => {});
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_summary_preview") return { model: "test-model", characterLimit: 40000, characterCount: 120,
      totalFacts: 0, includedFacts: 0, totalMessages: 1, includedMessages: 1, truncated: false,
      turnsComplete: true, itemsComplete: true, sourceCurrent: true, contentAvailable: true,
      cacheCurrent: false, analysisBlockedReason: null,
      cachedSummary: { content: { goal: "旧目标", activity: "活动", outcome: "结果", decisions: "决定", issues: "问题" },
        evidenceIds: ["item:turn-1:item-1"], model: "test-model", createdAtUnixMs: 1000, sourceUpdatedAt: 200 } };
    if (command === "get_latest_summary_run") return null;
    if (command === "inspect_summary_evidence") return { id: "item:turn-1:item-1", state: "staleVersion",
      message: "总结引用的是旧版历史，请重新读取并生成总结。", itemId: "item-1", location: null, excerpt: "旧内容" };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ThreadSummaryView threadId="thread-h" connected revision={0} onLocate={onLocate} />);
  fireEvent.click(await screen.findByRole("button", { name: "检查并定位" }));
  expect(await screen.findAllByText("总结引用的是旧版历史，请重新读取并生成总结。")).toHaveLength(2);
  expect(onLocate).not.toHaveBeenCalled();
});

test("认证隔离不成立时禁用生成并保留查看入口", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_summary_preview") return { model: "Codex 默认模型", characterLimit: 40000, characterCount: 100,
      totalFacts: 0, includedFacts: 0, totalMessages: 1, includedMessages: 1,
      truncated: false, turnsComplete: true, itemsComplete: true, sourceCurrent: true,
      contentAvailable: true, cachedSummary: null, cacheCurrent: false,
      analysisBlockedReason: "认证文件无法隔离。" };
    if (command === "get_latest_summary_run") return null;
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ThreadSummaryView threadId="thread-h" connected revision={0} onLocate={vi.fn(async () => {})} />);
  expect(await screen.findByRole("alert")).toHaveProperty("textContent", "认证文件无法隔离。");
  expect(screen.getByRole("button", { name: "手动生成" }).hasAttribute("disabled")).toBe(true);
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("start_thread_summary", expect.anything());
});
