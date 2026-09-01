import { render, screen, waitFor } from "@testing-library/react";
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
  graph: { etag: "absent", fileStatus: "absent", edges: [] },
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
  };
  return {
    health: overrides.health ?? defaults.health,
    selectProject: overrides.selectProject ?? defaults.selectProject,
    snapshot: overrides.snapshot ?? defaults.snapshot,
    refresh: overrides.refresh ?? defaults.refresh,
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
    await screen.findByText("Map the local runtime");

    expect(screen.getByText("active-id")).toBeInTheDocument();
    expect(screen.getByText("archived-id")).toBeInTheDocument();
    expect(screen.getByText("Archived")).toBeInTheDocument();
    expect(screen.getByText("vscode")).toBeInTheDocument();
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
    await screen.findByText("Map the local runtime");
    await user.click(screen.getByText("Map the local runtime"));

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
    expect(screen.getByText("Map the local runtime")).toBeInTheDocument();
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
});
