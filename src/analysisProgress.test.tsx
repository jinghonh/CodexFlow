// @vitest-environment jsdom
import { afterEach, expect, test } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { ProgressMeter } from "./ProgressMeter";
import { projectProgress, semanticProgress } from "./analysisProgress";
import type { ProjectRun, SemanticRun } from "./analysisProgress";

afterEach(cleanup);

test("候选总数先用预览计划，新增候选后更新真实总数", () => {
  const run: ProjectRun = {
    id: "run", projectId: "project", state: "queued", stageSelection: { summary: false, relations: true, naming: false },
    pauseReason: null, batchNumber: 1, batchCalls: 0, totalCalls: 0, totalQuestions: 0,
    inputTokens: null, outputTokens: null, processed: 0, succeeded: 0, failed: 0, pending: 0,
    plannedItems: 5, interrupted: false, error: null, limits: { callLimit: 100 },
    startedAtUnixMs: 1, finishedAtUnixMs: null, units: [],
  };
  const planned = projectProgress(run);
  expect(planned).toMatchObject({ stage: "准备候选会话对", completed: 0, total: 5, unit: "对" });

  run.state = "running";
  run.units = Array.from({ length: 7 }, (_, index) => ({
    id: `candidate-${index}`, stage: "relation" as const, state: index < 2 ? "succeeded" : "pending",
    attempts: index < 2 ? 1 : 0, actualModel: null, error: null,
  }));
  const progress = projectProgress(run);
  expect(progress).toMatchObject({ stage: "候选关系判断", completed: 2, total: 7 });
  render(<ProgressMeter progress={progress} label="候选关系判断进度" />);
  expect(screen.getByRole("progressbar", { name: "候选关系判断进度" }).getAttribute("aria-valuetext"))
    .toContain("已处理 2 / 7 对");
});

test("全局主题从向量处理切换到主题归属后保留分项计数", () => {
  const run: SemanticRun = {
    id: "semantic", state: "running", stage: "topicAssignment",
    embeddingCompleted: 8, embeddingTotal: 8, assignmentCompleted: 3, assignmentTotal: 8,
    relationCompleted: 0, relationTotal: 0, totalCalls: 4, result: null, error: null,
    startedAtUnixMs: 1, finishedAtUnixMs: null, interrupted: false,
  };
  expect(semanticProgress(run)).toMatchObject({ stage: "分配主题", completed: 3, total: 8, unit: "条" });
  run.stage = "crossProjectRelation";
  run.relationCompleted = 2;
  run.relationTotal = 5;
  expect(semanticProgress(run)).toMatchObject({ stage: "判断跨项目候选关系", completed: 2, total: 5, unit: "对" });
});
