// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ModelRunsPanel } from "./ModelRunsPanel";

const mock = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));
vi.mock("./analysisStartIntent", () => ({
  analysisStartIntents: () => [], analysisStartIntentsChanged: "analysis-starts-changed",
  analysisIntentHasRun: () => false,
  dismissAnalysisStartIntent: () => {},
  recoverAnalysisStarts: async () => {},
}));
afterEach(() => { cleanup(); mock.invoke.mockReset(); });

test("统一运行列表展示关系候选进度并允许取消", async () => {
  mock.invoke.mockImplementation(async (command: string) => command === "get_model_runs" ? {
    runs: [{ id: "analysis-1", kind: "project", name: "候选关系判断", projectId: "project", threadId: null,
      state: "running", stage: "候选关系判断", completed: 2, total: 5, unit: "对", calls: 2,
      startedAtUnixMs: Date.now(), error: null, result: "成功 2 · 待处理 3",
      stageSelection: { summary: false, relations: true, naming: false },
    }],
  } : {});
  render(<ModelRunsPanel onOpen={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: /运行列表/ }));
  expect(await screen.findByText(/已处理 2 \/ 5 对/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "取消" }));
  expect(mock.invoke).toHaveBeenCalledWith("cancel_analysis_run", { runId: "analysis-1" });
});
