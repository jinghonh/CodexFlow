// @vitest-environment jsdom
import { beforeEach, expect, test, vi } from "vitest";

const mock = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mock.invoke }));

const storageKey = "codexflow.analysis-start-intents";
const selection = { summary: false, relations: true, naming: false };
const limits = { callLimit: 20, concurrencyLimit: 2, timeoutSeconds: 180, retryLimit: 2, inputCharacterLimit: 40000 };
const saved = new Map<string, string>();
Object.defineProperty(window, "localStorage", { configurable: true, value: {
  getItem: (key: string) => saved.get(key) ?? null,
  setItem: (key: string, value: string) => { saved.set(key, value); },
  clear: () => saved.clear(),
} });

beforeEach(() => { saved.clear(); mock.invoke.mockReset(); vi.resetModules(); });

test("重开应用时重试尚未形成持久运行的启动请求", async () => {
  const intent = { id: "prepare-1", kind: "project", projectId: "project", limits,
    stageSelection: selection, startedAt: Date.now() - 1000, state: "preparing" };
  window.localStorage.setItem(storageKey, JSON.stringify([intent]));
  mock.invoke.mockImplementation(async (command: string) => command === "get_model_runs"
    ? { runs: [] } : { id: "analysis-1" });
  const { recoverAnalysisStarts } = await import("./analysisStartIntent");
  await recoverAnalysisStarts();
  expect(mock.invoke).toHaveBeenCalledWith("start_project_analysis", {
    projectId: "project", limits, stageSelection: selection,
  });
  expect(JSON.parse(window.localStorage.getItem(storageKey) ?? "[]")).toEqual([]);
});

test("后端已经保存运行时不重复启动；失败请求也不在下次启动自动重试", async () => {
  const startedAt = Date.now() - 1000;
  const intent = { id: "prepare-2", kind: "project", projectId: "project", limits,
    stageSelection: selection, startedAt, state: "preparing" };
  window.localStorage.setItem(storageKey, JSON.stringify([intent]));
  mock.invoke.mockResolvedValue({ runs: [{ kind: "project", projectId: "project", startedAtUnixMs: startedAt + 100,
    stageSelection: selection }] });
  const first = await import("./analysisStartIntent");
  await first.recoverAnalysisStarts();
  expect(mock.invoke).toHaveBeenCalledTimes(1);

  window.localStorage.setItem(storageKey, JSON.stringify([{ ...intent, state: "failed", error: "服务不可用" }]));
  vi.resetModules();
  mock.invoke.mockReset().mockResolvedValue({ runs: [] });
  const second = await import("./analysisStartIntent");
  await second.recoverAnalysisStarts();
  expect(mock.invoke).toHaveBeenCalledTimes(1);
});
