import { invoke } from "@tauri-apps/api/core";

const enabled = import.meta.env.VITE_CODEXFLOW_PERF_PROBE === "1";
const started = new Map<string, number>();
let startupRecorded = false;

function afterPaint(callback: () => void) {
  requestAnimationFrame(() => requestAnimationFrame(callback));
}

function record(kind: string, elapsedMs?: number) {
  void invoke("record_perf_sample", { kind, elapsedMs: elapsedMs ?? null }).catch(() => {});
}

export function recordStartupReady() {
  if (!enabled || startupRecorded) return;
  startupRecorded = true;
  afterPaint(() => record("startup"));
}

export function startResponse(kind: "query" | "filter") {
  if (enabled) started.set(kind, performance.now());
}

export function finishResponse() {
  if (!enabled) return;
  for (const kind of ["query", "filter"]) {
    const start = started.get(kind);
    if (start === undefined) continue;
    started.delete(kind);
    afterPaint(() => record(kind, performance.now() - start));
  }
}

export function startEvidence(threadId: string) {
  if (!enabled) return;
  const start = performance.now();
  for (const kind of ["evidence_turns", "evidence_items", "evidence_facts"]) started.set(`${kind}:${threadId}`, start);
}

export function finishEvidence(kind: "evidence_turns" | "evidence_items" | "evidence_facts", threadId: string) {
  if (!enabled) return;
  const key = `${kind}:${threadId}`;
  const start = started.get(key);
  if (start === undefined) return;
  started.delete(key);
  afterPaint(() => record(kind, performance.now() - start));
}
