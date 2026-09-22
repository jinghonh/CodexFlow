import { useEffect, useMemo, useSyncExternalStore } from "react";

import { ApiError } from "./api";
import type { DashboardApi, TimelineOptions } from "./api";
import type {
  Conversation, ConversationOverlayUpdate, DashboardSnapshot, GraphDocument,
  GraphDocumentNode, GraphEdge, GraphEdgeCreate, NodeLayout, ProjectView, UserStatus,
} from "./types";

export interface ConversationDraft {
  title: string;
  tags: string;
  status: UserStatus;
  note: string;
  hidden: boolean;
}

export interface RelationshipDraft {
  source: string;
  target: string;
  type: string;
  label: string;
}

type Change =
  | { kind: "node"; id: string; value: ConversationDraft }
  | { kind: "layout"; id: string; value: NodeLayout }
  | { kind: "edge"; id: string | null; value: RelationshipDraft }
  | { kind: "delete-edge"; id: string };

interface Draft {
  key: string;
  generation: number;
  revision: number;
  change: Change;
}

interface Feedback {
  status: "idle" | "saving" | "saved" | "error";
  error: string | null;
}

export interface GraphConflict {
  kind: "drafts" | "migration";
  message: string;
  expectedEtag: string | null;
  currentEtag: string | null;
}

type PendingIntent = { kind: "refresh" } | { kind: "project"; path: string };
type Phase = "idle" | "saving" | "refreshing" | "selecting" | "reloading" | "copying" | "migrating";

export interface GraphEditingState {
  project: ProjectView | null;
  snapshot: DashboardSnapshot | null;
  loadState: "idle" | "loading" | "ready" | "error";
  error: Error | null;
  phase: Phase;
  pendingSaves: number;
  dirtyDrafts: { key: string; label: string; error: string | null }[];
  layoutDrafts: ReadonlyMap<string, NodeLayout>;
  layoutError: string | null;
  relationshipId: string | null;
  pendingIntent: PendingIntent | null;
  decisionError: string | null;
  refreshError: string | null;
  conflict: GraphConflict | null;
  conflictAction: "idle" | "working" | "saved";
  conflictError: string | null;
  copyPath: string | null;
  migrationError: string | null;
  backupPath: string | null;
  timelineLoading: boolean;
  timelineError: string | null;
}

const IDLE: Feedback = { status: "idle", error: null };
const NEW_EDGE = "edge:new";
const nodeKey = (id: string) => `node:${id}`;
const edgeKey = (id: string | null) => id === null ? NEW_EDGE : `edge:existing:${id}`;
const emptyRelationship = (): RelationshipDraft => ({ source: "", target: "", type: "continues", label: "" });

/** Owns draft lifetimes and all requests that depend on the backend's selected Project. */
export class GraphEditing {
  private state: GraphEditingState = {
    project: null, snapshot: null, loadState: "idle", error: null, phase: "idle", pendingSaves: 0,
    dirtyDrafts: [], layoutDrafts: new Map(), layoutError: null, relationshipId: null,
    pendingIntent: null, decisionError: null, refreshError: null, conflict: null,
    conflictAction: "idle", conflictError: null, copyPath: null, migrationError: null,
    backupPath: null, timelineLoading: false, timelineError: null,
  };
  private drafts = new Map<string, Draft>();
  private feedback = new Map<string, Feedback>();
  private listeners = new Set<() => void>();
  private queue: Promise<void> = Promise.resolve();
  private epoch = 0;
  private revision = 0;
  private scheduled = new Map<number, number>();
  private confirmed = new Map<number, number>();
  private createdEdges = new Map<number, string>();

  constructor(private readonly api: DashboardApi) {}

  getState = (): GraphEditingState => this.state;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  invalidate(): void { this.epoch += 1; }

  get readOnly(): boolean {
    const status = this.state.snapshot?.graph.fileStatus ?? this.state.project?.graphFileStatus;
    return !this.state.snapshot || status === "future" || status === "legacy" || this.transitioning;
  }

  get transitioning(): boolean {
    return ["refreshing", "selecting", "reloading"].includes(this.state.phase);
  }

  conversation(id: string): { value: ConversationDraft; dirty: boolean } & Feedback {
    const draft = this.drafts.get(nodeKey(id));
    const value = draft?.change.kind === "node" ? draft.change.value : conversationDraft(this.findConversation(id));
    return { value, dirty: Boolean(draft), ...(this.feedback.get(nodeKey(id)) ?? IDLE) };
  }

  relationship(): { value: RelationshipDraft; dirty: boolean } & Feedback {
    const key = edgeKey(this.state.relationshipId);
    const draft = this.drafts.get(key);
    const edge = this.state.snapshot?.graph.edges.find(({ id }) => id === this.state.relationshipId);
    const value = draft?.change.kind === "edge" ? draft.change.value : edge ? relationshipDraft(edge) : emptyRelationship();
    return { value, dirty: Boolean(draft), ...(this.feedback.get(key) ?? IDLE) };
  }

  editConversation(id: string, value: ConversationDraft): void {
    if (!this.readOnly) this.stage({ kind: "node", id, value: { ...value } });
  }

  editRelationship(value: RelationshipDraft): void {
    if (!this.readOnly) this.stage({ kind: "edge", id: this.state.relationshipId, value: { ...value } });
  }

  selectRelationship(id: string | null): void {
    this.publish({ relationshipId: id });
  }

  discardRelationship(): void {
    const key = edgeKey(this.state.relationshipId);
    const draft = this.drafts.get(key);
    if (draft && this.scheduled.has(draft.generation)) return;
    this.drafts.delete(key);
    this.feedback.delete(key);
    this.publish({ relationshipId: null });
  }

  async saveConversation(id: string): Promise<void> {
    const draft = this.drafts.get(nodeKey(id));
    if (!this.readOnly && draft) await this.saveBatch([draft]);
  }

  async saveRelationship(): Promise<void> {
    if (this.readOnly) return;
    const key = edgeKey(this.state.relationshipId);
    const draft = this.drafts.get(key) ?? this.stage({
      kind: "edge", id: this.state.relationshipId, value: this.relationship().value,
    }, true);
    if (draft) await this.saveBatch([draft]);
  }

  async saveLayout(id: string, value: NodeLayout): Promise<void> {
    if (this.readOnly) return;
    const draft = this.stage({ kind: "layout", id, value: { ...value } });
    if (draft) await this.saveBatch([draft]);
  }

  async deleteRelationship(id: string): Promise<void> {
    if (this.readOnly || this.state.pendingSaves > 0) return;
    this.publish({ relationshipId: id });
    const draft = this.stage({ kind: "delete-edge", id });
    if (draft) await this.saveBatch([draft]);
  }

  async requestProject(path: string): Promise<void> {
    if (this.state.phase !== "idle") return;
    if (!path.trim()) {
      this.publish({ error: new Error("Choose a Project root first.") });
      return;
    }
    await this.requestIntent({ kind: "project", path: path.trim() });
  }

  async requestRefresh(): Promise<void> {
    if (this.state.phase === "idle") await this.requestIntent({ kind: "refresh" });
  }

  cancelPending(): void {
    if (this.state.phase === "idle") this.publish({ pendingIntent: null, decisionError: null });
  }

  async resolvePending(choice: "save" | "discard"): Promise<void> {
    const intent = this.state.pendingIntent;
    if (!intent || this.state.phase !== "idle") return;
    const epoch = this.epoch;
    if (choice === "save") {
      this.publish({ phase: "saving", decisionError: null });
      const failure = await this.saveBatch([...this.drafts.values()]);
      if (epoch !== this.epoch) return;
      if (failure || this.drafts.size > 0) {
        this.publish({ phase: "idle", decisionError: failure?.message ?? "New changes remain unsaved. Choose what to do before continuing." });
        return;
      }
    }
    await this.performIntent(intent, choice === "discard");
  }

  async changeTimeline(options: TimelineOptions): Promise<void> {
    if (this.state.phase !== "idle") return;
    const epoch = this.epoch;
    this.publish({ timelineLoading: true, timelineError: null });
    let failure: Error | null = null;
    await this.serial(async () => {
      try {
        const snapshot = await this.api.snapshot(options);
        if (epoch !== this.epoch) return;
        if (this.drafts.size && this.state.snapshot && snapshot.graph.etag !== this.state.snapshot.graph.etag) {
          const conflict = new ApiError(412, {
            code: "graph_conflict", message: "The Graph changed while drafts were open. Resolve the conflict before loading new views.",
            details: { expectedEtag: this.state.snapshot.graph.etag, currentEtag: snapshot.graph.etag }, retryable: true,
          });
          this.rememberConflict(conflict, "drafts");
          throw conflict;
        }
        this.adopt(snapshot);
      } catch (reason) {
        failure = asError(reason);
        if (epoch === this.epoch) this.publish({ timelineError: failure.message });
      }
    }, epoch);
    if (epoch === this.epoch) this.publish({ timelineLoading: false });
    if (failure) throw failure;
  }

  async reloadConflict(): Promise<void> {
    if (!this.state.conflict || this.state.phase !== "idle") return;
    const epoch = this.epoch;
    this.publish({ phase: "reloading", conflictAction: "working", conflictError: null });
    await this.serial(async () => {
      try {
        const snapshot = await this.refreshRequest();
        if (epoch !== this.epoch) return;
        this.clearDrafts();
        this.adopt(snapshot);
        this.publish({ conflict: null, pendingIntent: null, decisionError: null });
      } catch (reason) {
        if (epoch === this.epoch) this.publish({ conflictError: asError(reason).message });
      }
    }, epoch);
    if (epoch === this.epoch) this.publish({ phase: "idle", conflictAction: "idle" });
  }

  async saveConflictCopy(): Promise<void> {
    if (!this.state.conflict || !this.state.snapshot || this.state.phase !== "idle") return;
    const epoch = this.epoch;
    const base = this.state.snapshot;
    const drafts = [...this.drafts.values()];
    this.publish({ phase: "copying", conflictAction: "working", conflictError: null });
    await this.serial(async () => {
      try {
        const document = copyDocument(base, drafts.map((draft) => this.resolveChange(draft)));
        const result = await this.api.saveCopy(document);
        if (epoch === this.epoch) this.publish({ copyPath: result.copyPath, conflictAction: "saved" });
      } catch (reason) {
        if (epoch === this.epoch) this.publish({ conflictError: asError(reason).message, conflictAction: "idle" });
      }
    }, epoch);
    if (epoch === this.epoch) this.publish({ phase: "idle" });
  }

  async overwriteConflict(): Promise<void> {
    if (!this.state.conflict || this.state.phase !== "idle") return;
    if (this.state.conflict.kind === "migration") {
      await this.migrate(true);
      return;
    }
    const epoch = this.epoch;
    this.publish({ phase: "saving", conflictAction: "working", conflictError: null });
    const failure = await this.saveBatch([...this.drafts.values()], true);
    if (epoch === this.epoch) this.publish({
      phase: "idle", conflictAction: "idle", conflictError: failure?.message ?? null,
      ...(!failure ? { conflict: null } : {}),
    });
  }

  async migrate(overwrite = false): Promise<void> {
    if (!this.state.snapshot || this.state.phase !== "idle") return;
    const epoch = this.epoch;
    this.publish({ phase: "migrating", migrationError: null, conflictAction: "working", conflictError: null });
    await this.serial(async () => {
      try {
        const result = overwrite
          ? await this.api.migrateGraph("*", true)
          : await this.api.migrateGraph(this.state.snapshot!.graph.etag);
        if (epoch !== this.epoch) return;
        this.adopt(result);
        this.publish({ backupPath: result.backupPath, conflict: null });
      } catch (reason) {
        if (epoch !== this.epoch) return;
        this.rememberConflict(reason, "migration");
        this.publish({ migrationError: asError(reason).message, conflictError: overwrite ? asError(reason).message : null });
      }
    }, epoch);
    if (epoch === this.epoch) this.publish({ phase: "idle", conflictAction: "idle" });
  }

  private stage(change: Change, force = false): Draft | undefined {
    const key = changeKey(change);
    const previous = this.drafts.get(key);
    const revision = ++this.revision;
    const draft = { key, generation: previous?.generation ?? revision, revision, change };
    const pending = previous && this.scheduled.has(previous.generation);
    if (!force && !pending && this.isClean(change)) this.drafts.delete(key);
    else this.drafts.set(key, draft);
    this.feedback.set(key, { status: pending ? "saving" : "idle", error: null });
    this.publish();
    return this.drafts.get(key);
  }

  private async saveBatch(drafts: Draft[], overwrite = false): Promise<Error | null> {
    const epoch = this.epoch;
    let failure: Error | null = null;
    for (const draft of drafts) {
      this.scheduled.set(draft.generation, (this.scheduled.get(draft.generation) ?? 0) + 1);
      this.feedback.set(draft.key, { status: "saving", error: null });
    }
    this.publish({ pendingSaves: this.state.pendingSaves + 1 });
    await this.serial(async () => {
      let force = overwrite;
      for (const draft of drafts) {
        if ((this.confirmed.get(draft.generation) ?? 0) >= draft.revision) continue;
        try {
          if (this.state.conflict && !force) throw new Error("Resolve the Graph conflict before saving more changes.");
          await this.commit(draft, force, epoch);
          force = false;
        } catch (reason) {
          failure = asError(reason);
          if (epoch === this.epoch) {
            this.feedback.set(changeKey(this.resolveChange(draft)), { status: "error", error: describeMutationError(reason) });
            this.rememberConflict(reason, "drafts");
          }
          break;
        }
        if (epoch !== this.epoch) return;
      }
    }, epoch);
    for (const draft of drafts) {
      const remaining = (this.scheduled.get(draft.generation) ?? 1) - 1;
      if (remaining > 0) this.scheduled.set(draft.generation, remaining);
      else this.scheduled.delete(draft.generation);
      const key = changeKey(this.resolveChange(draft));
      if (epoch === this.epoch && !remaining && this.feedback.get(key)?.status === "saving") {
        this.feedback.set(key, IDLE);
      }
      if (!remaining && ![...this.drafts.values()].some((current) => current.generation === draft.generation)) {
        this.confirmed.delete(draft.generation);
        this.createdEdges.delete(draft.generation);
      }
    }
    if (epoch === this.epoch) this.publish({ pendingSaves: this.state.pendingSaves - 1 });
    return failure;
  }

  private async commit(draft: Draft, overwrite: boolean, epoch: number): Promise<void> {
    const base = this.state.snapshot;
    if (!base) throw new Error("Load a Project before saving Graph changes.");
    const change = this.resolveChange(draft);
    const etag = overwrite ? "*" : base.graph.etag;
    const force: [boolean] | [] = overwrite ? [true] : [];
    let updated: DashboardSnapshot;
    switch (change.kind) {
      case "node": updated = await this.api.updateNode(change.id, nodeChanges(change.value), etag, ...force); break;
      case "layout": updated = await this.api.updateNode(change.id, { layout: change.value }, etag, ...force); break;
      case "delete-edge": updated = await this.api.deleteEdge(change.id, etag, ...force); break;
      case "edge": {
        const fields = edgeChanges(change.value);
        updated = change.id === null
          ? await this.api.createEdge(fields, etag, ...force)
          : await this.api.updateEdge(change.id, fields, etag, ...force);
        break;
      }
    }
    if (epoch !== this.epoch) return;
    let key = changeKey(change);
    let current = this.drafts.get(key);
    if (change.kind === "edge" && change.id === null) {
      const before = new Set(base.graph.edges.map(({ id }) => id));
      const created = updated.graph.edges.find((edge) => !before.has(edge.id));
      if (created) {
        this.createdEdges.set(draft.generation, created.id);
        if (current?.generation === draft.generation && current.revision !== draft.revision && current.change.kind === "edge") {
          this.drafts.delete(key);
          key = edgeKey(created.id);
          current = { ...current, key, change: { ...change, id: created.id, value: current.change.value } };
          this.drafts.set(key, current);
          if (this.state.relationshipId === null) this.publish({ relationshipId: created.id });
        }
      } else if (current && current.revision !== draft.revision) {
        throw new Error("The saved relationship identity was not returned. Reload the Graph before retrying this draft.");
      }
    }
    this.confirmed.set(draft.generation, draft.revision);
    this.adopt(updated);
    if (current?.generation === draft.generation &&
      (current.revision === draft.revision || this.isClean(current.change))) {
      this.drafts.delete(key);
      if (change.kind === "edge" || change.kind === "delete-edge") {
        if (this.state.relationshipId === change.id) this.publish({ relationshipId: null });
      }
    }
    this.feedback.set(key, { status: this.drafts.has(key) ? "idle" : "saved", error: null });
    if (change.kind === "edge" || change.kind === "delete-edge") {
      this.feedback.set(NEW_EDGE, { status: "saved", error: null });
    }
    this.publish({ conflict: null });
  }

  private resolveChange(draft: Draft): Change {
    if (draft.change.kind !== "edge" || draft.change.id !== null) return draft.change;
    return { ...draft.change, id: this.createdEdges.get(draft.generation) ?? null };
  }

  private async requestIntent(intent: PendingIntent): Promise<void> {
    if (this.drafts.size > 0 || this.state.pendingSaves > 0) {
      this.publish({ pendingIntent: intent, decisionError: null });
    } else await this.performIntent(intent, false);
  }

  private async performIntent(intent: PendingIntent, discard: boolean): Promise<void> {
    if (intent.kind === "project") {
      this.epoch += 1;
      for (const [key, value] of this.feedback) {
        if (value.status === "saving") this.feedback.set(key, IDLE);
      }
    }
    const epoch = this.epoch;
    this.publish({ phase: intent.kind === "project" ? "selecting" : "refreshing", decisionError: null, refreshError: null,
      ...(intent.kind === "project" ? { loadState: "loading" as const, error: null, pendingSaves: 0, timelineLoading: false } : {}),
    });
    await this.serial(async () => {
      try {
        if (intent.kind === "project") {
          const selected = await this.api.selectProject(intent.path);
          if (epoch !== this.epoch) return;
          this.clearDrafts();
          this.confirmed.clear();
          this.createdEdges.clear();
          this.publish({ project: selected.project, snapshot: null, conflict: null, copyPath: null,
            backupPath: null, migrationError: null, pendingSaves: 0, timelineError: null, timelineLoading: false });
          const snapshot = await this.api.snapshot();
          if (epoch === this.epoch) this.adopt(snapshot);
        } else {
          if (discard) this.clearDrafts();
          const snapshot = await this.refreshRequest();
          if (epoch === this.epoch) this.adopt(snapshot);
        }
        if (epoch === this.epoch) this.publish({ error: null, conflict: null });
      } catch (reason) {
        if (epoch !== this.epoch) return;
        const error = asError(reason);
        if (intent.kind === "project") this.publish({ loadState: "error", error });
        else this.publish({ refreshError: describeRefreshError(error, this.state.snapshot !== null),
          ...(error instanceof ApiError ? { error } : {}) });
      }
    }, epoch);
    if (epoch === this.epoch) this.publish({ phase: "idle", pendingIntent: null });
  }

  private refreshRequest(): Promise<DashboardSnapshot> {
    const timeline = this.state.snapshot?.timeline;
    return timeline ? this.api.refresh({ granularity: timeline.granularity, timezone: timeline.timezone }) : this.api.refresh();
  }

  private serial(action: () => Promise<void>, epoch: number): Promise<void> {
    const result = this.queue.then(async () => { if (epoch === this.epoch) await action(); });
    this.queue = result.catch(() => undefined);
    return result;
  }

  private isClean(change: Change): boolean {
    switch (change.kind) {
      case "node": return equal(nodeChanges(change.value), nodeChanges(conversationDraft(this.findConversation(change.id))));
      case "layout": return equal(change.value, this.state.snapshot?.graph.nodes.find(({ id }) => id === change.id)?.layout);
      case "delete-edge": return false;
      case "edge": {
        const original = this.state.snapshot?.graph.edges.find(({ id }) => id === change.id);
        return equal(change.value, original ? relationshipDraft(original) : emptyRelationship());
      }
    }
  }

  private findConversation(id: string): Conversation | undefined {
    return this.state.snapshot?.conversations.find((conversation) => conversation.id === id);
  }

  private clearDrafts(): void {
    this.drafts.clear();
    this.feedback.clear();
    this.publish({ relationshipId: null });
  }

  private adopt(snapshot: DashboardSnapshot): void {
    this.publish({ snapshot, project: snapshot.project, loadState: "ready" });
  }

  private rememberConflict(reason: unknown, kind: GraphConflict["kind"]): void {
    if (!(reason instanceof ApiError) || reason.payload.code !== "graph_conflict") return;
    const details = reason.payload.details;
    this.publish({ conflict: {
      kind, message: reason.message,
      expectedEtag: typeof details?.expectedEtag === "string" ? details.expectedEtag : null,
      currentEtag: typeof details?.currentEtag === "string" ? details.currentEtag : null,
    }, conflictAction: "idle", conflictError: null, copyPath: null });
  }

  private publish(changes: Partial<GraphEditingState> = {}): void {
    const layoutDrafts = new Map<string, NodeLayout>();
    let layoutError: string | null = null;
    const dirtyDrafts = [...this.drafts.values()].map(({ key, change }) => {
      const error = this.feedback.get(key)?.error ?? null;
      let label: string;
      if (change.kind === "node" || change.kind === "layout") {
        label = `${this.findConversation(change.id)?.displayTitle ?? change.id} · ${change.kind === "node" ? "metadata" : "layout"}`;
        if (change.kind === "layout") { layoutDrafts.set(change.id, change.value); layoutError ??= error; }
      } else label = change.kind === "delete-edge" ? `Delete relationship ${change.id}` : `Relationship ${change.value.source || "…"} → ${change.value.target || "…"}`;
      return { key, label, error };
    });
    this.state = { ...this.state, ...changes, dirtyDrafts, layoutDrafts, layoutError };
    this.listeners.forEach((listener) => listener());
  }
}

export function useGraphEditing(api: DashboardApi): [GraphEditingState, GraphEditing] {
  const editing = useMemo(() => new GraphEditing(api), [api]);
  const state = useSyncExternalStore(editing.subscribe, editing.getState, editing.getState);
  useEffect(() => {
    const onLeave = (event: BeforeUnloadEvent) => {
      if (editing.getState().dirtyDrafts.length === 0) return;
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", onLeave);
    return () => { window.removeEventListener("beforeunload", onLeave); editing.invalidate(); };
  }, [editing]);
  return [state, editing];
}

function changeKey(change: Change): string {
  if (change.kind === "node") return nodeKey(change.id);
  if (change.kind === "layout") return `layout:${change.id}`;
  return edgeKey(change.id);
}

function conversationDraft(conversation: Conversation | undefined): ConversationDraft {
  return {
    title: conversation?.overlay.title ?? "", tags: conversation?.overlay.tags.join(", ") ?? "",
    status: conversation?.overlay.status ?? "none", note: conversation?.overlay.note ?? "",
    hidden: conversation?.overlay.hidden ?? false,
  };
}

function relationshipDraft(edge: GraphEdge): RelationshipDraft {
  return { source: edge.source, target: edge.target, type: edge.type, label: edge.label ?? "" };
}

function nodeChanges(value: ConversationDraft): ConversationOverlayUpdate {
  return { title: value.title.trim(), tags: [...new Set(value.tags.split(",").map((tag) => tag.trim()).filter(Boolean))],
    status: value.status, note: value.note.trim() ? value.note : null, hidden: value.hidden };
}

function edgeChanges(value: RelationshipDraft): GraphEdgeCreate {
  const source = value.source.trim();
  const target = value.target.trim();
  const type = value.type.trim();
  if (!source || !target || !type) throw new Error("Source, target, and relationship type are required.");
  if (source === target) throw new Error("A relationship cannot connect a Conversation to itself.");
  return { source, target, type, label: value.label.trim() || null };
}

function copyDocument(snapshot: DashboardSnapshot, changes: Change[]): GraphDocument {
  const nodes: Record<string, GraphDocumentNode> = {};
  for (const conversation of snapshot.conversations) {
    const node = sparseNode(conversation.overlay);
    if (Object.keys(node).length) nodes[conversation.id] = node;
  }
  let edges = snapshot.graph.edges.map((edge) => ({ ...edge }));
  for (const change of changes) {
    if (change.kind === "node" || change.kind === "layout") {
      const current = nodes[change.id] ?? {};
      const node = sparseNode({ ...current, ...(change.kind === "node" ? nodeChanges(change.value) : { layout: change.value }) });
      if (Object.keys(node).length) nodes[change.id] = node;
      else delete nodes[change.id];
    } else if (change.kind === "delete-edge") {
      edges = edges.filter(({ id }) => id !== change.id);
    } else {
      const fields = edgeChanges(change.value);
      const edge: GraphEdge = { id: change.id ?? `draft-${crypto.randomUUID()}`, source: fields.source, target: fields.target, type: fields.type };
      if (fields.label) edge.label = fields.label;
      edges = change.id === null ? [...edges, edge] : edges.map((current) => current.id === change.id ? edge : current);
    }
  }
  return { version: 1, nodes, edges };
}

function sparseNode(overlay: ConversationOverlayUpdate): GraphDocumentNode {
  const node: GraphDocumentNode = {};
  if (overlay.title?.trim()) node.title = overlay.title.trim();
  if (overlay.tags?.length) node.tags = [...overlay.tags];
  if (overlay.status && overlay.status !== "none") node.status = overlay.status;
  if (overlay.note?.trim()) node.note = overlay.note.trim();
  if (overlay.hidden) node.hidden = true;
  if (overlay.layout) node.layout = { ...overlay.layout };
  return node;
}

function equal(left: unknown, right: unknown): boolean { return JSON.stringify(left) === JSON.stringify(right); }

function asError(reason: unknown): Error {
  return reason instanceof Error ? reason : new Error("Unexpected local runtime error.");
}

function describeMutationError(reason: unknown): string {
  const error = asError(reason);
  if (error instanceof ApiError) {
    return `Error category: ${error.payload.code}. ${error.message} Your current edits remain here. Retryable: ${error.payload.retryable ? "yes" : "no"}. ${error.payload.retryable ? "Retry the save or resolve the Graph conflict." : "Review the field values before trying again."}`;
  }
  return `Error category: local_error. ${error.message} Your current edits remain here. Retry the save when ready.`;
}

function describeRefreshError(error: Error, hasSnapshot: boolean): string {
  const category = error instanceof ApiError ? error.payload.code : "local_error";
  const retryable = error instanceof ApiError ? error.payload.retryable : true;
  return `Error category: ${category}. ${error.message} ${hasSnapshot ? "The last complete snapshot is preserved." : "No complete source snapshot is available yet."} Retryable: ${retryable ? "yes" : "no"}. ${retryable ? "Retry the source refresh when it is available." : "Review the error details before trying again."}`;
}
