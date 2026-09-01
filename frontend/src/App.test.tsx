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
