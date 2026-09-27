export type StageSelection = { summary: boolean; relations: boolean; naming: boolean };
export type ProjectRun = {
  id: string; projectId: string;
  state: "queued" | "running" | "cancelling" | "cancelled" | "paused" | "complete" | "partial" | "failed";
  stageSelection?: StageSelection; pauseReason: string | null;
  batchNumber: number; batchCalls: number; totalCalls: number; totalQuestions: number;
  jevPinnedModel?: string | null; jevProbeAttempts?: number;
  inputTokens: number | null; outputTokens: number | null;
  processed: number; succeeded: number; failed: number; pending: number; plannedItems?: number;
  interrupted: boolean; error: { message: string } | null;
  limits: { callLimit: number };
  startedAtUnixMs: number; finishedAtUnixMs: number | null;
  units: { id: string; stage: "summary" | "relation" | "evidenceSelection" | "naming";
    state: string; attempts: number; actualModel: string | null; error: { message: string } | null }[];
};

export type SummaryRun = {
  id: string; threadId: string; projectId?: string | null;
  state: "running" | "cancelling" | "complete" | "failed" | "cancelled";
  model: string; temporaryThreadId: string | null; turnId: string | null; reusedCache: boolean;
  modelCalls?: number; interrupted?: boolean; error: { message: string } | null;
  startedAtUnixMs: number; finishedAtUnixMs: number | null;
};

export type SemanticRun = {
  id: string;
  state: ProjectRun["state"];
  stage: "preparing" | "embedding" | "topicAssignment" | "crossProjectRelation";
  embeddingCompleted: number; embeddingTotal: number;
  assignmentCompleted: number; assignmentTotal: number;
  relationCompleted: number; relationTotal: number;
  totalCalls: number; interrupted: boolean;
  result: { indexed: number; reused: number; unavailableSummaries: number; topicAssignments: number;
    pendingTopicAssignments: number; jevCalls: number; actualModel: string | null;
    crossProjectCandidates: number; crossProjectRelations: number; pendingCrossProjectPairs: number } | null;
  error: { message: string } | null;
  startedAtUnixMs: number; finishedAtUnixMs: number | null;
};

export type ModelRunSummary = {
  id: string; kind: "project" | "summary" | "semantic"; name: string;
  state: ProjectRun["state"]; stage: string; projectId: string | null; threadId: string | null;
  completed: number; total: number | null; unit: string; calls: number;
  startedAtUnixMs: number; error: string | null; result: string;
  stageSelection: StageSelection | null;
};
export type ModelRunsSnapshot = { runs: ModelRunSummary[] };
export type Progress = { stage: string; completed: number; total: number | null; unit: string; calls: number };

export const projectStateNames: Record<ProjectRun["state"], string> = {
  queued: "待执行", running: "执行中", cancelling: "取消中", cancelled: "已取消",
  paused: "已暂停", complete: "完成", partial: "部分完成", failed: "失败",
};

export function projectRunName(run: ProjectRun): string {
  const selection = run.stageSelection;
  if (selection?.summary && !selection.relations && !selection.naming) return "项目会话总结";
  if (selection?.relations && !selection.summary && !selection.naming) return "候选关系判断";
  if (selection?.naming && !selection.summary && !selection.relations) return "工作流命名";
  return "项目分析";
}

export function projectProgress(run: ProjectRun): Progress {
  const selection = run.stageSelection;
  if (!selection || [selection.summary, selection.relations, selection.naming].filter(Boolean).length !== 1) {
    const running = run.units.find((unit) => unit.state === "running")?.stage;
    const stage = running === "relation" || running === "evidenceSelection" ? "候选关系判断"
      : running === "naming" ? "工作流命名" : "会话总结";
    const total = Math.max(run.plannedItems ?? 0, run.units.length);
    return { stage, completed: run.processed, total: total || (run.state === "complete" ? 0 : null),
      unit: "项", calls: run.totalCalls };
  }
  const stage = selection?.relations && !selection.summary && !selection.naming
    ? run.units.length === 0 && run.state !== "complete" ? "准备候选会话对" : "候选关系判断"
    : selection?.naming && !selection.summary && !selection.relations ? "工作流命名"
      : selection?.summary && !selection.relations && !selection.naming ? "会话总结"
        : run.units.find((unit) => unit.state === "running")?.stage === "naming" ? "工作流命名"
          : run.units.find((unit) => unit.state === "running")?.stage === "relation" ? "候选关系判断" : "会话总结";
  const units = stage === "候选关系判断" || stage === "准备候选会话对"
    ? run.units.filter((unit) => unit.stage === "relation" || unit.stage === "evidenceSelection")
    : stage === "工作流命名" ? run.units.filter((unit) => unit.stage === "naming")
      : run.units.filter((unit) => unit.stage === "summary");
  const completed = units.filter((unit) => unit.state === "succeeded" || unit.state === "failed").length;
  const planned = run.plannedItems ?? 0;
  const total = Math.max(planned, units.length);
  return { stage, completed, total: total || (run.state === "complete" ? 0 : null),
    unit: stage === "候选关系判断" || stage === "准备候选会话对" ? "对" : stage === "工作流命名" ? "组" : "条",
    calls: run.totalCalls };
}

export function semanticProgress(run: SemanticRun): Progress {
  if (run.stage === "embedding") return { stage: "生成语义向量", completed: run.embeddingCompleted,
    total: run.embeddingTotal, unit: "条", calls: run.totalCalls };
  if (run.stage === "topicAssignment") return { stage: "分配主题", completed: run.assignmentCompleted,
    total: run.assignmentTotal, unit: "条", calls: run.totalCalls };
  if (run.stage === "crossProjectRelation") return { stage: "判断跨项目候选关系", completed: run.relationCompleted,
    total: run.relationTotal, unit: "对", calls: run.totalCalls };
  return { stage: "准备全局主题分析", completed: 0, total: null, unit: "项", calls: run.totalCalls };
}

export function summaryProgress(run: SummaryRun): Progress {
  return { stage: run.state === "running" ? "生成会话总结" : "会话总结",
    completed: run.state === "complete" || run.state === "failed" ? 1 : 0,
    total: 1, unit: "条", calls: run.modelCalls ?? 0 };
}
