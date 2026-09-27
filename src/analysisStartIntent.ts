import { invoke } from "@tauri-apps/api/core";
import { formatAppError } from "./appError";
import type { ModelRunsSnapshot, StageSelection } from "./analysisProgress";

export type AnalysisStartIntent = {
  id: string; kind: "project" | "summary"; projectId?: string; threadId?: string;
  limits?: { callLimit: number; concurrencyLimit: number; timeoutSeconds: number; retryLimit: number; inputCharacterLimit: number };
  stageSelection?: StageSelection; startedAt: number; state: "preparing" | "failed"; error?: string;
};

const storageKey = "codexflow.analysis-start-intents";
const changeEvent = "codexflow-analysis-start-intents-changed";
let intents: AnalysisStartIntent[] = [];
let recoveryStarted = false;

try {
  const saved = JSON.parse(window.localStorage.getItem(storageKey) ?? "[]");
  if (Array.isArray(saved)) intents = saved.filter((item) => item && typeof item.id === "string" &&
    (item.kind === "project" || item.kind === "summary") &&
    (item.state === "preparing" || item.state === "failed"));
} catch { /* Local storage may be disabled; in-memory progress still works. */ }

export function analysisStartIntents(): AnalysisStartIntent[] { return [...intents]; }
export const analysisStartIntentsChanged = changeEvent;

function save() {
  try { window.localStorage.setItem(storageKey, JSON.stringify(intents)); } catch { /* Keep this session's progress. */ }
  window.dispatchEvent(new Event(changeEvent));
}

function begin(intent: Omit<AnalysisStartIntent, "id" | "startedAt" | "state">): AnalysisStartIntent {
  const entry: AnalysisStartIntent = { ...intent, id: `preparing-${Date.now()}-${Math.random().toString(36).slice(2)}`,
    startedAt: Date.now(), state: "preparing" };
  intents = [entry, ...intents];
  save();
  return entry;
}

function finish(id: string) { intents = intents.filter((item) => item.id !== id); save(); }
export function dismissAnalysisStartIntent(id: string) { finish(id); }
function fail(id: string, error: unknown) {
  intents = intents.map((item) => item.id === id
    ? { ...item, state: "failed", error: formatAppError(error, "分析启动失败。") } : item);
  save();
}

export async function startTrackedProjectAnalysis<T>(
  projectId: string, limits: AnalysisStartIntent["limits"], stageSelection: StageSelection,
): Promise<T> {
  const intent = begin({ kind: "project", projectId, limits, stageSelection });
  try {
    const run = await invoke<T>("start_project_analysis", { projectId, limits, stageSelection });
    finish(intent.id);
    return run;
  } catch (error) { fail(intent.id, error); throw error; }
}

export async function startTrackedThreadSummary<T>(threadId: string): Promise<T> {
  const intent = begin({ kind: "summary", threadId });
  try {
    const run = await invoke<T>("start_thread_summary", { threadId });
    finish(intent.id);
    return run;
  } catch (error) { fail(intent.id, error); throw error; }
}

export function analysisIntentHasRun(intent: AnalysisStartIntent, snapshot: ModelRunsSnapshot): boolean {
  const after = intent.startedAt;
  if (intent.kind === "summary") return snapshot.runs.some((run) =>
    run.kind === "summary" && run.threadId === intent.threadId && run.startedAtUnixMs >= after);
  return snapshot.runs.some((run) => run.kind === "project" && run.projectId === intent.projectId &&
    run.startedAtUnixMs >= after &&
    run.stageSelection?.summary === intent.stageSelection?.summary &&
    run.stageSelection?.relations === intent.stageSelection?.relations &&
    run.stageSelection?.naming === intent.stageSelection?.naming);
}

export async function recoverAnalysisStarts() {
  if (recoveryStarted) return;
  recoveryStarted = true;
  let snapshot: ModelRunsSnapshot;
  try { snapshot = await invoke<ModelRunsSnapshot>("get_model_runs"); }
  catch { recoveryStarted = false; return; }
  for (const intent of analysisStartIntents().filter((item) => item.state === "preparing")) {
    if (analysisIntentHasRun(intent, snapshot)) { finish(intent.id); continue; }
    try {
      if (intent.kind === "project" && intent.projectId && intent.limits && intent.stageSelection)
        await invoke("start_project_analysis", { projectId: intent.projectId, limits: intent.limits, stageSelection: intent.stageSelection });
      else if (intent.kind === "summary" && intent.threadId)
        await invoke("start_thread_summary", { threadId: intent.threadId });
      else throw new Error("保存的启动请求不完整。");
      finish(intent.id);
    } catch (error) { fail(intent.id, error); }
  }
}
