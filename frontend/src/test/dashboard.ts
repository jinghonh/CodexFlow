import { vi } from "vitest";
import type { DashboardApi } from "../api";
import type { ConversationOverlayUpdate, DashboardSnapshot, HealthResponse, ProjectView } from "../types";

export const project: ProjectView = {
  originalPath: "/projects/codexflow",
  realPath: "/projects/codexflow",
  gitRoot: null,
  worktreeRoot: null,
  isGitProject: false,
  graphFile: "/projects/codexflow/.codex/graph.yaml",
  graphFileStatus: "absent",
};

export const health: HealthResponse = {
  status: "ok",
  listenHost: "127.0.0.1",
  source: { status: "unavailable", userAgent: null, error: null },
  project: null,
};

export const snapshot: DashboardSnapshot = {
  project,
  source: {
    status: "ready",
    generatedAt: "2024-01-01T02:00:00Z",
    userAgent: "Codex Desktop/0.150.1 fixture",
    error: null,
  },
  graph: {
    etag: "absent",
    fileStatus: "absent",
    nodes: [
      {
        id: "active-id",
        displayTitle: "Map the local runtime",
        missing: false,
        hidden: false,
        layout: null,
      },
      {
        id: "archived-id",
        displayTitle: "Archive the first pass",
        missing: false,
        hidden: false,
        layout: null,
      },
    ],
    edges: [],
  },
  conversations: [
    {
      id: "active-id",
      displayTitle: "Map the local runtime",
      codex: {
        title: "Map the local runtime",
        preview: "Map the local runtime",
        createdAt: "2024-01-01T00:00:00Z",
        updatedAt: "2024-01-01T01:00:00Z",
        recencyAt: "2024-01-01T01:00:00Z",
        cwd: "/projects/codexflow",
        source: "cli",
        archived: false,
        historyMode: null,
        status: "idle",
        projectId: null,
        gitInfo: null,
      },
      overlay: { title: null, tags: [], status: "none", note: null, hidden: false, layout: null },
      derived: { missing: false, unlinked: true, sourceAvailable: true, validObservationRange: true },
    },
    {
      id: "archived-id",
      displayTitle: "Archive the first pass",
      codex: {
        title: null,
        preview: "Archive the first pass",
        createdAt: "2024-01-02T00:00:00Z",
        updatedAt: "2024-01-02T00:00:00Z",
        recencyAt: null,
        cwd: "/projects/codexflow",
        source: "vscode",
        archived: true,
        historyMode: null,
        status: "notLoaded",
        projectId: null,
        gitInfo: null,
      },
      overlay: { title: null, tags: [], status: "none", note: null, hidden: false, layout: null },
      derived: { missing: false, unlinked: true, sourceAvailable: true, validObservationRange: true },
    },
  ],
  excludedConversations: [],
};

export function apiDouble(overrides: Partial<DashboardApi> = {}): DashboardApi {
  const defaults: DashboardApi = {
    health: vi.fn().mockResolvedValue(health),
    pickProjectDirectory: vi.fn().mockResolvedValue({ path: null }),
    selectProject: vi.fn().mockResolvedValue({ project, source: health.source }),
    snapshot: vi.fn().mockResolvedValue(snapshot),
    refresh: vi.fn().mockResolvedValue(snapshot),
    migrateGraph: vi.fn().mockResolvedValue({ ...snapshot, backupPath: null }),
    saveCopy: vi.fn().mockResolvedValue({ copyPath: "/projects/codexflow/.codex/graph.yaml.copy.fixture" }),
    updateNode: vi.fn().mockResolvedValue(snapshot),
    createEdge: vi.fn().mockResolvedValue(snapshot),
    updateEdge: vi.fn().mockResolvedValue(snapshot),
    deleteEdge: vi.fn().mockResolvedValue(snapshot),
  };
  return {
    health: overrides.health ?? defaults.health,
    pickProjectDirectory: overrides.pickProjectDirectory ?? defaults.pickProjectDirectory,
    selectProject: overrides.selectProject ?? defaults.selectProject,
    snapshot: overrides.snapshot ?? defaults.snapshot,
    refresh: overrides.refresh ?? defaults.refresh,
    migrateGraph: overrides.migrateGraph ?? defaults.migrateGraph,
    saveCopy: overrides.saveCopy ?? defaults.saveCopy,
    updateNode: overrides.updateNode ?? defaults.updateNode,
    createEdge: overrides.createEdge ?? defaults.createEdge,
    updateEdge: overrides.updateEdge ?? defaults.updateEdge,
    deleteEdge: overrides.deleteEdge ?? defaults.deleteEdge,
  };
}

export function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

export function nodeSnapshot(id: string, changes: ConversationOverlayUpdate, etag: string, base = snapshot): DashboardSnapshot {
  return {
    ...base,
    graph: { ...base.graph, etag, nodes: base.graph.nodes.map((node) => node.id === id ? {
      ...node, ...(changes.layout !== undefined ? { layout: changes.layout } : {}),
    } : node) },
    conversations: base.conversations.map((conversation) => conversation.id === id ? {
      ...conversation, displayTitle: changes.title || conversation.displayTitle,
      overlay: { ...conversation.overlay, ...changes },
    } : conversation),
  };
}
