import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

if (!window.PointerEvent) window.PointerEvent = MouseEvent as typeof PointerEvent;
// Node 26 can expose an unavailable storage slot in jsdom workers.
const storedValues = new Map<string, string>();
const testStorage: Storage = {
  get length() { return storedValues.size; },
  clear: () => storedValues.clear(),
  getItem: (key) => storedValues.get(key) ?? null,
  key: (index) => [...storedValues.keys()][index] ?? null,
  removeItem: (key) => { storedValues.delete(key); },
  setItem: (key, value) => { storedValues.set(key, String(value)); },
};
Object.defineProperty(globalThis, "localStorage", { configurable: true, value: testStorage });
afterEach(() => { cleanup(); localStorage.clear(); });
