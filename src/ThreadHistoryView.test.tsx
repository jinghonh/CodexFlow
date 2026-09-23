// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { ThreadHistoryView } from "./ThreadHistoryView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const coverage = { threadId: "thread-h", sourceUpdatedAt: 200, attemptedAtUnixMs: 300_000,
  path: "paginated", turnsComplete: true, itemsComplete: false, turnPages: 2, itemPages: 1,
  loadedTurns: 25, loadedItems: 21, incompatible: false, error: "第二页读取失败" };
const turn = (id: string, ordinal: number) => ({ id, ordinal, status: "completed", startedAtUnixMs: 100_000,
  completedAtUnixMs: 102_000, durationMs: 2_000, sourceUpdatedAt: 200, contentVersion: "turn-version" });
const item = (id: string, turnId: string) => ({ id, turnId, ordinal: 24, sourceType: "agentMessage",
  supported: true, text: "目标正文", command: null, cwd: null, output: null, exitCode: null,
  status: null, changes: [], sourceUpdatedAt: 200, contentVersion: "abcdef1234567890" });

test("相同条目标识按回合定位到对应条目页", async () => {
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_history_turns") {
      const offset = (args as { offset: number }).offset;
      return { coverage, turns: offset === 0 ? [turn("turn-1", 0)] : [turn("turn-25", 24)], total: 25, offset, limit: 20 };
    }
    if (command === "get_history_items") {
      const { turnId, offset } = args as { turnId: string; offset: number };
      return { coverage, items: turnId === "turn-25" && offset === 20 || turnId === "turn-1" ? [item("item-target", turnId)] : [],
        total: turnId === "turn-25" ? 25 : 1, offset, limit: 20 };
    }
    if (command === "locate_history_item") {
      expect(args).toEqual({ threadId: "thread-h", turnId: "turn-25", itemId: "item-target" });
      return { turnId: "turn-25", turnOffset: 24, offset: 24 };
    }
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ThreadHistoryView threadId="thread-h" updatedAt={200} connected />);
  expect(await screen.findByText("部分完整")).toBeTruthy();
  fireEvent.change(screen.getByPlaceholderText("输入完整 Turn ID"), { target: { value: "turn-25" } });
  fireEvent.change(screen.getByPlaceholderText("输入完整 Item ID"), { target: { value: "item-target" } });
  fireEvent.click(screen.getByRole("button", { name: "定位" }));
  expect(await screen.findByText("目标正文")).toBeTruthy();
  expect(screen.getByText("已定位到回合 turn-25。")).toBeTruthy();
  await waitFor(() => expect(screen.getAllByText("21–25 / 25")).toHaveLength(2));
  await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("get_history_items",
    { threadId: "thread-h", turnId: "turn-25", offset: 20, limit: 20 }));
  expect(vi.mocked(invoke)).not.toHaveBeenCalledWith("load_thread_history", expect.anything());
});

test("缓存缺失且来源可用时读取一次，失败后仍给出可操作状态", async () => {
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_history_turns") return { coverage: null, turns: [], total: 0, offset: 0, limit: 20 };
    if (command === "load_thread_history") throw { message: "来源暂时不可用" };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ThreadHistoryView threadId="thread-h" updatedAt={200} connected />);
  expect(await screen.findByRole("alert")).toHaveProperty("textContent", "来源暂时不可用");
  expect(screen.getByText("尚未读取历史")).toBeTruthy();
  expect(vi.mocked(invoke)).toHaveBeenCalledWith("load_thread_history", { threadId: "thread-h" });
});

test("新版本无法读取时旧条目保留但当前内容显示不可用", async () => {
  const failed = { ...coverage, loadedTurns: 0, loadedItems: 0, turnsComplete: false,
    error: "分页及完整读取均失败" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "get_history_turns") return { coverage: failed,
      turns: [{ ...turn("turn-old", 0), sourceUpdatedAt: 199 }], total: 1, offset: 0, limit: 20 };
    if (command === "get_history_items") return { coverage: failed, items: [], total: 0, offset: 0, limit: 20 };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ThreadHistoryView threadId="thread-h" updatedAt={200} connected={false} />);
  expect(await screen.findByText("内容不可用")).toBeTruthy();
  expect(screen.getByText("旧缓存")).toBeTruthy();
  expect(screen.getByText("来源当前不可用；已保存的历史仍可浏览。")).toBeTruthy();
});

test("事实证据检查后跳到对应来源条目，过期证据保留说明", async () => {
  const fact = { id: "fact-1", turnId: "turn-25", itemId: "item-target", kind: "command",
    subject: "cargo test", operation: "executed", outcome: "succeeded", evidenceId: "evidence-1",
    contentVersion: "abcdef1234567890", ruleVersion: "structured-facts-v1" };
  let stale = false;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "get_history_turns") {
      const offset = (args as { offset: number }).offset;
      return { coverage, turns: offset === 0 ? [turn("turn-1", 0)] : [turn("turn-25", 24)], total: 25, offset, limit: 20 };
    }
    if (command === "get_history_items") {
      const { turnId } = args as { turnId: string };
      return { coverage, items: turnId === "turn-25" ? [item("item-target", turnId)] : [],
        total: turnId === "turn-25" ? 1 : 0, offset: 0, limit: 20 };
    }
    if (command === "get_source_facts") return { facts: [fact], total: 1, offset: 0, limit: 20, coverage };
    if (command === "get_source_evidence") return { evidence: [{ id: "evidence-1", excerpt: "cargo test", field: "command", contentVersion: fact.contentVersion }], total: 1, offset: 0, limit: 20 };
    if (command === "validate_source_evidence") return stale
      ? { state: "staleVersion", message: "证据内容版本已失效；请重新读取来源历史。", location: null }
      : { state: "valid", message: "证据有效。", location: { turnId: "turn-25", turnOffset: 24, offset: 0 } };
    throw new Error(`Unexpected command ${command}`);
  });
  render(<ThreadHistoryView threadId="thread-h" updatedAt={200} connected />);
  await screen.findAllByText("cargo test");
  fireEvent.click(screen.getByRole("button", { name: "检查证据并定位条目" }));
  expect(await screen.findByText("已定位到回合 turn-25 的来源条目。")).toBeTruthy();
  expect(await screen.findByText("目标正文")).toBeTruthy();
  stale = true;
  fireEvent.click(screen.getByRole("button", { name: "检查证据并定位条目" }));
  expect((await screen.findAllByText("证据内容版本已失效；请重新读取来源历史。")).length).toBeGreaterThan(0);
  expect(screen.getByText("证据过期")).toBeTruthy();
});
