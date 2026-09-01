import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ApiError, createApi } from "./api";
import { App } from "./App";
import type { DashboardApi } from "./api";
import type { DashboardSnapshot, HealthResponse, ProjectView } from "./types";

const project: ProjectView = {
  originalPath: "/projects/codexflow",
  realPath: "/projects/codexflow",
  gitRoot: null,
  worktreeRoot: null,
  isGitProject: false,
  graphFile: "/projects/codexflow/.codex/graph.yaml",
  graphFileStatus: "absent",
};

const health: HealthResponse = {
  status: "ok",
  listenHost: "127.0.0.1",
  source: { status: "unavailable", userAgent: null, error: null },
  project: null,
};

const snapshot: DashboardSnapshot = {
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

function apiDouble(overrides: Partial<DashboardApi> = {}): DashboardApi {
  const defaults: DashboardApi = {
    health: vi.fn().mockResolvedValue(health),
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

function httpApiDouble(snapshotToServe: DashboardSnapshot): DashboardApi {
  const fetchLike = vi.fn(async (input: RequestInfo | URL) => {
    switch (String(input)) {
      case "/api/health":
        return jsonResponse(health);
      case "/api/project/select":
        return jsonResponse({ project, source: health.source });
      case "/api/snapshot":
        return jsonResponse(snapshotToServe);
      default:
        throw new Error(`Unexpected fixture request: ${String(input)}`);
    }
  });
  return createApi(fetchLike);
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

function snapshotWithTimeline(base: DashboardSnapshot = snapshot): DashboardSnapshot {
  return {
    ...base,
    timeline: {
      granularity: "day",
      timezone: "UTC",
      ranges: base.conversations
        .filter((conversation) => !conversation.derived.missing)
        .map((conversation, index) => ({
          conversationId: conversation.id,
          start: `2024-01-0${index + 1}T00:00:00Z`,
          end: `2024-01-0${index + 1}T01:00:00Z`,
          valid: true,
          isPoint: false,
          error: null,
        })),
      buckets: [
        {
          start: "2024-01-01T00:00:00Z",
          end: "2024-01-03T00:00:00Z",
          label: "2024-01-01 → 2024-01-02",
          overlapCount: base.conversations.filter((conversation) => !conversation.derived.missing).length,
        },
      ],
      warnings: [],
    },
  };
}

describe("Dashboard conversation list", () => {
  it("shows loading, then active and archived source metadata after project selection", async () => {
    const user = userEvent.setup();
    let finishSelection: ((value: { project: ProjectView; source: HealthResponse["source"] }) => void) | undefined;
    const selectionPending = new Promise<{ project: ProjectView; source: HealthResponse["source"] }>((resolve) => {
      finishSelection = resolve;
    });
    const api = apiDouble({ selectProject: vi.fn().mockReturnValue(selectionPending) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    expect(screen.getByRole("status")).toHaveTextContent("Loading conversations");
    finishSelection?.({ project, source: health.source });
    const conversationList = within(await screen.findByRole("table", { name: "Conversation list" }));

    expect(conversationList.getByText("active-id")).toBeInTheDocument();
    expect(conversationList.getByText("archived-id")).toBeInTheDocument();
    expect(conversationList.getByText("Archived")).toBeInTheDocument();
    expect(conversationList.getByText("vscode")).toBeInTheDocument();
    expect(screen.getAllByText("/projects/codexflow").length).toBeGreaterThan(0);
    expect(api.selectProject).toHaveBeenCalledWith("/projects/codexflow");
  });

  it("shows Source unavailable when the snapshot cannot be loaded", async () => {
    const user = userEvent.setup();
    const api = apiDouble({
      snapshot: vi.fn().mockRejectedValue(
        new ApiError(503, {
          code: "source_unavailable",
          message: "fixture app-server failed",
          details: null,
          retryable: true,
        }),
      ),
    });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Source unavailable");
    expect(alert).toHaveTextContent("fixture app-server failed");
  });

  it("renders a chosen conversation without changing its source ID", async () => {
    const user = userEvent.setup();
    const api = apiDouble();
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const conversationList = within(await screen.findByRole("table", { name: "Conversation list" }));
    await user.click(conversationList.getByText("Map the local runtime"));

    await waitFor(() => expect(screen.getByRole("region", { name: "Conversation detail" })).toHaveTextContent("active-id"));
  });

  it("keeps the last complete list visible while marking the source stale", async () => {
    const user = userEvent.setup();
    const staleSnapshot: DashboardSnapshot = {
      ...snapshot,
      source: {
        ...snapshot.source,
        status: "stale",
        error: {
          code: "source_unavailable",
          message: "refresh failed",
          details: null,
          retryable: true,
        },
      },
    };
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(staleSnapshot) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    expect(await screen.findByText("Source stale")).toBeInTheDocument();
    expect(within(screen.getByRole("table", { name: "Conversation list" })).getByText("Map the local runtime")).toBeInTheDocument();
  });

  it("shows a stale marker after a refresh failure while retaining the previous views", async () => {
    const user = userEvent.setup();
    const staleSnapshot: DashboardSnapshot = {
      ...snapshot,
      source: {
        ...snapshot.source,
        status: "stale",
        error: {
          code: "source_unavailable",
          message: "active page failed",
          details: { archived: false },
          retryable: true,
        },
      },
    };
    const refresh = vi.fn().mockResolvedValue(staleSnapshot);
    const api = apiDouble({ refresh });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(screen.getByRole("button", { name: "Refresh source" }));

    expect(await screen.findByText("Source stale")).toBeInTheDocument();
    expect(within(screen.getByRole("table", { name: "Conversation list" })).getByText("active-id")).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Conversation graph" })).toBeInTheDocument();
    expect(refresh).toHaveBeenCalledWith();
  });

  it("keeps the last complete views visible when a refresh endpoint is unavailable", async () => {
    const user = userEvent.setup();
    const refresh = vi.fn().mockRejectedValue(
      new ApiError(503, {
        code: "source_unavailable",
        message: "The source could not be reached.",
        details: { hasCompleteSnapshot: true },
        retryable: true,
      }),
    );
    const api = apiDouble({ refresh });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(screen.getByRole("button", { name: "Refresh source" }));

    const refreshAlert = await screen.findByRole("alert");
    expect(refreshAlert).toHaveTextContent("Refresh failed · Error category: source_unavailable. The source could not be reached.");
    expect(refreshAlert).toHaveTextContent("The last complete snapshot is preserved.");
    expect(refreshAlert).toHaveTextContent("Retryable: yes.");
    expect(within(screen.getByRole("table", { name: "Conversation list" })).getByText("active-id")).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Conversation graph" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Conversation timeline" })).toBeInTheDocument();
  });

  it("can retry the source from a selected Project after the initial load is unavailable", async () => {
    const user = userEvent.setup();
    const snapshotRequest = vi.fn().mockRejectedValue(
      new ApiError(503, {
        code: "source_unavailable",
        message: "The source is not ready yet.",
        details: null,
        retryable: true,
      }),
    );
    const refresh = vi.fn().mockResolvedValue(snapshot);
    const api = apiDouble({ snapshot: snapshotRequest, refresh });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    expect(await screen.findByText("Source unavailable")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Refresh source" }));

    await waitFor(() => expect(refresh).toHaveBeenCalledWith());
    expect(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("active-id")).toBeInTheDocument();
  });

  it("keeps a conversation with an invalid observation range visible with a warning", async () => {
    const user = userEvent.setup();
    const invalidSnapshot: DashboardSnapshot = {
      ...snapshot,
      conversations: [
        {
          ...snapshot.conversations[0],
          id: "invalid-time-id",
          displayTitle: "Needs time review",
          codex: { ...snapshot.conversations[0].codex, createdAt: null },
          derived: { ...snapshot.conversations[0].derived, validObservationRange: false },
        },
      ],
    };
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(invalidSnapshot) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    expect(await screen.findByText("Invalid observation range")).toBeInTheDocument();
    expect(screen.getByText("Needs time review")).toBeInTheDocument();
  });

  it("explains source conversations excluded by the selected Project boundary", async () => {
    const user = userEvent.setup();
    const excludedSnapshot: DashboardSnapshot = {
      ...snapshot,
      excludedConversations: [
        {
          id: "outside-id",
          cwd: "/projects/other",
          resolvedCwd: "/projects/other",
          gitRoot: null,
          worktreeRoot: null,
          reason: "outside_project",
        },
      ],
    };
    const api = httpApiDouble(excludedSnapshot);
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const membership = await screen.findByRole("region", { name: "Project membership" });
    expect(membership).toHaveTextContent("1 conversation excluded");
    expect(membership).toHaveTextContent("/projects/other");
    expect(membership).toHaveTextContent("Outside the selected Project");
    expect(membership).toHaveTextContent("nested Git repositories are excluded");
    expect(membership).not.toHaveTextContent("its Git worktree");
  });

  it("refreshes the source while preserving selection and filters and discovers new conversations", async () => {
    const user = userEvent.setup();
    const initialSnapshot = snapshotWithTimeline();
    const newArchived = {
      ...snapshot.conversations[1],
      id: "new-archived-id",
      displayTitle: "New archived conversation",
      codex: { ...snapshot.conversations[1].codex, title: "New archived conversation" },
    };
    const refreshedSnapshot = snapshotWithTimeline({
      ...snapshot,
      source: { ...snapshot.source, generatedAt: "2024-01-03T02:00:00Z" },
      conversations: [...snapshot.conversations, newArchived],
      graph: {
        ...snapshot.graph,
        nodes: [...snapshot.graph.nodes, {
          id: newArchived.id,
          displayTitle: newArchived.displayTitle,
          missing: false,
          hidden: false,
          layout: null,
        }],
      },
    });
    const refresh = vi.fn().mockResolvedValue(refreshedSnapshot);
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(initialSnapshot), refresh });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const list = within(await screen.findByRole("table", { name: "Conversation list" }));
    await user.click(list.getByText("Archive the first pass"));
    const filters = screen.getByRole("region", { name: "Conversation filters" });
    await user.selectOptions(within(filters).getByLabelText("Filter by archived"), "archived");

    await user.click(screen.getByRole("button", { name: "Refresh source" }));

    await waitFor(() => expect(refresh).toHaveBeenCalledWith({ granularity: "day", timezone: "UTC" }));
    expect(within(screen.getByRole("region", { name: "Conversation filters" })).getByLabelText("Filter by archived")).toHaveValue("archived");
    const refreshedList = within(screen.getByRole("table", { name: "Conversation list" }));
    expect(refreshedList.getByText("new-archived-id")).toBeInTheDocument();
    expect(refreshedList.queryByText("active-id")).not.toBeInTheDocument();
    expect(refreshedList.getByText("archived-id").closest("tr")).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("region", { name: "Relationship editor" })).toHaveTextContent("No artificial relationships yet.");
  });

  it("requires a decision for a dirty detail draft and keeps cancel non-destructive", async () => {
    const user = userEvent.setup();
    const refresh = vi.fn().mockResolvedValue(snapshot);
    const api = apiDouble({ refresh });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));
    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    const titleInput = within(detail).getByLabelText("Custom title");
    await user.type(titleInput, "Draft title");

    await user.click(screen.getByRole("button", { name: "Refresh source" }));
    const dialog = await screen.findByRole("dialog", { name: "Save your Graph changes first?" });
    expect(refresh).not.toHaveBeenCalled();
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));

    expect(screen.queryByRole("dialog", { name: "Save your Graph changes first?" })).not.toBeInTheDocument();
    expect(titleInput).toHaveValue("Draft title");
    expect(refresh).not.toHaveBeenCalled();
  });

  it("discards a dirty detail draft before refreshing without changing the committed overlay", async () => {
    const user = userEvent.setup();
    const refresh = vi.fn().mockResolvedValue(snapshot);
    const updateNode = vi.fn();
    const api = apiDouble({ refresh, updateNode });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));
    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    await user.type(within(detail).getByLabelText("Custom title"), "Discard me");

    await user.click(screen.getByRole("button", { name: "Refresh source" }));
    await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Discard changes & refresh" }));

    await waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    expect(updateNode).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(within(await screen.findByRole("region", { name: "Conversation detail" })).getByLabelText("Custom title")).toHaveValue("");
  });

  it("keeps the dirty draft and decision dialog open when saving before refresh fails", async () => {
    const user = userEvent.setup();
    const updateNode = vi.fn().mockRejectedValue(
      new ApiError(412, {
        code: "graph_conflict",
        message: "The Graph changed elsewhere; reload before saving.",
        details: null,
        retryable: true,
      }),
    );
    const refresh = vi.fn().mockResolvedValue(snapshot);
    const api = apiDouble({ updateNode, refresh });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));
    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    const titleInput = within(detail).getByLabelText("Custom title");
    await user.type(titleInput, "Keep this draft");

    await user.click(screen.getByRole("button", { name: "Refresh source" }));
    const dialog = await screen.findByRole("dialog", { name: "Save your Graph changes first?" });
    await user.click(within(dialog).getByRole("button", { name: "Save changes & refresh" }));

    await waitFor(() => expect(within(dialog).getByRole("alert")).toHaveTextContent("Your draft is still dirty."));
    expect(refresh).not.toHaveBeenCalled();
    expect(titleInput).toHaveValue("Keep this draft");
    expect(screen.getByRole("button", { name: "Refresh source" })).toHaveTextContent("Refresh source");
  });

  it("saves the dirty draft before refreshing and clears the dirty state", async () => {
    const user = userEvent.setup();
    const savedSnapshot: DashboardSnapshot = {
      ...snapshot,
      graph: { ...snapshot.graph, etag: "saved-etag" },
      conversations: snapshot.conversations.map((conversation) =>
        conversation.id === "active-id"
          ? {
              ...conversation,
              displayTitle: "Saved title",
              overlay: { ...conversation.overlay, title: "Saved title" },
            }
          : conversation,
      ),
    };
    const updateNode = vi.fn().mockResolvedValue(savedSnapshot);
    const refresh = vi.fn().mockResolvedValue(savedSnapshot);
    const api = apiDouble({ updateNode, refresh });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));
    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    await user.type(within(detail).getByLabelText("Custom title"), "Saved title");

    await user.click(screen.getByRole("button", { name: "Refresh source" }));
    await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Save changes & refresh" }));

    await waitFor(() => expect(updateNode).toHaveBeenCalledWith(
      "active-id",
      {
        title: "Saved title",
        tags: [],
        status: "none",
        note: null,
        hidden: false,
      },
      "absent",
    ));
    await waitFor(() => expect(refresh).toHaveBeenCalledWith());
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.getByText("Source sync ready")).toBeInTheDocument();
  });

  it("protects an unsaved relationship draft during refresh", async () => {
    const user = userEvent.setup();
    const refresh = vi.fn().mockResolvedValue(snapshot);
    const createEdge = vi.fn();
    const api = apiDouble({ refresh, createEdge });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const editor = await screen.findByRole("region", { name: "Relationship editor" });
    const sourceInput = within(editor).getByLabelText("Source");
    await user.type(sourceInput, "active-id");

    await user.click(screen.getByRole("button", { name: "Refresh source" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("One unsaved Graph draft is open");
    await user.click(within(dialog).getByRole("button", { name: "Discard changes & refresh" }));

    await waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    expect(createEdge).not.toHaveBeenCalled();
    expect(within(await screen.findByRole("region", { name: "Relationship editor" })).getByLabelText("Source")).toHaveValue("");
  });
});

describe("Dashboard graph", () => {
  it("edits Conversation metadata through the detail panel and refreshes the snapshot", async () => {
    const user = userEvent.setup();
    const updatedSnapshot: DashboardSnapshot = {
      ...snapshot,
      graph: {
        ...snapshot.graph,
        etag: "saved-etag",
        nodes: snapshot.graph.nodes.map((node) =>
          node.id === "active-id"
            ? { ...node, displayTitle: "Local title", hidden: true }
            : node,
        ),
      },
      conversations: snapshot.conversations.map((conversation) =>
        conversation.id === "active-id"
          ? {
              ...conversation,
              displayTitle: "Local title",
              overlay: {
                ...conversation.overlay,
                title: "Local title",
                tags: ["alpha", "beta"],
                status: "done",
                note: "Need this context",
                hidden: true,
              },
            }
          : conversation,
      ),
    };
    const updateNode = vi.fn().mockResolvedValue(updatedSnapshot);
    const api = apiDouble({ updateNode });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));

    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    await user.clear(within(detail).getByLabelText("Custom title"));
    await user.type(within(detail).getByLabelText("Custom title"), "Local title");
    await user.clear(within(detail).getByLabelText("Tags"));
    await user.type(within(detail).getByLabelText("Tags"), "alpha, beta");
    await user.selectOptions(within(detail).getByLabelText("User status"), "done");
    await user.type(within(detail).getByLabelText("Note"), "Need this context");
    await user.click(within(detail).getByLabelText("Hidden from Graph"));
    await user.click(within(detail).getByRole("button", { name: "Save changes" }));

    expect(updateNode).toHaveBeenCalledWith(
      "active-id",
      {
        title: "Local title",
        tags: ["alpha", "beta"],
        status: "done",
        note: "Need this context",
        hidden: true,
      },
      "absent",
    );
    await waitFor(() => expect(screen.getByText("Changes saved")).toBeInTheDocument());
    expect(within(detail).getByLabelText("Custom title")).toHaveValue("Local title");
    expect(within(detail).getByLabelText("Tags")).toHaveValue("alpha, beta");
    expect(within(screen.getByRole("table", { name: "Conversation list" })).getByText("active-id")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Conversation graph" })).queryByRole("button", { name: /Map the local runtime/ })).not.toBeInTheDocument();
  });

  it("keeps unsaved form values and explains a failed metadata update", async () => {
    const user = userEvent.setup();
    const updateNode = vi.fn().mockRejectedValue(
      new ApiError(412, {
        code: "graph_conflict",
        message: "The Graph changed elsewhere; reload before saving.",
        details: null,
        retryable: true,
      }),
    );
    const api = apiDouble({ updateNode });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));

    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    const titleInput = within(detail).getByLabelText("Custom title");
    const noteInput = within(detail).getByLabelText("Note");
    await user.type(titleInput, "Draft title");
    await user.type(noteInput, "Draft note");
    await user.click(within(detail).getByRole("button", { name: "Save changes" }));

    const alert = await within(detail).findByRole("alert");
    expect(alert).toHaveTextContent("Could not save changes");
    expect(alert).toHaveTextContent("The Graph changed elsewhere; reload before saving.");
    expect(titleInput).toHaveValue("Draft title");
    expect(noteInput).toHaveValue("Draft note");
  });

  it("offers reload, save-copy, and explicit overwrite actions after a Graph conflict", async () => {
    const user = userEvent.setup();
    const updateNode = vi
      .fn()
      .mockRejectedValueOnce(
        new ApiError(412, {
          code: "graph_conflict",
          message: "The Graph changed elsewhere; reload before saving.",
          details: { expectedEtag: "draft-etag", currentEtag: "external-etag" },
          retryable: true,
        }),
      )
      .mockResolvedValue(snapshot);
    const refresh = vi.fn().mockResolvedValue(snapshot);
    const saveCopy = vi.fn().mockResolvedValue({ copyPath: "/projects/codexflow/.codex/graph.yaml.copy.fixture" });
    const api = apiDouble({ updateNode, refresh, saveCopy });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));

    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    await user.type(within(detail).getByLabelText("Custom title"), "Draft title");
    await user.click(within(detail).getByRole("button", { name: "Save changes" }));

    const conflict = await screen.findByRole("alert", { name: "Graph conflict" });
    expect(conflict).toHaveTextContent("The Graph changed elsewhere; reload before saving.");
    expect(conflict).toHaveTextContent("draft-etag");
    expect(conflict).toHaveTextContent("external-etag");
    expect(within(detail).getByLabelText("Custom title")).toHaveValue("Draft title");
    expect(within(conflict).getByRole("button", { name: "Reload Graph" })).toBeInTheDocument();
    expect(within(conflict).getByRole("button", { name: "Save a copy" })).toBeInTheDocument();
    expect(within(conflict).getByRole("button", { name: "Overwrite explicitly" })).toBeInTheDocument();

    await user.click(within(conflict).getByRole("button", { name: "Save a copy" }));
    await waitFor(() => expect(saveCopy).toHaveBeenCalled());
    expect(saveCopy.mock.calls[0][0].nodes["active-id"].title).toBe("Draft title");
    expect(conflict).toHaveTextContent("graph.yaml.copy.fixture");

    await user.click(within(conflict).getByRole("button", { name: "Overwrite explicitly" }));
    await waitFor(() => expect(updateNode).toHaveBeenCalledTimes(2));
    expect(updateNode.mock.calls[1][2]).toBe("*");
    expect(updateNode.mock.calls[1][3]).toBe(true);
  });

  it("opens a future Graph version read-only and disables save controls", async () => {
    const user = userEvent.setup();
    const futureSnapshot: DashboardSnapshot = {
      ...snapshot,
      project: { ...project, graphFileStatus: "future" },
      graph: { ...snapshot.graph, fileStatus: "future" },
    };
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(futureSnapshot) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const notice = await screen.findByRole("status", { name: "Graph overlay status" });
    expect(notice).toHaveTextContent("read-only");

    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));
    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    expect(within(detail).getByRole("button", { name: "Save changes" })).toBeDisabled();
  });

  it("keeps a structured disk-full error visible in the detail editor", async () => {
    const user = userEvent.setup();
    const updateNode = vi.fn().mockRejectedValue(
      new ApiError(500, {
        code: "graph_write_error",
        message: "Graph overlay could not be saved: disk full.",
        details: { errno: 28, errnoName: "ENOSPC" },
        retryable: true,
      }),
    );
    const api = apiDouble({ updateNode });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));
    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    await user.type(within(detail).getByLabelText("Custom title"), "Draft title");
    await user.click(within(detail).getByRole("button", { name: "Save changes" }));

    const alert = await within(detail).findByRole("alert");
    expect(alert).toHaveTextContent("graph_write_error");
    expect(alert).toHaveTextContent("disk full");
    expect(within(detail).getByLabelText("Custom title")).toHaveValue("Draft title");
  });

  it("offers an explicit migration action for a legacy Graph version", async () => {
    const user = userEvent.setup();
    const legacySnapshot: DashboardSnapshot = {
      ...snapshot,
      project: { ...project, graphFileStatus: "legacy" },
      graph: { ...snapshot.graph, fileStatus: "legacy" },
    };
    const migratedSnapshot = {
      ...snapshot,
      graph: { ...snapshot.graph, etag: "migrated-etag", fileStatus: "ready" },
      project: { ...project, graphFileStatus: "ready" },
      backupPath: "/projects/codexflow/.codex/graph.yaml.bak.fixture",
    };
    const migrateGraph = vi.fn().mockResolvedValue(migratedSnapshot);
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(legacySnapshot), migrateGraph });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const notice = await screen.findByRole("status", { name: "Graph overlay status" });
    await user.click(within(notice).getByRole("button", { name: "Migrate with backup" }));

    await waitFor(() => expect(migrateGraph).toHaveBeenCalledWith("absent"));
    expect(await screen.findByText(/graph.yaml.bak.fixture/)).toBeInTheDocument();
  });

  it("reloads the externally saved Graph and replaces the local draft", async () => {
    const user = userEvent.setup();
    const externalSnapshot: DashboardSnapshot = {
      ...snapshot,
      graph: { ...snapshot.graph, etag: "external-etag" },
      conversations: snapshot.conversations.map((conversation) =>
        conversation.id === "active-id"
          ? {
              ...conversation,
              displayTitle: "External title",
              overlay: { ...conversation.overlay, title: "External title" },
            }
          : conversation,
      ),
    };
    const updateNode = vi.fn().mockRejectedValue(
      new ApiError(412, {
        code: "graph_conflict",
        message: "The Graph changed elsewhere; reload before saving.",
        details: null,
        retryable: true,
      }),
    );
    const refresh = vi.fn().mockResolvedValue(externalSnapshot);
    const api = apiDouble({ updateNode, refresh });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    await user.click(within(await screen.findByRole("table", { name: "Conversation list" })).getByText("Map the local runtime"));
    const detail = await screen.findByRole("region", { name: "Conversation detail" });
    await user.type(within(detail).getByLabelText("Custom title"), "Local draft");
    await user.click(within(detail).getByRole("button", { name: "Save changes" }));

    const conflict = await screen.findByRole("alert", { name: "Graph conflict" });
    await user.click(within(conflict).getByRole("button", { name: "Reload Graph" }));

    await waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("alert", { name: "Graph conflict" })).not.toBeInTheDocument();
    expect(within(await screen.findByRole("region", { name: "Conversation detail" })).getByLabelText("Custom title")).toHaveValue("External title");
  });

  it("routes a migration conflict through the same explicit resolution actions", async () => {
    const user = userEvent.setup();
    const legacySnapshot: DashboardSnapshot = {
      ...snapshot,
      project: { ...project, graphFileStatus: "legacy" },
      graph: { ...snapshot.graph, fileStatus: "legacy" },
    };
    const migratedSnapshot = {
      ...snapshot,
      project: { ...project, graphFileStatus: "ready" },
      graph: { ...snapshot.graph, etag: "migrated-etag", fileStatus: "ready" },
      backupPath: "/projects/codexflow/.codex/graph.yaml.bak.fixture",
    };
    const migrateGraph = vi
      .fn()
      .mockRejectedValueOnce(
        new ApiError(412, {
          code: "graph_conflict",
          message: "The legacy Graph changed elsewhere.",
          details: { expectedEtag: "legacy-etag", currentEtag: "external-etag" },
          retryable: true,
        }),
      )
      .mockResolvedValue(migratedSnapshot);
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(legacySnapshot), migrateGraph });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const notice = await screen.findByRole("status", { name: "Graph overlay status" });
    await user.click(within(notice).getByRole("button", { name: "Migrate with backup" }));

    const conflict = await screen.findByRole("alert", { name: "Graph conflict" });
    expect(conflict).toHaveTextContent("legacy-etag");
    expect(conflict).toHaveTextContent("external-etag");
    await user.click(within(conflict).getByRole("button", { name: "Overwrite explicitly" }));

    await waitFor(() => expect(migrateGraph).toHaveBeenCalledWith("*", true));
    expect(await screen.findByText(/graph.yaml.bak.fixture/)).toBeInTheDocument();
  });

  it("saves a node layout only after the node drag ends", async () => {
    const user = userEvent.setup();
    const updateNode = vi.fn().mockResolvedValue(snapshot);
    const api = apiDouble({ updateNode });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const graph = await screen.findByRole("region", { name: "Conversation graph" });
    const canvas = within(graph).getByRole("application", { name: "Graph canvas" });
    const node = within(graph).getByRole("button", { name: /Map the local runtime/ });
    fireEvent.mouseDown(node, { clientX: 50, clientY: 50, button: 0 });
    fireEvent.mouseMove(canvas, { clientX: 90, clientY: 70 });
    expect(updateNode).not.toHaveBeenCalled();
    fireEvent.mouseUp(canvas, { clientX: 90, clientY: 70 });

    await waitFor(() => {
      expect(updateNode).toHaveBeenCalledWith(
        "active-id",
        { layout: { x: 72, y: 54 } },
        "absent",
      );
    });
  });

  it("keeps the attempted node position when layout persistence fails", async () => {
    const user = userEvent.setup();
    const updateNode = vi.fn().mockRejectedValue(new Error("Graph is read-only."));
    const api = apiDouble({ updateNode });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const graph = await screen.findByRole("region", { name: "Conversation graph" });
    const canvas = within(graph).getByRole("application", { name: "Graph canvas" });
    const node = within(graph).getByRole("button", { name: /Map the local runtime/ });
    fireEvent.mouseDown(node, { clientX: 50, clientY: 50, button: 0 });
    fireEvent.mouseMove(canvas, { clientX: 90, clientY: 70 });
    fireEvent.mouseUp(canvas, { clientX: 90, clientY: 70 });

    expect(await within(graph).findByRole("alert")).toHaveTextContent("Graph is read-only.");
    expect(node).toHaveStyle({ left: "72px", top: "54px" });
  });

  it("renders source and missing nodes, selects a node, and supports zoom and pan", async () => {
    const user = userEvent.setup();
    const graphSnapshot = {
      ...snapshot,
      conversations: [
        ...snapshot.conversations,
        {
          ...snapshot.conversations[0],
          id: "missing-id",
          displayTitle: "Orphaned work",
          codex: {
            ...snapshot.conversations[0].codex,
            title: null,
            preview: "",
            createdAt: null,
            updatedAt: null,
            cwd: "",
          },
          derived: {
            ...snapshot.conversations[0].derived,
            missing: true,
            validObservationRange: false,
          },
        },
      ],
      graph: {
        ...snapshot.graph,
        nodes: [
          {
            id: "active-id",
            displayTitle: "Map the local runtime",
            missing: false,
            hidden: false,
            layout: null,
          },
          {
            id: "missing-id",
            displayTitle: "Orphaned work",
            missing: true,
            hidden: false,
            layout: { x: 320, y: 180 },
          },
        ],
        edges: [
          { id: "edge-1", source: "active-id", target: "missing-id", type: "continues" },
          { id: "edge-2", source: "active-id", target: "missing-id", type: "related_to" },
        ],
      },
    } as DashboardSnapshot;
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(graphSnapshot) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const graph = await screen.findByRole("region", { name: "Conversation graph" });
    const canvas = within(graph).getByRole("application", { name: "Graph canvas" });
    expect(within(graph).getByRole("button", { name: /Map the local runtime/ })).toBeInTheDocument();
    expect(within(graph).getByRole("button", { name: /Orphaned work/ })).toHaveTextContent("Missing source");
    expect(canvas).toHaveAttribute("data-zoom", "1");

    await user.click(within(graph).getByRole("button", { name: "Zoom in" }));
    expect(canvas).toHaveAttribute("data-zoom", "1.1");
    fireEvent.mouseDown(canvas, { clientX: 40, clientY: 40, button: 0 });
    fireEvent.mouseMove(canvas, { clientX: 90, clientY: 70 });
    fireEvent.mouseUp(canvas, { clientX: 90, clientY: 70 });
    expect(canvas).toHaveAttribute("data-pan-x", "50");
    expect(canvas).toHaveAttribute("data-pan-y", "30");
    const edgeLines = graph.querySelectorAll("line");
    expect(edgeLines[0]).toHaveAttribute("marker-end", "url(#graph-arrow)");
    expect(edgeLines[1]).not.toHaveAttribute("marker-end");

    await user.click(within(graph).getByRole("button", { name: /Orphaned work/ }));
    await waitFor(() => {
      expect(screen.getByRole("region", { name: "Conversation detail" })).toHaveTextContent("missing-id");
    });
  });

  it("creates a custom relationship and renders its source-to-target meaning", async () => {
    const user = userEvent.setup();
    const createdSnapshot: DashboardSnapshot = {
      ...snapshot,
      graph: {
        ...snapshot.graph,
        etag: "edge-etag",
        edges: [
          {
            id: "edge-custom",
            source: "active-id",
            target: "archived-id",
            type: "informs",
            label: "context handoff",
          },
        ],
      },
    };
    const createEdge = vi.fn().mockResolvedValue(createdSnapshot);
    const api = apiDouble({ createEdge });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const editor = await screen.findByRole("region", { name: "Relationship editor" });
    await user.type(within(editor).getByLabelText("Source"), "active-id");
    await user.type(within(editor).getByLabelText("Target"), "archived-id");
    await user.selectOptions(within(editor).getByLabelText("Relationship type"), "__custom__");
    await user.type(within(editor).getByLabelText("Custom type"), "informs");
    await user.type(within(editor).getByLabelText(/Label/), "context handoff");
    await user.click(within(editor).getByRole("button", { name: "Add relationship" }));

    expect(createEdge).toHaveBeenCalledWith(
      {
        source: "active-id",
        target: "archived-id",
        type: "informs",
        label: "context handoff",
      },
      "absent",
    );
    expect(await within(editor).findByText("Relationship saved")).toBeInTheDocument();
    const relationship = within(editor).getByRole("listitem", {
      name: "Relationship active-id informs archived-id",
    });
    expect(relationship).toHaveTextContent("Map the local runtime");
    expect(relationship).toHaveTextContent("Archive the first pass");
    expect(relationship).toHaveTextContent("→");
    expect(relationship).toHaveTextContent("informs");
    expect(relationship).toHaveTextContent("context handoff");
  });

  it("edits a dangling relationship without changing its id, then deletes only the edge", async () => {
    const user = userEvent.setup();
    const edge = {
      id: "edge-1",
      source: "active-id",
      target: "missing-id",
      type: "continues",
      label: "history",
    };
    const edgeSnapshot: DashboardSnapshot = {
      ...snapshot,
      graph: {
        ...snapshot.graph,
        etag: "edge-etag",
        nodes: [
          ...snapshot.graph.nodes,
          { id: "missing-id", displayTitle: "Orphaned work", missing: true, hidden: false, layout: null },
        ],
        edges: [edge],
      },
    };
    const updatedSnapshot: DashboardSnapshot = {
      ...edgeSnapshot,
      graph: {
        ...edgeSnapshot.graph,
        etag: "updated-edge-etag",
        edges: [{ ...edge, type: "fixes", label: "repaired history" }],
      },
    };
    const deletedSnapshot: DashboardSnapshot = {
      ...updatedSnapshot,
      graph: { ...updatedSnapshot.graph, etag: "deleted-edge-etag", edges: [] },
    };
    const updateEdge = vi.fn().mockResolvedValue(updatedSnapshot);
    const deleteEdge = vi.fn().mockResolvedValue(deletedSnapshot);
    const api = apiDouble({
      snapshot: vi.fn().mockResolvedValue(edgeSnapshot),
      updateEdge,
      deleteEdge,
    });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const editor = await screen.findByRole("region", { name: "Relationship editor" });
    const relationship = within(editor).getByRole("listitem", {
      name: "Relationship active-id continues missing-id",
    });
    await user.click(within(relationship).getByRole("button", { name: "Edit relationship edge-1" }));
    await user.selectOptions(within(editor).getByLabelText("Relationship type"), "fixes");
    await user.clear(within(editor).getByLabelText(/Label/));
    await user.type(within(editor).getByLabelText(/Label/), "repaired history");
    await user.click(within(editor).getByRole("button", { name: "Save relationship" }));

    expect(updateEdge).toHaveBeenCalledWith(
      "edge-1",
      {
        source: "active-id",
        target: "missing-id",
        type: "fixes",
        label: "repaired history",
      },
      "edge-etag",
    );
    const updatedRelationship = await within(editor).findByRole("listitem", {
      name: "Relationship active-id fixes missing-id",
    });
    expect(updatedRelationship).toHaveTextContent("edge-1");
    expect(updatedRelationship).toHaveTextContent("Orphaned work");

    await user.click(within(updatedRelationship).getByRole("button", { name: "Delete relationship edge-1" }));
    expect(deleteEdge).toHaveBeenCalledWith("edge-1", "updated-edge-etag");
    expect(await within(editor).findByText("No artificial relationships yet.")).toBeInTheDocument();
    expect(within(screen.getByRole("table", { name: "Conversation list" })).getByText("active-id")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Conversation graph" })).getByRole("button", { name: /Orphaned work/ })).toBeInTheDocument();
  });

  it("keeps relationship form values and explains a duplicate relationship error", async () => {
    const user = userEvent.setup();
    const createEdge = vi.fn().mockRejectedValue(
      new ApiError(409, {
        code: "duplicate_edge",
        message: "This relationship already exists.",
        details: null,
        retryable: false,
      }),
    );
    const api = apiDouble({ createEdge });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const editor = await screen.findByRole("region", { name: "Relationship editor" });
    const sourceInput = within(editor).getByLabelText("Source");
    const targetInput = within(editor).getByLabelText("Target");
    await user.type(sourceInput, "active-id");
    await user.type(targetInput, "archived-id");
    await user.click(within(editor).getByRole("button", { name: "Add relationship" }));

    const alert = await within(editor).findByRole("alert");
    expect(alert).toHaveTextContent("duplicate_edge");
    expect(alert).toHaveTextContent("This relationship already exists.");
    expect(sourceInput).toHaveValue("active-id");
    expect(targetInput).toHaveValue("archived-id");
  });
});

describe("Dashboard timeline", () => {
  it("shows buckets, point markers, invalid-time warnings, and selects a conversation", async () => {
    const user = userEvent.setup();
    const timelineSnapshot: DashboardSnapshot = {
      ...snapshot,
      conversations: [
        ...snapshot.conversations,
        {
          ...snapshot.conversations[0],
          id: "invalid-time-id",
          displayTitle: "Needs time review",
          codex: { ...snapshot.conversations[0].codex, createdAt: null },
          derived: { ...snapshot.conversations[0].derived, validObservationRange: false },
        },
      ],
      timeline: {
        granularity: "day",
        timezone: "UTC",
        ranges: [
          {
            conversationId: "active-id",
            start: "2024-01-01T00:00:00Z",
            end: "2024-01-01T01:00:00Z",
            valid: true,
            isPoint: false,
            error: null,
          },
          {
            conversationId: "archived-id",
            start: "2024-01-02T00:00:00Z",
            end: "2024-01-02T00:00:00Z",
            valid: true,
            isPoint: true,
            error: null,
          },
          {
            conversationId: "invalid-time-id",
            start: null,
            end: "2024-01-02T01:00:00Z",
            valid: false,
            isPoint: false,
            error: "createdAt invalid",
          },
        ],
        buckets: [
          {
            start: "2024-01-01T00:00:00Z",
            end: "2024-01-02T00:00:00Z",
            label: "2024-01-01",
            overlapCount: 1,
          },
          {
            start: "2024-01-02T00:00:00Z",
            end: "2024-01-03T00:00:00Z",
            label: "2024-01-02",
            overlapCount: 1,
          },
        ],
        warnings: [
          {
            conversationId: "invalid-time-id",
            code: "invalid_observation_range",
            message: "createdAt invalid",
          },
        ],
      },
    };
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(timelineSnapshot) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const timeline = await screen.findByRole("region", { name: "Conversation timeline" });
    expect(timeline).toHaveTextContent("Timeline");
    expect(timeline).toHaveTextContent("2024-01-01");
    expect(timeline).toHaveTextContent("2024-01-02");
    expect(timeline).toHaveTextContent("Overlap count");
    expect(timeline).toHaveTextContent("Invalid observation range");
    expect(timeline).toHaveTextContent("Point");

    const pointControls = within(timeline).getAllByRole("button", {
      name: /Archive the first pass.*point/i,
    });
    await user.click(pointControls[pointControls.length - 1]);
    await waitFor(() => {
      expect(screen.getByRole("region", { name: "Conversation detail" })).toHaveTextContent(
        "archived-id",
      );
    });
  });

  it("requests a new snapshot when the bucket scale or time zone changes", async () => {
    const user = userEvent.setup();
    const timelineSnapshot: DashboardSnapshot = {
      ...snapshot,
      timeline: {
        granularity: "day",
        timezone: "UTC",
        ranges: [],
        buckets: [],
        warnings: [],
      },
    };
    const snapshotRequest = vi.fn().mockResolvedValue(timelineSnapshot);
    const api = apiDouble({ snapshot: snapshotRequest });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const timeline = await screen.findByRole("region", { name: "Conversation timeline" });

    await user.click(within(timeline).getByRole("button", { name: "Week" }));
    await waitFor(() => {
      expect(snapshotRequest).toHaveBeenLastCalledWith({
        granularity: "week",
        timezone: "UTC",
      });
    });

    const timezone = within(timeline).getByLabelText("Time zone");
    await user.clear(timezone);
    await user.type(timezone, "Asia/Shanghai");
    await user.click(within(timeline).getByRole("button", { name: "Apply" }));
    await waitFor(() => {
      expect(snapshotRequest).toHaveBeenLastCalledWith({
        granularity: "week",
        timezone: "Asia/Shanghai",
      });
    });
  });
});

describe("Dashboard selection and filters", () => {
  it("keeps one selected Conversation ID highlighted in List, Graph, and Timeline", async () => {
    const user = userEvent.setup();
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(snapshotWithTimeline()) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));

    const list = within(await screen.findByRole("table", { name: "Conversation list" }));
    const graph = within(screen.getByRole("region", { name: "Conversation graph" }));
    const timeline = within(screen.getByRole("region", { name: "Conversation timeline" }));
    const activeRow = list.getByText("active-id").closest("tr");
    const activeGraphNode = graph.getByRole("button", { name: /Map the local runtime/ });
    const activeTimelineRow = timeline.getAllByRole("button", { name: /Map the local runtime/ })[0];

    await user.click(list.getByText("Map the local runtime"));
    expect(activeRow).toHaveAttribute("aria-selected", "true");
    expect(activeGraphNode).toHaveAttribute("aria-pressed", "true");
    expect(activeTimelineRow).toHaveAttribute("aria-pressed", "true");

    const archivedGraphNode = graph.getByRole("button", { name: /Archive the first pass/ });
    await user.click(archivedGraphNode);
    expect(list.getByText("archived-id").closest("tr")).toHaveAttribute("aria-selected", "true");
    expect(activeRow).toHaveAttribute("aria-selected", "false");
    expect(archivedGraphNode).toHaveAttribute("aria-pressed", "true");
    expect(activeGraphNode).toHaveAttribute("aria-pressed", "false");

    await user.click(activeTimelineRow);
    expect(activeRow).toHaveAttribute("aria-selected", "true");
    expect(activeGraphNode).toHaveAttribute("aria-pressed", "true");
    expect(activeTimelineRow).toHaveAttribute("aria-pressed", "true");
    expect(archivedGraphNode).toHaveAttribute("aria-pressed", "false");
  });

  it("applies one search projection to List, Graph, and Timeline without changing IDs", async () => {
    const user = userEvent.setup();
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(snapshotWithTimeline()) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const filters = await screen.findByRole("region", { name: "Conversation filters" });
    await user.type(within(filters).getByRole("searchbox", { name: "Search conversations" }), "archive");

    const list = within(screen.getByRole("table", { name: "Conversation list" }));
    const graph = within(screen.getByRole("region", { name: "Conversation graph" }));
    const timeline = within(screen.getByRole("region", { name: "Conversation timeline" }));
    expect(list.getByText("archived-id")).toBeInTheDocument();
    expect(list.queryByText("active-id")).not.toBeInTheDocument();
    expect(graph.getByRole("button", { name: /Archive the first pass/ })).toBeInTheDocument();
    expect(graph.queryByRole("button", { name: /Map the local runtime/ })).not.toBeInTheDocument();
    expect(timeline.getAllByRole("button", { name: /Archive the first pass/ }).length).toBeGreaterThan(0);
    expect(timeline.queryByRole("button", { name: /Map the local runtime/ })).not.toBeInTheDocument();
    expect(timeline.getByLabelText(/1 overlapping conversations/)).toBeInTheDocument();
  });

  it("filters tag, User status, archived, missing, unlinked, and hidden states", async () => {
    const user = userEvent.setup();
    const active = {
      ...snapshot.conversations[0],
      overlay: { ...snapshot.conversations[0].overlay, tags: ["focus"], status: "done" as const, hidden: true },
      derived: { ...snapshot.conversations[0].derived, unlinked: false },
    };
    const archived = {
      ...snapshot.conversations[1],
      overlay: { ...snapshot.conversations[1].overlay, tags: ["archive"], status: "active" as const },
      codex: { ...snapshot.conversations[1].codex, archived: true },
    };
    const missing = {
      ...active,
      id: "missing-id",
      displayTitle: "Orphaned work",
      codex: { ...active.codex, title: null, preview: "", createdAt: null, updatedAt: null, cwd: "" },
      overlay: { ...active.overlay, tags: ["history"], hidden: false },
      derived: { ...active.derived, missing: true, unlinked: true, validObservationRange: false },
    };
    const filterSnapshot = snapshotWithTimeline({
      ...snapshot,
      graph: {
        ...snapshot.graph,
        nodes: [
          { id: active.id, displayTitle: active.displayTitle, missing: false, hidden: true, layout: null },
          { id: archived.id, displayTitle: archived.displayTitle, missing: false, hidden: false, layout: null },
          { id: missing.id, displayTitle: missing.displayTitle, missing: true, hidden: false, layout: null },
        ],
      },
      conversations: [active, archived, missing],
    });
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(filterSnapshot) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const filters = await screen.findByRole("region", { name: "Conversation filters" });
    const list = () => within(screen.getByRole("table", { name: "Conversation list" }));

    await user.selectOptions(within(filters).getByLabelText("Filter by tag"), "focus");
    expect(list().getByText("active-id")).toBeInTheDocument();
    expect(list().queryByText("archived-id")).not.toBeInTheDocument();
    await user.click(within(filters).getByRole("button", { name: "Clear filters" }));

    await user.selectOptions(within(filters).getByLabelText("Filter by User status"), "done");
    expect(list().getByText("active-id")).toBeInTheDocument();
    await user.click(within(filters).getByRole("button", { name: "Clear filters" }));

    await user.selectOptions(within(filters).getByLabelText("Filter by archived"), "archived");
    expect(list().getByText("archived-id")).toBeInTheDocument();
    expect(list().queryByText("active-id")).not.toBeInTheDocument();
    await user.click(within(filters).getByRole("button", { name: "Clear filters" }));

    await user.selectOptions(within(filters).getByLabelText("Filter by missing"), "missing");
    expect(list().getByText("missing-id")).toBeInTheDocument();
    await user.click(within(filters).getByRole("button", { name: "Clear filters" }));

    await user.selectOptions(within(filters).getByLabelText("Filter by unlinked"), "unlinked");
    expect(list().getByText("archived-id")).toBeInTheDocument();
    expect(list().queryByText("missing-id")).not.toBeInTheDocument();
    await user.click(within(filters).getByRole("button", { name: "Clear filters" }));

    await user.selectOptions(within(filters).getByLabelText("Filter by hidden"), "hidden");
    expect(list().getByText("active-id")).toBeInTheDocument();
    expect(list().queryByText("archived-id")).not.toBeInTheDocument();

    await user.click(within(filters).getByRole("button", { name: "Clear filters" }));
    await user.selectOptions(within(filters).getByLabelText("Sort conversations"), "updatedAt");
    const sortedRows = list().getAllByRole("row").slice(1);
    expect(sortedRows[0]).toHaveAttribute("data-conversation-id", "archived-id");
    expect(sortedRows[1]).toHaveAttribute("data-conversation-id", "active-id");
  });

  it("projects Timeline overlap counts and warnings with the same filters", async () => {
    const user = userEvent.setup();
    const baseTimeline = snapshotWithTimeline();
    const filteredTimelineSnapshot: DashboardSnapshot = {
      ...baseTimeline,
      timeline: {
        ...baseTimeline.timeline!,
        warnings: [
          {
            conversationId: "active-id",
            code: "invalid_observation_range",
            message: "createdAt invalid",
          },
        ],
      },
    };
    const api = apiDouble({ snapshot: vi.fn().mockResolvedValue(filteredTimelineSnapshot) });
    render(<App api={api} />);

    await screen.findByText("Local runtime ready");
    await user.type(screen.getByLabelText("Project root"), "/projects/codexflow");
    await user.click(screen.getByRole("button", { name: "Load Project" }));
    const timeline = await screen.findByRole("region", { name: "Conversation timeline" });
    expect(within(timeline).getByRole("alert", { name: "Timeline warnings" })).toBeInTheDocument();
    expect(within(timeline).getByLabelText(/2 overlapping conversations/)).toBeInTheDocument();

    const filters = screen.getByRole("region", { name: "Conversation filters" });
    await user.type(within(filters).getByRole("searchbox", { name: "Search conversations" }), "archive");

    expect(within(timeline).queryByRole("alert", { name: "Timeline warnings" })).not.toBeInTheDocument();
    expect(within(timeline).getByLabelText(/1 overlapping conversations/)).toBeInTheDocument();
  });
});
