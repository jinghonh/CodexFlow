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
    contentAvailable: true, cacheCurrent: false, cachedSummary: null };
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
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ThreadSummaryView threadId="thread-h" connected revision={0} />);
  expect(await screen.findByText("已截断或抽样，部分内容未发送")).toBeTruthy();
  expect(screen.getByText("来源历史不完整，总结只能覆盖已取得内容")).toBeTruthy();
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("start_thread_summary", expect.anything());
  fireEvent.click(screen.getByRole("button", { name: "手动生成" }));
  await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("start_thread_summary", { threadId: "thread-h" }));
  expect(await screen.findByText("目标内容")).toBeTruthy();
});
