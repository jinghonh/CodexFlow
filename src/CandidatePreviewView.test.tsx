// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { CandidatePreviewView } from "./CandidatePreviewView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

test("候选预览显示上限、筛选依据和双侧证据", async () => {
  vi.mocked(invoke).mockResolvedValue({ projectId: "project", threadCount: 2, neighborLimit: 10, candidateCount: 1,
    candidates: [{ id: "candidate", leftThreadId: "thread-a", rightThreadId: "thread-b", score: 40,
      reasons: [{ signal: "file", detail: "双方来源条目记录同一文件：src/lib.rs" }],
      evidence: { leftAvailable: 1, rightAvailable: 1, combinationsAvailable: 1, combinationsShown: 1,
        leftSampled: 1, rightSampled: 1, samplingRule: "稳定排序", pairs: [{ id: "pair",
          left: { id: "a", threadId: "thread-a", turnId: "turn-a", itemId: "item-a", excerpt: "左侧来源", contentVersion: "v-a" },
          right: { id: "b", threadId: "thread-b", turnId: "turn-b", itemId: "item-b", excerpt: "右侧来源", contentVersion: "v-b" },
        }] } }] });
  const select = vi.fn();
  render(<CandidatePreviewView projectId="project" refreshVersion="1" onSelectEvidence={select} />);
  expect(await screen.findByText("1 对候选")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /thread-a ↔ thread-b/ }));
  expect(screen.getByText("双侧来源证据组合 1 / 1")).toBeTruthy();
  expect(screen.getByText("左侧来源")).toBeTruthy();
  expect(screen.getByText("右侧来源")).toBeTruthy();
  fireEvent.click(screen.getAllByRole("button", { name: "定位来源条目" })[1]);
  expect(select).toHaveBeenCalledWith(expect.objectContaining({ threadId: "thread-b", itemId: "item-b" }));
});

test("无双侧来源条目时明确显示证据不足", async () => {
  vi.mocked(invoke).mockResolvedValue({ projectId: "project", threadCount: 2, neighborLimit: 10, candidateCount: 1,
    candidates: [{ id: "branch-only", leftThreadId: "thread-a", rightThreadId: "thread-b", score: 4,
      reasons: [{ signal: "branch", detail: "同一分支：main" }],
      evidence: { leftAvailable: 0, rightAvailable: 0, combinationsAvailable: 0, combinationsShown: 0,
        leftSampled: 0, rightSampled: 0, samplingRule: "稳定排序", pairs: [] } }] });
  render(<CandidatePreviewView projectId="project" refreshVersion="1" onSelectEvidence={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /thread-a ↔ thread-b/ }));
  expect(screen.getByText(/证据不足：至少一侧没有可定位的来源条目/)).toBeTruthy();
});

test("部分来源留下旧候选和过期原因供检查", async () => {
  vi.mocked(invoke).mockResolvedValue({ projectId: "project", threadCount: 1, unavailableThreads: 1,
    neighborLimit: 10, candidateCount: 0, candidates: [], staleCandidates: [{ inputVersion: "old-version",
      reason: "来源部分可读、读取失败或尚未读取新版内容；旧候选保留供检查。",
      candidate: { id: "old", leftThreadId: "thread-a", rightThreadId: "thread-b", score: 4,
        reasons: [{ signal: "branch", detail: "同一分支：main" }],
        evidence: { leftAvailable: 0, rightAvailable: 0, combinationsAvailable: 0, combinationsShown: 0,
          leftSampled: 0, rightSampled: 0, samplingRule: "稳定排序", pairs: [] } } }] });
  render(<CandidatePreviewView projectId="project" refreshVersion="1" onSelectEvidence={vi.fn()} />);
  expect(await screen.findByText(/1 条会话内容部分可读/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /旧输入版本 old-version/ }));
  expect(screen.getByRole("heading", { name: "旧候选依据" })).toBeTruthy();
  expect(screen.getByText(/以下是旧版双侧来源摘录/)).toBeTruthy();
});
