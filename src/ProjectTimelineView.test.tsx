// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { makeScale, ProjectTimelineView } from "./ProjectTimelineView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

const first = Date.parse("2026-03-07T10:00:00Z");
const later = Date.parse("2026-03-09T10:00:00Z");
const turn = (id: string, ordinal: number, start: number | null, end: number | null, durationMs: number | null,
  timeState: string, sourceStatus = "completed") => ({ id, ordinal, sourceStatus, timeState, startedAtUnixMs: start,
  completedAtUnixMs: end, intervalStartUnixMs: timeState === "complete" ? start : null,
  intervalEndUnixMs: timeState === "complete" ? end : null, durationMs, timeError: null });

const timeline = { projectId: "project", threads: [
  { threadId: "thread", title: "恢复的会话", turnsComplete: false, quality: "partial", knownDurationMs: 4_500,
    lastActivityAtUnixMs: later + 5_000, lastActivityBasis: "turnStart", turns: [
    turn("first", 0, first, first + 1_000, 1_000, "complete"),
    turn("later", 1, later, later + 3_000, 3_000, "complete"),
    turn("duration", 2, null, null, 500, "durationOnly"),
    turn("ended", 3, later + 5_000, null, null, "incomplete"),
  ] },
  { threadId: "unknown", title: "旧会话", turnsComplete: false, quality: "unknown", knownDurationMs: null,
    lastActivityAtUnixMs: null, lastActivityBasis: "unknown", turns: [] },
] };

test("跨日恢复保留空档，未知与无位置回合可检查，缩放及会话定位可用", async () => {
  vi.mocked(invoke).mockResolvedValue(timeline);
  const selectThread = vi.fn();
  render(<ProjectTimelineView projectId="project" refreshVersion="0" connected onSelectThread={selectThread} />);
  expect(await screen.findByText("未分组会话")).toBeTruthy();
  const bars = document.querySelectorAll<HTMLButtonElement>(".timeline-segment");
  expect(bars).toHaveLength(2);
  const firstLeft = Number.parseFloat(bars[0].style.left);
  const laterLeft = Number.parseFloat(bars[1].style.left);
  expect(laterLeft).toBeGreaterThan(firstLeft + Number.parseFloat(bars[0].style.width) + 20);
  expect(screen.getByText("已知时长：未知")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /恢复的会话.*已知时长/ }));
  fireEvent.click(screen.getByRole("button", { name: /#3 仅有来源时长/ }));
  expect(screen.getByText(/仅有来源时长，无时间线位置（completed）/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /#4 已结束，时间不完整/ }));
  expect(screen.getByText(/已结束，时间不完整（completed）/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "打开会话历史" }));
  expect(selectThread).toHaveBeenCalledWith("thread");
  fireEvent.click(screen.getByRole("button", { name: "放大时间线" }));
  await waitFor(() => expect(Number.parseFloat(bars[1].style.left)).toBeGreaterThan(laterLeft));
  fireEvent.click(screen.getByRole("button", { name: "周" }));
  expect(screen.getByRole("button", { name: "周" }).getAttribute("aria-pressed")).toBe("true");
});

test("系统时区跨夏令时以本地日历生成刻度，区间仍按真实毫秒定位", () => {
  const scale = makeScale(timeline.threads as Parameters<typeof makeScale>[0], "day", 1)!;
  expect(scale.ticks.length).toBeGreaterThanOrEqual(3);
  expect(scale.start).toBeLessThan(first);
  if (Intl.DateTimeFormat().resolvedOptions().timeZone === "America/New_York") {
    const springDay = scale.ticks.find((tick) => tick.label === "3月8日")!;
    const nextDay = scale.ticks.find((tick) => tick.label === "3月9日")!;
    expect(nextDay.at - springDay.at).toBe(23 * 60 * 60 * 1000);
    const fall = Date.parse("2026-11-01T06:30:00Z");
    const fallScale = makeScale([{ ...timeline.threads[0], turns: [turn("fall", 0, fall, fall + 3_600_000, 3_600_000, "complete")] }] as Parameters<typeof makeScale>[0], "day", 1)!;
    const fallDay = fallScale.ticks.find((tick) => tick.label === "11月1日")!;
    expect(fallScale.end - fallDay.at).toBe(25 * 60 * 60 * 1000);
  }
});

test("同刻起止、重叠及无效时间都有明确可见状态", async () => {
  vi.mocked(invoke).mockResolvedValue({ projectId: "project", threads: [{ ...timeline.threads[0], turns: [
    turn("same", 0, first, first, 0, "complete"),
    turn("overlap-a", 1, first + 10_000, first + 60_000, 50_000, "complete"),
    turn("overlap-b", 2, first + 20_000, first + 40_000, 20_000, "complete"),
    { ...turn("invalid", 3, first + 90_000, first + 80_000, null, "invalid"),
      intervalStartUnixMs: null, intervalEndUnixMs: null, timeError: "结束时间早于开始时间" },
  ] }] });
  render(<ProjectTimelineView projectId="project" refreshVersion="0" connected={false} onSelectThread={vi.fn()} />);
  await screen.findByText("未分组会话");
  const bars = document.querySelectorAll<HTMLButtonElement>(".timeline-segment");
  expect(bars).toHaveLength(3);
  expect(bars[0].classList.contains("instant")).toBe(true);
  expect(bars[1].style.top).not.toBe(bars[2].style.top);
  fireEvent.click(screen.getByRole("button", { name: /恢复的会话.*已知时长/ }));
  fireEvent.click(screen.getByRole("button", { name: /#4 来源时间无效/ }));
  expect(screen.getByText("结束时间早于开始时间")).toBeTruthy();
});

test("可按需读取项目缺失回合并更新活动段", async () => {
  let loaded = false;
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "load_thread_history") { loaded = true; return {}; }
    if (command === "get_project_timeline") return {
      projectId: "project", threads: [{ ...timeline.threads[0], turnsComplete: loaded,
        turns: loaded ? [timeline.threads[0].turns[0]] : [] }],
    };
    throw new Error(`未知命令 ${command}`);
  });
  render(<ProjectTimelineView projectId="project" refreshVersion="0" connected onSelectThread={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "读取缺失回合" }));
  await waitFor(() => expect(document.querySelectorAll(".timeline-segment")).toHaveLength(1));
  expect(vi.mocked(invoke)).toHaveBeenCalledWith("load_thread_history", { threadId: "thread" });
});

test("工作流泳道按主要归属显示，同一会话只出现一次", async () => {
  vi.mocked(invoke).mockResolvedValue(timeline);
  render(<ProjectTimelineView projectId="project" refreshVersion="0" connected={false} onSelectThread={vi.fn()}
    visibleThreadIds={new Set(["thread", "unknown"])} workstreams={[{ id: "stream", name: "实现功能", members: ["thread"] }]} />);
  await screen.findByText("实现功能");
  expect(document.querySelectorAll(".timeline-row")).toHaveLength(2);
  expect(screen.getByText("未分组会话")).toBeTruthy();
  expect(screen.getAllByRole("button", { name: /恢复的会话.*已知时长/ })).toHaveLength(1);
});

test("大项目时间线按页展示会话且可继续查看", async () => {
  vi.mocked(invoke).mockResolvedValue({ projectId: "project", threads: Array.from({ length: 81 }, (_, index) => ({
    ...timeline.threads[0], threadId: `thread-${index}`, title: `会话 ${index}`,
    lastActivityAtUnixMs: first + index * 1_000,
  })) });
  render(<ProjectTimelineView projectId="project" refreshVersion="0" connected={false} onSelectThread={vi.fn()} />);
  await screen.findByText("未分组会话");
  expect(document.querySelectorAll(".timeline-row")).toHaveLength(40);
  fireEvent.click(screen.getByRole("button", { name: /显示更多会话（已显示 40 \/ 81）/ }));
  expect(document.querySelectorAll(".timeline-row")).toHaveLength(80);
  fireEvent.click(screen.getByRole("button", { name: /显示更多会话（已显示 80 \/ 81）/ }));
  expect(document.querySelectorAll(".timeline-row")).toHaveLength(81);
});
