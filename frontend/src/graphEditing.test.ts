import { waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ApiError } from "./api";
import type { DashboardApi } from "./api";
import { GraphEditing } from "./graphEditing";
import { apiDouble, deferred, health, nodeSnapshot, project, snapshot } from "./test/dashboard";
import type { DashboardSnapshot, GraphCopyResult } from "./types";

const conflict = () => new ApiError(412, {
  code: "graph_conflict", message: "Changed elsewhere", retryable: true,
  details: { expectedEtag: "absent", currentEtag: "external" },
});

async function load(overrides: Partial<DashboardApi> = {}) {
  const api = apiDouble(overrides);
  const editing = new GraphEditing(api);
  await editing.requestProject(project.realPath);
  return { editing, api };
}

function title(editing: GraphEditing, id: string, value: string) {
  editing.editConversation(id, { ...editing.conversation(id).value, title: value });
}

describe("Graph editing through its public interface", () => {
  it("keeps independent drafts and recognizes reverting to committed content", async () => {
    const { editing, api } = await load();
    title(editing, "active-id", "First draft");
    title(editing, "archived-id", "Second draft");
    expect(editing.conversation("active-id").value.title).toBe("First draft");
    expect(editing.getState().dirtyDrafts).toHaveLength(2);
    title(editing, "active-id", "");
    expect(editing.getState().dirtyDrafts).toHaveLength(1);
    expect(editing.conversation("archived-id").value.title).toBe("Second draft");
    expect(api.updateNode).not.toHaveBeenCalled();
  });

  it("confirms only submitted content when the user edits during a save", async () => {
    const response = deferred<DashboardSnapshot>();
    const updateNode = vi.fn().mockReturnValueOnce(response.promise).mockResolvedValueOnce(nodeSnapshot("active-id", { title: "Later" }, "v2"));
    const { editing } = await load({ updateNode });
    title(editing, "active-id", "Submitted");
    const saving = editing.saveConversation("active-id");
    await waitFor(() => expect(updateNode).toHaveBeenCalledTimes(1));
    title(editing, "active-id", "Later");
    response.resolve(nodeSnapshot("active-id", { title: "Submitted" }, "v1"));
    await saving;
    expect(editing.conversation("active-id")).toMatchObject({ dirty: true, value: { title: "Later" } });
    await editing.saveConversation("active-id");
    expect(updateNode.mock.calls[1][2]).toBe("v1");
    expect(editing.getState().dirtyDrafts).toHaveLength(0);
  });

  it("keeps a revert made during a pending save as an unsaved change", async () => {
    const response = deferred<DashboardSnapshot>();
    const { editing, api } = await load({ updateNode: vi.fn().mockReturnValue(response.promise) });
    title(editing, "active-id", "Submitted");
    const saving = editing.saveConversation("active-id");
    await waitFor(() => expect(api.updateNode).toHaveBeenCalled());
    title(editing, "active-id", "");
    response.resolve(nodeSnapshot("active-id", { title: "Submitted" }, "v1"));
    await saving;
    expect(editing.conversation("active-id")).toMatchObject({ dirty: true, value: { title: "" } });
  });

  it("serializes metadata and layout writes using the returned file version", async () => {
    const first = deferred<DashboardSnapshot>();
    const saved = nodeSnapshot("active-id", { title: "Saved" }, "v1");
    const updateNode = vi.fn().mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce(nodeSnapshot("active-id", { layout: { x: 80, y: 90 } }, "v2", saved));
    const { editing } = await load({ updateNode });
    title(editing, "active-id", "Saved");
    const metadata = editing.saveConversation("active-id");
    const layout = editing.saveLayout("active-id", { x: 80, y: 90 });
    await waitFor(() => expect(updateNode).toHaveBeenCalledTimes(1));
    title(editing, "active-id", "New draft");
    first.resolve(saved);
    await Promise.all([metadata, layout]);
    expect(updateNode.mock.calls[1]).toEqual(["active-id", { layout: { x: 80, y: 90 } }, "v1"]);
    expect(editing.conversation("active-id")).toMatchObject({ dirty: true, value: { title: "New draft" } });
    expect(editing.getState().layoutDrafts.size).toBe(0);
  });

  it("stops a multi-draft save on failure and retries only remaining drafts", async () => {
    const first = nodeSnapshot("active-id", { title: "First" }, "v1");
    const second = nodeSnapshot("archived-id", { title: "Second" }, "v2", first);
    const updateNode = vi.fn().mockResolvedValueOnce(first).mockRejectedValueOnce(new Error("Disk full"))
      .mockResolvedValueOnce(second);
    const { editing, api } = await load({ updateNode, refresh: vi.fn().mockResolvedValue(second) });
    title(editing, "active-id", "First");
    title(editing, "archived-id", "Second");
    await editing.requestRefresh();
    await editing.resolvePending("save");
    expect(editing.getState().dirtyDrafts).toHaveLength(1);
    expect(editing.getState().decisionError).toBe("Disk full");
    expect(editing.conversation("active-id").dirty).toBe(false);
    expect(api.refresh).not.toHaveBeenCalled();
    await editing.resolvePending("save");
    expect(updateNode.mock.calls.map(([id]) => id)).toEqual(["active-id", "archived-id", "archived-id"]);
    expect(api.refresh).toHaveBeenCalledTimes(1);
    expect(editing.getState().pendingIntent).toBeNull();
  });

  it("pauses save-and-refresh if new edits arrive after the batch was captured", async () => {
    const response = deferred<DashboardSnapshot>();
    const { editing, api } = await load({ updateNode: vi.fn().mockReturnValue(response.promise) });
    title(editing, "active-id", "Submitted");
    await editing.requestRefresh();
    const saving = editing.resolvePending("save");
    await waitFor(() => expect(api.updateNode).toHaveBeenCalled());
    title(editing, "active-id", "Later");
    response.resolve(nodeSnapshot("active-id", { title: "Submitted" }, "v1"));
    await saving;
    expect(api.refresh).not.toHaveBeenCalled();
    expect(editing.getState().decisionError).toContain("New changes remain unsaved");
    expect(editing.conversation("active-id").value.title).toBe("Later");
  });

  it("does not switch Project after cancel or a failed draft save", async () => {
    const { editing, api } = await load({ updateNode: vi.fn().mockRejectedValue(new Error("Disk full")) });
    title(editing, "active-id", "Keep");
    await editing.requestProject("/projects/other");
    editing.cancelPending();
    expect(api.selectProject).toHaveBeenCalledTimes(1);
    await editing.requestProject("/projects/other");
    await editing.resolvePending("save");
    expect(api.selectProject).toHaveBeenCalledTimes(1);
    expect(editing.getState().project?.realPath).toBe(project.realPath);
    expect(editing.conversation("active-id").value.title).toBe("Keep");
  });

  it("keeps the newly selected Project visible when its source cannot be loaded", async () => {
    const { editing, api } = await load();
    const next = { ...project, realPath: "/projects/other" };
    vi.mocked(api.selectProject).mockResolvedValueOnce({ project: next, source: health.source });
    vi.mocked(api.snapshot).mockRejectedValueOnce(new ApiError(503, { code: "source_unavailable", message: "Offline", details: null, retryable: true }));
    title(editing, "active-id", "Discard");
    await editing.requestProject(next.realPath);
    await editing.resolvePending("discard");
    expect(editing.getState()).toMatchObject({ project: next, snapshot: null, loadState: "error", dirtyDrafts: [] });
  });

  it("allows recovery when a project switch fails after an old write was in flight", async () => {
    const pending = deferred<DashboardSnapshot>();
    const { editing, api } = await load({ updateNode: vi.fn().mockReturnValueOnce(pending.promise)
      .mockResolvedValueOnce(nodeSnapshot("active-id", { title: "Retry" }, "v2")) });
    vi.mocked(api.selectProject).mockRejectedValueOnce(new Error("Path does not exist"));
    title(editing, "active-id", "Submitted");
    const saving = editing.saveConversation("active-id");
    await waitFor(() => expect(api.updateNode).toHaveBeenCalledTimes(1));
    await editing.requestProject("/projects/missing");
    const switching = editing.resolvePending("discard");
    pending.resolve(nodeSnapshot("active-id", { title: "Submitted" }, "v1"));
    await Promise.all([saving, switching]);
    expect(editing.getState()).toMatchObject({ pendingSaves: 0, phase: "idle", project });
    title(editing, "active-id", "Retry");
    await editing.saveConversation("active-id");
    expect(api.updateNode).toHaveBeenCalledTimes(2);
  });

  it("rejects a late old-Project read while serializing the backend Project selection", async () => {
    const { editing, api } = await load();
    const oldRead = deferred<DashboardSnapshot>();
    const next = { ...project, realPath: "/projects/other" };
    vi.mocked(api.snapshot).mockReturnValueOnce(oldRead.promise).mockResolvedValueOnce({ ...snapshot, project: next });
    vi.mocked(api.selectProject).mockResolvedValueOnce({ project: next, source: health.source });
    const timeline = editing.changeTimeline({ granularity: "week", timezone: "UTC" });
    await waitFor(() => expect(api.snapshot).toHaveBeenCalledTimes(2));
    const selecting = editing.requestProject(next.realPath);
    expect(api.selectProject).toHaveBeenCalledTimes(1);
    oldRead.resolve(nodeSnapshot("active-id", { title: "Old reply" }, "old-reply"));
    await Promise.all([timeline, selecting]);
    expect(editing.getState().snapshot?.graph.etag).toBe("absent");
    expect(editing.getState().project?.realPath).toBe(next.realPath);
  });

  it("retains a failed layout as a draft that participates in refresh decisions", async () => {
    const { editing, api } = await load({ updateNode: vi.fn().mockRejectedValue(new Error("Read only")) });
    await editing.saveLayout("active-id", { x: 30, y: 40 });
    expect(editing.getState().layoutDrafts.get("active-id")).toEqual({ x: 30, y: 40 });
    await editing.requestRefresh();
    expect(api.refresh).not.toHaveBeenCalled();
    await editing.resolvePending("discard");
    expect(editing.getState().layoutDrafts.size).toBe(0);
    expect(api.refresh).toHaveBeenCalledTimes(1);
  });

  it("copies all captured drafts without confirming them or including later typing", async () => {
    const copied = deferred<GraphCopyResult>();
    const { editing, api } = await load({ updateNode: vi.fn().mockRejectedValue(conflict()), saveCopy: vi.fn().mockReturnValue(copied.promise) });
    title(editing, "active-id", "First");
    title(editing, "archived-id", "Second");
    await editing.saveConversation("active-id");
    const copying = editing.saveConflictCopy();
    await waitFor(() => expect(api.saveCopy).toHaveBeenCalled());
    title(editing, "active-id", "Later");
    copied.resolve({ copyPath: "/projects/copy.yaml" });
    await copying;
    expect(vi.mocked(api.saveCopy).mock.calls[0][0].nodes).toMatchObject({
      "active-id": { title: "First" }, "archived-id": { title: "Second" },
    });
    expect(editing.getState().dirtyDrafts).toHaveLength(2);
    expect(editing.conversation("active-id").value.title).toBe("Later");
    expect(editing.getState().copyPath).toBe("/projects/copy.yaml");
  });

  it("overwrites all captured drafts in sequence and retains edits made during overwrite", async () => {
    const overwritten = deferred<DashboardSnapshot>();
    const first = nodeSnapshot("active-id", { title: "First" }, "v1");
    const second = nodeSnapshot("archived-id", { title: "Second" }, "v2", first);
    const updateNode = vi.fn().mockRejectedValueOnce(conflict()).mockReturnValueOnce(overwritten.promise).mockResolvedValueOnce(second);
    const { editing } = await load({ updateNode });
    title(editing, "active-id", "First");
    title(editing, "archived-id", "Second");
    await editing.saveConversation("active-id");
    const overwriting = editing.overwriteConflict();
    await waitFor(() => expect(updateNode).toHaveBeenCalledTimes(2));
    title(editing, "active-id", "Later");
    overwritten.resolve(first);
    await overwriting;
    expect(updateNode.mock.calls[1].slice(2)).toEqual(["*", true]);
    expect(updateNode.mock.calls[2].slice(2)).toEqual(["v1"]);
    expect(editing.getState().conflict).toBeNull();
    expect(editing.getState().dirtyDrafts).toHaveLength(1);
    expect(editing.conversation("active-id").value.title).toBe("Later");
  });

  it("stops overwrite on a new conflict without repeating unconditional writes", async () => {
    const updateNode = vi.fn().mockRejectedValueOnce(conflict())
      .mockResolvedValueOnce(nodeSnapshot("active-id", { title: "First" }, "v1")).mockRejectedValueOnce(conflict());
    const { editing } = await load({ updateNode });
    title(editing, "active-id", "First");
    title(editing, "archived-id", "Second");
    await editing.saveConversation("active-id");
    await editing.overwriteConflict();
    expect(updateNode.mock.calls[2].slice(2)).toEqual(["v1"]);
    expect(editing.conversation("active-id").dirty).toBe(false);
    expect(editing.conversation("archived-id").dirty).toBe(true);
    expect(editing.getState().conflict).not.toBeNull();
  });

  it("clears all drafts only after a conflict reload succeeds", async () => {
    const { editing, api } = await load({ updateNode: vi.fn().mockRejectedValue(conflict()),
      refresh: vi.fn().mockRejectedValueOnce(new Error("Offline")).mockResolvedValueOnce(snapshot) });
    title(editing, "active-id", "First");
    title(editing, "archived-id", "Second");
    await editing.saveConversation("active-id");
    await editing.reloadConflict();
    expect(editing.getState().dirtyDrafts).toHaveLength(2);
    expect(editing.getState().conflictError).toBe("Offline");
    await editing.reloadConflict();
    expect(api.refresh).toHaveBeenCalledTimes(2);
    expect(editing.getState()).toMatchObject({ dirtyDrafts: [], conflict: null });
  });

  it("keeps a newly created relationship's identity for queued later edits", async () => {
    const response = deferred<DashboardSnapshot>();
    const edge = { id: "stable-edge", source: "active-id", target: "archived-id", type: "depends_on", label: "Submitted" };
    const created = { ...snapshot, graph: { ...snapshot.graph, etag: "v1", edges: [edge] } };
    const updated = { ...created, graph: { ...created.graph, etag: "v2", edges: [{ ...edge, label: "Later" }] } };
    const { editing, api } = await load({ createEdge: vi.fn().mockReturnValue(response.promise), updateEdge: vi.fn().mockResolvedValue(updated), refresh: vi.fn().mockResolvedValue(updated) });
    editing.editRelationship({ source: edge.source, target: edge.target, type: edge.type, label: "Submitted" });
    const creating = editing.saveRelationship();
    await waitFor(() => expect(api.createEdge).toHaveBeenCalled());
    editing.editRelationship({ ...editing.relationship().value, label: "Later" });
    await editing.requestRefresh();
    const continuing = editing.resolvePending("save");
    response.resolve(created);
    await Promise.all([creating, continuing]);
    expect(api.createEdge).toHaveBeenCalledTimes(1);
    expect(api.updateEdge).toHaveBeenCalledWith("stable-edge", expect.objectContaining({ label: "Later" }), "v1");
    expect(editing.getState().dirtyDrafts).toHaveLength(0);
    expect(api.refresh).toHaveBeenCalledTimes(1);
  });

  it("preserves separate existing-relationship and new-relationship drafts", async () => {
    const base = { ...snapshot, graph: { ...snapshot.graph, edges: [{ id: "edge", source: "active-id", target: "archived-id", type: "references" }] } };
    const { editing } = await load({ snapshot: vi.fn().mockResolvedValue(base) });
    editing.editRelationship({ source: "new-source", target: "new-target", type: "references", label: "New" });
    editing.selectRelationship("edge");
    expect(editing.getState().dirtyDrafts).toHaveLength(1);
    editing.editRelationship({ ...editing.relationship().value, label: "Existing" });
    editing.selectRelationship(null);
    expect(editing.relationship().value.label).toBe("New");
    editing.selectRelationship("edge");
    expect(editing.relationship().value.label).toBe("Existing");
    expect(editing.getState().dirtyDrafts).toHaveLength(2);
  });

  it("blocks copying an incomplete draft while retaining all drafts", async () => {
    const { editing, api } = await load({ updateNode: vi.fn().mockRejectedValue(conflict()) });
    title(editing, "active-id", "Keep");
    editing.editRelationship({ ...editing.relationship().value, source: "active-id" });
    await editing.saveConversation("active-id");
    await editing.saveConflictCopy();
    expect(api.saveCopy).not.toHaveBeenCalled();
    expect(editing.getState().conflictError).toContain("required");
    expect(editing.getState().dirtyDrafts).toHaveLength(2);
  });

  it("does not silently rebase open drafts when a timeline read finds an external file change", async () => {
    const { editing, api } = await load();
    title(editing, "active-id", "Local");
    vi.mocked(api.snapshot).mockResolvedValueOnce(nodeSnapshot("active-id", { title: "External" }, "external"));
    await expect(editing.changeTimeline({ granularity: "week", timezone: "UTC" })).rejects.toThrow("Graph changed");
    expect(editing.getState().snapshot?.graph.etag).toBe("absent");
    expect(editing.conversation("active-id").value.title).toBe("Local");
    expect(editing.getState().conflict).not.toBeNull();
  });
});
