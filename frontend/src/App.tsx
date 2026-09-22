import { useEffect, useMemo, useRef, useState } from "react";

import { ApiError, createApi } from "./api";
import type { DashboardApi } from "./api";
import {
  DEFAULT_CONVERSATION_FILTERS,
  filterConversations,
  isDefaultConversationFilters,
} from "./filters";
import { TimelineView } from "./Timeline";
import { useGraphEditing } from "./graphEditing";
import type { ConversationDraft, GraphConflict, GraphEditing, GraphEditingState } from "./graphEditing";
import type {
  Conversation,
  ConversationFilters,
  ExcludedConversation,
  GraphEdge,
  GraphNode,
  HealthResponse,
  UserStatus,
} from "./types";

interface AppProps {
  api?: DashboardApi;
}

export function App({ api }: AppProps) {
  const apiClient = useMemo(() => api ?? createApi(), [api]);
  const [state, editing] = useGraphEditing(apiClient);
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [healthError, setHealthError] = useState<Error | null>(null);
  const [projectPath, setProjectPath] = useState("");
  const [selectedConversationId, setSelectedConversationId] = useState<string | null>(null);
  const [filters, setFilters] = useState<ConversationFilters>(DEFAULT_CONVERSATION_FILTERS);
  const { project, snapshot, loadState, timelineLoading, timelineError } = state;
  const error = state.error ?? healthError;
  const dirtyDraftCount = state.dirtyDrafts.length;

  useEffect(() => {
    let active = true;
    void apiClient.health().then((response) => {
      if (active) setHealth(response);
    }).catch((reason: unknown) => {
      if (active) setHealthError(reason instanceof Error ? reason : new Error("Unexpected local runtime error."));
    });
    return () => { active = false; };
  }, [apiClient]);

  useEffect(() => {
    setSelectedConversationId(null);
    setFilters(DEFAULT_CONVERSATION_FILTERS);
  }, [project?.realPath]);

  useEffect(() => {
    if (!snapshot) return;
    setSelectedConversationId((id) => id && snapshot.conversations.some((conversation) => conversation.id === id) ? id : null);
  }, [snapshot]);

  function handleProjectSelect(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void editing.requestProject(projectPath);
  }

  const sourceUnavailable =
    ((error instanceof ApiError && error.status === 503) && !snapshot) ||
    snapshot?.source.status === "unavailable" ||
    snapshot?.source.status === "incompatible";
  const sourceStale = snapshot?.source.status === "stale";
  const graphFileStatus = snapshot?.graph.fileStatus ?? project?.graphFileStatus;
  const graphReadOnly = editing.readOnly;
  const runtimeLabel = health ? "Local runtime ready" : error && !project ? "Local runtime unavailable" : "Checking local runtime";
  const filteredConversations = useMemo(
    () => filterConversations(snapshot?.conversations ?? [], filters),
    [filters, snapshot?.conversations],
  );
  const filteredConversationIds = useMemo(
    () => {
      const ids = new Set(filteredConversations.map(({ id }) => id));
      if (isDefaultConversationFilters(filters)) {
        snapshot?.graph.nodes.forEach((node) => ids.add(node.id));
      }
      return ids;
    },
    [filteredConversations, filters, snapshot?.graph.nodes],
  );
  const availableTags = useMemo(
    () => Array.from(new Set((snapshot?.conversations ?? []).flatMap(({ overlay }) => overlay.tags))).sort(),
    [snapshot?.conversations],
  );

  return (
    <main className="app-shell">
      <header className="topbar">
        <div>
          <p className="eyebrow">CODEXFLOW / LOCAL THREAD INDEX</p>
          <h1>Conversation field notes</h1>
        </div>
        <div className={`runtime-chip ${health ? "is-ready" : ""}`}>
          <span className="status-dot" aria-hidden="true" />
          <span>{runtimeLabel}</span>
        </div>
      </header>

      <section className="hero-grid" aria-labelledby="hero-title">
        <div className="hero-copy">
          <p className="section-kicker">A quiet index for noisy work</p>
          <h2 id="hero-title">Find the thread<br />behind the work.</h2>
          <p className="hero-description">
            Choose one local Project root. CodexFlow reads the app-server source and keeps the
            original Thread identity intact.
          </p>
        </div>
        <form className="project-card" onSubmit={handleProjectSelect}>
          <div className="card-index">01 <span>/ PROJECT ROOT</span></div>
          <label htmlFor="project-path">Project root</label>
          <div className="path-input-row">
            <input
              id="project-path"
              value={projectPath}
              onChange={(event) => setProjectPath(event.target.value)}
              placeholder="/Users/you/Code/project"
              autoComplete="off"
            />
            <button type="submit" disabled={state.phase !== "idle"}>
              {loadState === "loading" ? "Loading…" : "Load Project"}
            </button>
          </div>
          <p className="card-footnote">The path is checked locally and resolved before reading.</p>
        </form>
      </section>

      {project && (
        <div className="project-ribbon">
          <div>
            <span className="ribbon-label">SELECTED PROJECT</span>
            <strong>{project.realPath}</strong>
            {project.originalPath !== project.realPath && (
              <span className="project-alias">alias · {project.originalPath}</span>
            )}
          </div>
          <div className="ribbon-facts">
            <span className="ribbon-meta">{project.isGitProject ? "GIT PROJECT" : "LOCAL DIRECTORY"}</span>
            {project.gitRoot && <span>Git root · {project.gitRoot}</span>}
            {project.worktreeRoot && <span>Worktree root · {project.worktreeRoot}</span>}
            <span>Graph overlay · {snapshot?.project.graphFileStatus ?? project.graphFileStatus}</span>
          </div>
          {project && (
            <div className="ribbon-actions">
              <span className={`draft-status ${dirtyDraftCount > 0 ? "is-dirty" : ""}`}>
                {dirtyDraftCount > 0
                  ? "Unsaved Graph changes"
                  : snapshot
                  ? "Source sync ready"
                  : "Source load needs retry"}
              </span>
              <button
                type="button"
                className="refresh-button"
                onClick={() => void editing.requestRefresh()}
                disabled={state.phase !== "idle"}
              >
                {state.phase === "refreshing" ? "Refreshing…" : "Refresh source"}
              </button>
              {state.refreshError && <span className="refresh-error" role="alert">Refresh failed · {state.refreshError}</span>}
            </div>
          )}
        </div>
      )}

      {loadState === "loading" && (
        <div className="loading-panel" role="status" aria-live="polite">
          <span className="loading-mark" aria-hidden="true">↗</span>
          <span>Loading conversations…</span>
        </div>
      )}

      {sourceUnavailable && (
        <div className="source-alert" role="alert">
          <div className="alert-symbol" aria-hidden="true">!</div>
          <div>
            <strong>Source unavailable</strong>
            <p>{error?.message ?? snapshot?.source.error?.message ?? "The Codex app-server source is not available yet."}</p>
            <span className="alert-note">No incomplete list has been presented as a complete snapshot.</span>
          </div>
        </div>
      )}

      {sourceStale && !sourceUnavailable && (
        <div className="source-stale" role="status" aria-live="polite">
          <span className="stale-mark" aria-hidden="true">↻</span>
          <div>
            <strong>Source stale</strong>
            <p>
              {snapshot.source.error?.message ?? "The last complete source snapshot is still shown; refresh could not confirm newer data."}
            </p>
            <span className="alert-note">The last complete snapshot is preserved; retry the source refresh when available.</span>
          </div>
        </div>
      )}

      {state.pendingIntent && (
        <RefreshDecisionDialog
          drafts={state.dirtyDrafts}
          action={state.phase}
          switchingProject={state.pendingIntent.kind === "project"}
          error={state.decisionError}
          onSave={() => void editing.resolvePending("save")}
          onDiscard={() => void editing.resolvePending("discard")}
          onCancel={() => editing.cancelPending()}
        />
      )}

      {state.conflict && (
        <GraphConflictPanel
          conflict={state.conflict}
          drafts={state.dirtyDrafts}
          action={state.conflictAction}
          copyPath={state.copyPath}
          actionError={state.conflictError}
          onReload={() => editing.reloadConflict()}
          onSaveCopy={() => editing.saveConflictCopy()}
          onOverwrite={() => editing.overwriteConflict()}
        />
      )}

      {graphFileStatus === "future" || graphFileStatus === "legacy" || state.backupPath ? (
        <GraphStatusNotice
          status={graphFileStatus ?? "ready"}
          migrationState={state.phase === "migrating" ? "working" : "idle"}
          migrationError={state.migrationError}
          backupPath={state.backupPath}
          onMigrate={() => editing.migrate()}
        />
      ) : null}

      {snapshot && !sourceUnavailable && snapshot.excludedConversations.length > 0 && (
        <ProjectMembershipNotice
          excluded={snapshot.excludedConversations}
          isGitProject={project?.isGitProject ?? snapshot.project.isGitProject}
        />
      )}

      {snapshot && !sourceUnavailable && (
        <>
          <ConversationFilterBar
            filters={filters}
            tags={availableTags}
            visibleCount={filteredConversations.length}
            totalCount={snapshot.conversations.length}
            onChange={(changes) => setFilters((current) => ({ ...current, ...changes }))}
            onReset={() => setFilters(DEFAULT_CONVERSATION_FILTERS)}
          />
          <TimelineView
            timeline={snapshot.timeline}
            conversations={filteredConversations}
            selectedId={selectedConversationId}
            loading={timelineLoading || state.phase !== "idle" || state.pendingSaves > 0}
            error={timelineError}
            onSelect={setSelectedConversationId}
            onOptionsChange={(options) => editing.changeTimeline(options)}
          />
          <ConversationGraph
            key={project?.realPath}
            nodes={snapshot.graph.nodes}
            edges={snapshot.graph.edges}
            visibleIds={filteredConversationIds}
            editing={editing}
            layoutDrafts={state.layoutDrafts}
            layoutError={state.layoutError}
            selectedId={selectedConversationId}
            onSelect={setSelectedConversationId}
            readOnly={graphReadOnly}
          />
          <ConversationList
            conversations={filteredConversations}
            selectedId={selectedConversationId}
            onSelect={setSelectedConversationId}
          />
        </>
      )}

      {selectedConversationId && snapshot && (
        <ConversationDetail
          conversation={snapshot.conversations.find(({ id }) => id === selectedConversationId) ?? null}
          editing={editing}
          readOnly={graphReadOnly}
        />
      )}

      {loadState === "error" && !sourceUnavailable && error && (
        <div className="source-alert" role="alert">
          <div className="alert-symbol" aria-hidden="true">!</div>
          <div>
            <strong>Project could not be loaded</strong>
            <p>{error.message}</p>
          </div>
        </div>
      )}
    </main>
  );
}

function RefreshDecisionDialog({
  drafts,
  action,
  switchingProject,
  error,
  onSave,
  onDiscard,
  onCancel,
}: {
  drafts: GraphEditingState["dirtyDrafts"];
  action: GraphEditingState["phase"];
  switchingProject: boolean;
  error: string | null;
  onSave: () => void;
  onDiscard: () => void;
  onCancel: () => void;
}) {
  const working = action !== "idle";
  const dirtyCount = drafts.length;
  const nextAction = switchingProject ? "switch Project" : "refresh";
  return (
    <div className="refresh-dialog-backdrop">
      <section className="refresh-dialog" role="dialog" aria-modal="true" aria-labelledby="refresh-dialog-title">
        <p className="section-kicker">SOURCE REFRESH / UNSAVED WORK</p>
        <h2 id="refresh-dialog-title">Save your Graph changes first?</h2>
        <p>
          {dirtyCount === 1 ? "One" : dirtyCount} unsaved Graph draft{dirtyCount === 1 ? " is" : "s are"} open. Choose what to do before {switchingProject ? "switching Project" : "reading the source again"}.
        </p>
        <ul className="draft-summary" aria-label="Unsaved Graph drafts">
          {drafts.map((draft) => <li key={draft.key}>{draft.label}</li>)}
        </ul>
        {error && <p className="refresh-dialog-error" role="alert">Save failed · {error} Your draft is still dirty.</p>}
        <div className="refresh-dialog-actions">
          <button type="button" className="refresh-dialog-save" onClick={onSave} disabled={working}>
            {action === "saving" ? "Saving…" : `Save changes & ${nextAction}`}
          </button>
          <button type="button" className="refresh-dialog-discard" onClick={onDiscard} disabled={working}>
            {working && action !== "saving" ? "Loading…" : `Discard changes & ${nextAction}`}
          </button>
          <button type="button" className="refresh-dialog-cancel" onClick={onCancel} disabled={working}>
            Cancel
          </button>
        </div>
      </section>
    </div>
  );
}

function GraphConflictPanel({
  conflict,
  drafts,
  action,
  copyPath,
  actionError,
  onReload,
  onSaveCopy,
  onOverwrite,
}: {
  conflict: GraphConflict;
  drafts: GraphEditingState["dirtyDrafts"];
  action: "idle" | "working" | "saved";
  copyPath: string | null;
  actionError: string | null;
  onReload: () => Promise<void>;
  onSaveCopy: () => Promise<void>;
  onOverwrite: () => Promise<void>;
}) {
  const working = action === "working";
  return (
    <section className="graph-conflict-panel" role="alert" aria-label="Graph conflict">
      <div className="conflict-symbol" aria-hidden="true">!</div>
      <div className="conflict-copy">
        <p className="section-kicker">PERSISTENCE / EXTERNAL CHANGE</p>
        <strong>Graph changed elsewhere</strong>
        <p>{conflict.message}</p>
        <span className="conflict-note">
          All {drafts.length} unsaved Graph drafts are retained. Copy and overwrite include all current drafts; later edits stay unsaved.
        </span>
        {(conflict.expectedEtag || conflict.currentEtag) && (
          <div className="conflict-version" aria-label="Graph version comparison">
            <span>Draft base · <code>{conflict.expectedEtag ?? "unknown"}</code></span>
            <span>File now · <code>{conflict.currentEtag ?? "unknown"}</code></span>
          </div>
        )}
        {copyPath && <span className="conflict-result">Copy saved · {copyPath}</span>}
        {actionError && <span className="conflict-action-error">Could not complete that action: {actionError}</span>}
        <div className="conflict-actions">
          <button type="button" onClick={() => void onReload()} disabled={working}>
            Discard all drafts &amp; reload Graph
          </button>
          <button type="button" onClick={() => void onSaveCopy()} disabled={working}>
            {action === "saved" ? "Save another copy" : "Save a copy"}
          </button>
          <button type="button" className="is-danger" onClick={() => void onOverwrite()} disabled={working}>
            Overwrite explicitly
          </button>
        </div>
      </div>
    </section>
  );
}

function GraphStatusNotice({
  status,
  migrationState,
  migrationError,
  backupPath,
  onMigrate,
}: {
  status: string;
  migrationState: "idle" | "working";
  migrationError: string | null;
  backupPath: string | null;
  onMigrate: () => Promise<void>;
}) {
  const isFuture = status === "future";
  const isMigrated = status === "ready" && backupPath !== null;
  return (
    <section className={`graph-status-notice ${isFuture ? "is-future" : isMigrated ? "is-migrated" : "is-legacy"}`} role="status" aria-label="Graph overlay status">
      <div className="status-symbol" aria-hidden="true">{isFuture ? "↗" : isMigrated ? "✓" : "↻"}</div>
      <div>
        <p className="section-kicker">GRAPH OVERLAY / {isFuture ? "FUTURE VERSION" : isMigrated ? "MIGRATION COMPLETE" : "MIGRATION AVAILABLE"}</p>
        <strong>{isFuture ? "Graph overlay is read-only" : isMigrated ? "Graph overlay migrated safely" : "Graph overlay needs migration"}</strong>
        <p>
          {isFuture
            ? "This CodexFlow version can display the file but will not save over a newer schema."
            : isMigrated
            ? "The original file was backed up before the schema upgrade."
            : "The file will be backed up before it is upgraded to the current schema."}
        </p>
        {backupPath && <span className="status-result">Backup created · {backupPath}</span>}
        {migrationError && <span className="status-error">Migration failed · {migrationError}</span>}
        {!isFuture && !isMigrated && (
          <button type="button" onClick={() => void onMigrate()} disabled={migrationState === "working"}>
            {migrationState === "working" ? "Migrating…" : "Migrate with backup"}
          </button>
        )}
      </div>
    </section>
  );
}

function ProjectMembershipNotice({
  excluded,
  isGitProject,
}: {
  excluded: ExcludedConversation[];
  isGitProject: boolean;
}) {
  const countLabel = `${excluded.length} conversation${excluded.length === 1 ? "" : "s"} excluded`;
  const boundaryDescription = isGitProject
    ? "Only a resolved cwd inside the selected Project and the same Git worktree is included in the List."
    : "Only a resolved cwd inside the selected Project is included in the List; nested Git repositories are excluded.";
  return (
    <section className="membership-panel" aria-label="Project membership">
      <div className="membership-heading">
        <div>
          <p className="section-kicker">PROJECT BOUNDARY / SOURCE FILTER</p>
          <strong>{countLabel}</strong>
        </div>
        <span className="membership-count">{String(excluded.length).padStart(2, "0")}</span>
      </div>
      <p className="membership-description">
        {boundaryDescription}
      </p>
      <ul className="excluded-list">
        {excluded.map((conversation) => (
          <li key={conversation.id}>
            <div className="excluded-title">
              <code>{conversation.id}</code>
              <strong>{membershipReasonLabel(conversation.reason)}</strong>
            </div>
            <span>cwd · {conversation.cwd}</span>
            {conversation.resolvedCwd && conversation.resolvedCwd !== conversation.cwd && (
              <span>resolved · {conversation.resolvedCwd}</span>
            )}
            {conversation.gitRoot && <span>Git root · {conversation.gitRoot}</span>}
            {conversation.worktreeRoot && <span>Worktree root · {conversation.worktreeRoot}</span>}
          </li>
        ))}
      </ul>
    </section>
  );
}

function membershipReasonLabel(reason: string): string {
  const labels: Record<string, string> = {
    outside_project: "Outside the selected Project",
    nested_git_repository: "Nested Git repository",
    different_git_root: "Different Git root or worktree",
    cwd_not_absolute: "Working directory is not absolute",
    unresolvable_cwd: "Working directory could not be resolved",
    cwd_not_directory: "Working directory is not a directory",
    git_root_unresolvable: "Git root could not be resolved",
  };
  return labels[reason] ?? "Outside the selected Project boundary";
}

function ConversationFilterBar({
  filters,
  tags,
  visibleCount,
  totalCount,
  onChange,
  onReset,
}: {
  filters: ConversationFilters;
  tags: string[];
  visibleCount: number;
  totalCount: number;
  onChange: (changes: Partial<ConversationFilters>) => void;
  onReset: () => void;
}) {
  return (
    <section className="filter-section" aria-label="Conversation filters">
      <div className="filter-heading">
        <div>
          <p className="section-kicker">One projection / three views</p>
          <h2>Filter the field</h2>
        </div>
        <div className="filter-summary" aria-live="polite">
          <strong>{String(visibleCount).padStart(2, "0")}</strong>
          <span>of {totalCount} shown</span>
        </div>
      </div>
      <div className="filter-controls">
        <label className="filter-search">
          <span>Search conversations</span>
          <input
            type="search"
            value={filters.search}
            onChange={(event) => onChange({ search: event.target.value })}
            placeholder="Title, preview, or Conversation ID"
            autoComplete="off"
          />
        </label>
        <FilterSelect
          label="Filter by tag"
          value={filters.tag ?? ""}
          options={[{ value: "", label: "All tags" }, ...tags.map((tag) => ({ value: tag, label: tag }))]}
          onChange={(value) => onChange({ tag: value || null })}
        />
        <FilterSelect
          label="Filter by User status"
          value={filters.userStatus}
          options={[
            { value: "all", label: "All User status" },
            { value: "none", label: "None" },
            { value: "active", label: "Active" },
            { value: "done", label: "Done" },
            { value: "blocked", label: "Blocked" },
          ]}
          onChange={(value) => onChange({ userStatus: value as ConversationFilters["userStatus"] })}
        />
        <FilterSelect
          label="Filter by archived"
          value={filters.archived}
          options={[
            { value: "all", label: "All archive states" },
            { value: "active", label: "Active only" },
            { value: "archived", label: "Archived only" },
          ]}
          onChange={(value) => onChange({ archived: value as ConversationFilters["archived"] })}
        />
        <FilterSelect
          label="Filter by missing"
          value={filters.missing}
          options={[
            { value: "all", label: "All source states" },
            { value: "present", label: "Present only" },
            { value: "missing", label: "Missing only" },
          ]}
          onChange={(value) => onChange({ missing: value as ConversationFilters["missing"] })}
        />
        <FilterSelect
          label="Filter by unlinked"
          value={filters.unlinked}
          options={[
            { value: "all", label: "All link states" },
            { value: "linked", label: "Linked only" },
            { value: "unlinked", label: "Unlinked only" },
          ]}
          onChange={(value) => onChange({ unlinked: value as ConversationFilters["unlinked"] })}
        />
        <FilterSelect
          label="Filter by hidden"
          value={filters.hidden}
          options={[
            { value: "all", label: "All visibility states" },
            { value: "visible", label: "Visible only" },
            { value: "hidden", label: "Hidden only" },
          ]}
          onChange={(value) => onChange({ hidden: value as ConversationFilters["hidden"] })}
        />
        <FilterSelect
          label="Sort conversations"
          value={filters.sortBy}
          options={[
            { value: "source", label: "Source order" },
            { value: "updatedAt", label: "Updated · newest first" },
            { value: "createdAt", label: "Created · newest first" },
          ]}
          onChange={(value) => onChange({ sortBy: value as ConversationFilters["sortBy"] })}
        />
        <button type="button" className="filter-reset" onClick={onReset}>Clear filters</button>
      </div>
      <p className="filter-caption">
        Search and filters keep the source Conversation ID intact. Hidden is a Graph display state; hidden Conversations remain available in List and Timeline.
      </p>
    </section>
  );
}

interface FilterSelectProps {
  label: string;
  value: string;
  options: Array<{ value: string; label: string }>;
  onChange: (value: string) => void;
}

function FilterSelect({ label, value, options, onChange }: FilterSelectProps) {
  return (
    <label>
      <span>{label}</span>
      <select value={value} onChange={(event) => onChange(event.target.value)}>
        {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </select>
    </label>
  );
}

interface ConversationGraphProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
  visibleIds: ReadonlySet<string>;
  selectedId: string | null;
  onSelect: (id: string) => void;
  editing: GraphEditing;
  layoutDrafts: GraphEditingState["layoutDrafts"];
  layoutError: string | null;
  readOnly: boolean;
}

interface GraphPoint {
  x: number;
  y: number;
}

interface PanOrigin extends GraphPoint {
  panX: number;
  panY: number;
}

interface NodeDragState {
  id: string;
  startX: number;
  startY: number;
  origin: GraphPoint;
  current: GraphPoint;
  moved: boolean;
}

function ConversationGraph({
  nodes,
  edges,
  visibleIds,
  selectedId,
  onSelect,
  editing,
  layoutDrafts,
  layoutError,
  readOnly,
}: ConversationGraphProps) {
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState<GraphPoint>({ x: 0, y: 0 });
  const [isPanning, setIsPanning] = useState(false);
  const [draggedPosition, setDraggedPosition] = useState<{ id: string; point: GraphPoint } | null>(null);
  const panOrigin = useRef<PanOrigin | null>(null);
  const nodeDrag = useRef<NodeDragState | null>(null);
  const suppressClick = useRef(false);
  const filteredNodes = useMemo(
    () => nodes.filter((node) => visibleIds.has(node.id)),
    [nodes, visibleIds],
  );
  const visibleNodes = useMemo(() => filteredNodes.filter((node) => !node.hidden), [filteredNodes]);
  const positions = useMemo(
    () => {
      const next = new Map(visibleNodes.map((node, index) => [node.id, graphPosition(node, index)]));
      for (const [id, position] of layoutDrafts) {
        if (next.has(id)) next.set(id, position);
      }
      if (draggedPosition) next.set(draggedPosition.id, draggedPosition.point);
      return next;
    },
    [draggedPosition, visibleNodes, layoutDrafts],
  );
  const visibleEdges = edges.filter((edge) => positions.has(edge.source) && positions.has(edge.target));

  function adjustZoom(delta: number) {
    setZoom((current) => Math.max(0.6, Math.min(1.8, Math.round((current + delta) * 10) / 10)));
  }

  function resetView() {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }

  function beginPan(x: number, y: number) {
    panOrigin.current = { x, y, panX: pan.x, panY: pan.y };
    setIsPanning(true);
  }

  function movePan(x: number, y: number) {
    if (nodeDrag.current) {
      const drag = nodeDrag.current;
      const point = {
        x: drag.origin.x + (x - drag.startX) / zoom,
        y: drag.origin.y + (y - drag.startY) / zoom,
      };
      drag.current = point;
      drag.moved = drag.moved || point.x !== drag.origin.x || point.y !== drag.origin.y;
      setDraggedPosition({ id: drag.id, point });
      return;
    }
    if (!panOrigin.current) return;
    setPan({
      x: panOrigin.current.panX + x - panOrigin.current.x,
      y: panOrigin.current.panY + y - panOrigin.current.y,
    });
  }

  function endPan() {
    const drag = nodeDrag.current;
    if (drag) {
      nodeDrag.current = null;
      panOrigin.current = null;
      setIsPanning(false);
      if (drag.moved) {
        suppressClick.current = true;
        void editing.saveLayout(drag.id, drag.current);
        setDraggedPosition(null);
      } else {
        setDraggedPosition(null);
      }
      return;
    }
    panOrigin.current = null;
    setIsPanning(false);
  }

  return (
    <section className="graph-section" aria-label="Conversation graph">
      <div className="graph-heading">
        <div>
          <p className="section-kicker">Relationship surface / editable nodes</p>
          <h2>Graph</h2>
        </div>
        <div className="graph-toolbar" role="toolbar" aria-label="Graph controls">
          <button type="button" aria-label="Zoom out" onClick={() => adjustZoom(-0.1)}>
            −
          </button>
          <span aria-live="polite">{Math.round(zoom * 100)}%</span>
          <button type="button" aria-label="Zoom in" onClick={() => adjustZoom(0.1)}>
            +
          </button>
          <button type="button" aria-label="Reset view" onClick={resetView}>
            Reset
          </button>
        </div>
      </div>
      <div
        className={`graph-viewport ${isPanning ? "is-panning" : ""}`}
        role="application"
        aria-label="Graph canvas"
        tabIndex={0}
        data-zoom={zoom}
        data-pan-x={pan.x}
        data-pan-y={pan.y}
        onMouseDown={(event) => {
          if (event.button === 0) beginPan(event.clientX, event.clientY);
        }}
        onMouseMove={(event) => movePan(event.clientX, event.clientY)}
        onMouseUp={endPan}
        onMouseLeave={endPan}
      >
        {visibleNodes.length === 0 ? (
          <div className="graph-empty">No visible Conversation nodes in this overlay.</div>
        ) : (
          <div
            className="graph-stage"
            style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})` }}
          >
            <svg className="graph-edges" viewBox="0 0 980 520" aria-hidden="true">
              <defs>
                <marker id="graph-arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
                  <path d="M0,0 L8,4 L0,8 z" />
                </marker>
              </defs>
              {visibleEdges.map((edge) => {
                const source = positions.get(edge.source);
                const target = positions.get(edge.target);
                if (!source || !target) return null;
                return (
                  <line
                    key={edge.id}
                    className="graph-edge"
                    x1={source.x + 92}
                    y1={source.y + 48}
                    x2={target.x + 92}
                    y2={target.y + 48}
                    markerEnd={edge.type === "related_to" ? undefined : "url(#graph-arrow)"}
                  />
                );
              })}
            </svg>
            {visibleNodes.map((node) => {
              const point = positions.get(node.id);
              if (!point) return null;
              return (
                <button
                  key={node.id}
                  type="button"
                  className={`graph-node ${node.missing ? "is-missing" : ""} ${node.id === selectedId ? "is-selected" : ""}`}
                  style={{ left: point.x, top: point.y }}
                  data-conversation-id={node.id}
                  aria-label={`Conversation ${node.displayTitle} (${node.id})`}
                  aria-pressed={node.id === selectedId}
                  onMouseDown={(event) => {
                    event.stopPropagation();
                    if (readOnly) return;
                    suppressClick.current = false;
                    const point = positions.get(node.id);
                    if (!point) return;
                    nodeDrag.current = {
                      id: node.id,
                      startX: event.clientX,
                      startY: event.clientY,
                      origin: point,
                      current: point,
                      moved: false,
                    };
                  }}
                  onClick={() => {
                    if (suppressClick.current) {
                      suppressClick.current = false;
                      return;
                    }
                    onSelect(node.id);
                  }}
                >
                  <span className="graph-node-kind">{node.missing ? "Missing source" : "Conversation"}</span>
                  <strong>{node.displayTitle}</strong>
                  <code>{node.id}</code>
                </button>
              );
            })}
          </div>
        )}
      </div>
      {layoutError && (
        <p className="graph-error" role="alert">
          Could not save node layout: {layoutError}
        </p>
      )}
      <p className="graph-caption">
        {visibleNodes.length} visible node{visibleNodes.length === 1 ? "" : "s"} · drag nodes to save layout · drag the field to pan
      </p>
      <RelationshipEditor
        nodes={nodes}
        edges={edges}
        editing={editing}
        readOnly={readOnly}
      />
    </section>
  );
}

function graphPosition(node: GraphNode, index: number): GraphPoint {
  if (node.layout) return node.layout;
  const column = index % 3;
  const row = Math.floor(index / 3);
  return { x: 32 + column * 292, y: 34 + row * 150 };
}

const BUILT_IN_RELATION_TYPES = [
  "continues",
  "depends_on",
  "implements",
  "reviewed_by",
  "fixes",
  "references",
  "related_to",
] as const;
const CUSTOM_RELATION_VALUE = "__custom__";

interface RelationshipEditorProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
  editing: GraphEditing;
  readOnly: boolean;
}

function RelationshipEditor({ nodes, edges, editing, readOnly }: RelationshipEditorProps) {
  const { value, status: mutationState, error: mutationError } = editing.relationship();
  const editingEdgeId = editing.getState().relationshipId;
  const { source, target, label } = value;
  const builtIn = BUILT_IN_RELATION_TYPES.includes(value.type as (typeof BUILT_IN_RELATION_TYPES)[number]);
  const relationType = builtIn ? value.type : CUSTOM_RELATION_VALUE;
  const customType = builtIn ? "" : value.type;
  const nodeById = useMemo(() => new Map(nodes.map((node) => [node.id, node])), [nodes]);
  const pendingSave = editing.getState().pendingSaves > 0;

  function submitRelationship(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void editing.saveRelationship();
  }

  return (
    <section className="relationship-editor" aria-label="Relationship editor">
      <div className="relationship-heading">
        <div>
          <p className="section-kicker">RELATIONSHIPS / SOURCE → TARGET</p>
          <h3>Relationships</h3>
        </div>
        <span className="relationship-count">{String(edges.length).padStart(2, "0")}</span>
      </div>
      <div className="relationship-layout">
        <form className="relationship-form" onSubmit={submitRelationship}>
          <p className="relationship-form-kicker">{editingEdgeId ? "EDIT RELATIONSHIP" : "ADD RELATIONSHIP"}</p>
          <label htmlFor="relationship-source">Source</label>
          <input
            id="relationship-source"
            list="conversation-id-options"
          value={source}
          onChange={(event) => editing.editRelationship({ ...value, source: event.target.value })}
          placeholder="Conversation ID"
          autoComplete="off"
          disabled={readOnly}
          />

          <label htmlFor="relationship-target">Target</label>
          <input
            id="relationship-target"
            list="conversation-id-options"
          value={target}
          onChange={(event) => editing.editRelationship({ ...value, target: event.target.value })}
          placeholder="Conversation ID"
          autoComplete="off"
          disabled={readOnly}
          />

          <label htmlFor="relationship-type">Relationship type</label>
          <select
            id="relationship-type"
          value={relationType}
          onChange={(event) => editing.editRelationship({ ...value, type: event.target.value === CUSTOM_RELATION_VALUE ? "" : event.target.value })}
          disabled={readOnly}
          >
            {BUILT_IN_RELATION_TYPES.map((type) => (
              <option key={type} value={type}>{type}</option>
            ))}
            <option value={CUSTOM_RELATION_VALUE}>Custom relationship…</option>
          </select>
          {relationType === CUSTOM_RELATION_VALUE && (
            <>
              <label htmlFor="custom-relationship-type">Custom type</label>
              <input
                id="custom-relationship-type"
                value={customType}
                onChange={(event) => editing.editRelationship({ ...value, type: event.target.value })}
                placeholder="e.g. informs"
                autoComplete="off"
                disabled={readOnly}
              />
            </>
          )}

          <label htmlFor="relationship-label">Label <span>(optional)</span></label>
          <input
            id="relationship-label"
            value={label}
            onChange={(event) => editing.editRelationship({ ...value, label: event.target.value })}
            placeholder="Explain the relationship"
            disabled={readOnly}
          />
          <div className="relationship-form-actions">
            <button type="submit" className="relationship-save" disabled={readOnly || pendingSave}>
              {mutationState === "saving" ? "Saving…" : editingEdgeId ? "Save relationship" : "Add relationship"}
            </button>
            {editingEdgeId && (
              <button type="button" className="relationship-cancel" onClick={() => editing.selectRelationship(null)} disabled={readOnly}>
                New relationship
              </button>
            )}
            {editingEdgeId && (
              <button type="button" className="relationship-cancel" onClick={() => editing.discardRelationship()} disabled={readOnly || pendingSave}>
                Cancel edit
              </button>
            )}
          </div>
          {mutationState === "saved" && <span className="relationship-success" role="status">Relationship saved</span>}
          {mutationState === "error" && mutationError && (
            <span className="relationship-error" role="alert">Could not save relationship: {mutationError}</span>
          )}
        </form>

        <div className="relationship-list">
          <div className="relationship-list-heading">
            <span>Saved edges</span>
            <span>Direction is semantic</span>
          </div>
          {edges.length === 0 ? (
            <p className="relationship-empty">No artificial relationships yet.</p>
          ) : (
            <ul>
              {edges.map((edge) => {
                const sourceNode = nodeById.get(edge.source);
                const targetNode = nodeById.get(edge.target);
                const connector = edge.type === "related_to" ? "↔" : "→";
                return (
                  <li key={edge.id} aria-label={`Relationship ${edge.source} ${edge.type} ${edge.target}`}>
                    <div className="relationship-flow">
                      <strong>{sourceNode?.displayTitle ?? edge.source}</strong>
                      <span aria-label={edge.type === "related_to" ? "undirected" : "directed"}>{connector}</span>
                      <strong>{targetNode?.displayTitle ?? edge.target}</strong>
                    </div>
                    <div className="relationship-meta">
                      <span>{edge.type}</span>
                      {edge.label && <span>· {edge.label}</span>}
                    </div>
                    <code>{edge.source} {connector} {edge.target} · {edge.id}</code>
                    <div className="relationship-actions">
                      <button type="button" onClick={() => editing.selectRelationship(edge.id)} disabled={readOnly} aria-label={`Edit relationship ${edge.id}`}>
                        Edit
                      </button>
                      <button type="button" onClick={() => void editing.deleteRelationship(edge.id)} disabled={readOnly || pendingSave} aria-label={`Delete relationship ${edge.id}`}>
                        Delete
                      </button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </div>
      <datalist id="conversation-id-options">
        {nodes.map((node) => <option key={node.id} value={node.id}>{node.displayTitle}</option>)}
      </datalist>
    </section>
  );
}

interface ConversationListProps {
  conversations: Conversation[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

function ConversationList({ conversations, selectedId, onSelect }: ConversationListProps) {
  return (
    <section className="list-section" aria-labelledby="conversation-list-title">
      <div className="list-heading">
        <div>
          <p className="section-kicker">Source snapshot / complete</p>
          <h2 id="conversation-list-title">Conversation List</h2>
        </div>
        <div className="list-count">
          <strong>{String(conversations.length).padStart(2, "0")}</strong>
          <span>THREADS FOUND</span>
        </div>
      </div>

      {conversations.length === 0 ? (
        <div className="empty-panel">No local conversations belong to this Project yet.</div>
      ) : (
        <div className="table-wrap">
          <table aria-label="Conversation list">
            <thead>
              <tr>
                <th>Conversation</th>
                <th>Observed</th>
                <th>Working directory</th>
                <th>Source</th>
                <th>State</th>
              </tr>
            </thead>
            <tbody>
              {conversations.map((conversation) => (
                <ConversationRow
                  key={conversation.id}
                  conversation={conversation}
                  selected={conversation.id === selectedId}
                  onSelect={onSelect}
                />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

interface ConversationRowProps {
  conversation: Conversation;
  selected: boolean;
  onSelect: (id: string) => void;
}

function ConversationRow({ conversation, selected, onSelect }: ConversationRowProps) {
  const { codex } = conversation;
  return (
    <tr
      className={selected ? "is-selected" : ""}
      aria-selected={selected}
      data-conversation-id={conversation.id}
      tabIndex={0}
      onClick={() => onSelect(conversation.id)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onSelect(conversation.id);
        }
      }}
    >
      <td>
        <div className="conversation-title">{conversation.displayTitle}</div>
        <code>{conversation.id}</code>
      </td>
      <td>
          <div className="time-stack">
            <time dateTime={codex.createdAt ?? undefined}>{formatTimestamp(codex.createdAt)}</time>
            <span>to {formatTimestamp(codex.updatedAt)}</span>
            {!conversation.derived.validObservationRange && (
              <strong className="time-warning">Invalid observation range</strong>
            )}
          </div>
      </td>
      <td><span className="cwd-value" title={codex.cwd}>{codex.cwd || "—"}</span></td>
      <td><span className="source-tag">{codex.source}</span></td>
      <td>
        {conversation.derived.missing ? (
          <span className="missing-tag">Missing source</span>
        ) : codex.archived ? (
          <span className="archive-tag">Archived</span>
        ) : (
          <span className="active-tag">Active</span>
        )}
      </td>
    </tr>
  );
}

function ConversationDetail({ conversation, editing, readOnly }: {
  conversation: Conversation | null;
  editing: GraphEditing;
  readOnly: boolean;
}) {
  if (!conversation) return null;
  const { value: draft, status: saveState, error: saveError } = editing.conversation(conversation.id);
  const conversationId = conversation.id;
  function updateDraft(next: ConversationDraft) { editing.editConversation(conversationId, next); }
  function saveChanges(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void editing.saveConversation(conversationId);
  }

  return (
    <form className="detail-panel" role="region" aria-label="Conversation detail" onSubmit={saveChanges}>
      <span className="ribbon-label">SELECTED CONVERSATION</span>
      <strong>{conversation.displayTitle}</strong>
      <code>{conversation.id}</code>
      {conversation.derived.missing && <span className="missing-detail">Source record is no longer available.</span>}
      <span className="detail-source">{conversation.codex.cwd || "No source working directory"}</span>

      <label htmlFor="conversation-title">Custom title</label>
      <input
        id="conversation-title"
        value={draft.title}
        onChange={(event) => updateDraft({ ...draft, title: event.target.value })}
        placeholder={conversation.codex.title ?? "Uses the Codex title"}
        disabled={readOnly}
      />

      <label htmlFor="conversation-tags">Tags</label>
      <input
        id="conversation-tags"
        value={draft.tags}
        onChange={(event) => updateDraft({ ...draft, tags: event.target.value })}
        placeholder="design, release, research"
        disabled={readOnly}
      />

      <label htmlFor="conversation-status">User status</label>
      <select
        id="conversation-status"
        value={draft.status}
        onChange={(event) => updateDraft({ ...draft, status: event.target.value as UserStatus })}
        disabled={readOnly}
      >
        <option value="none">None</option>
        <option value="active">Active</option>
        <option value="done">Done</option>
        <option value="blocked">Blocked</option>
      </select>

      <label htmlFor="conversation-note">Note</label>
      <textarea
        id="conversation-note"
        value={draft.note}
        onChange={(event) => updateDraft({ ...draft, note: event.target.value })}
        rows={4}
        placeholder="Add context only you own."
        disabled={readOnly}
      />

      <label className="detail-checkbox" htmlFor="conversation-hidden">
        <input
          id="conversation-hidden"
          type="checkbox"
          checked={draft.hidden}
          onChange={(event) => updateDraft({ ...draft, hidden: event.target.checked })}
          disabled={readOnly}
        />
        <span>Hidden from Graph</span>
      </label>

      {conversation.overlay.layout && (
        <span className="detail-layout">
          Layout · x {formatLayoutValue(conversation.overlay.layout.x)} / y {formatLayoutValue(conversation.overlay.layout.y)}
        </span>
      )}
      {saveState === "saved" && <span className="detail-success" role="status">Changes saved</span>}
      {saveState === "error" && saveError && (
        <span className="detail-error" role="alert">Could not save changes: {saveError}</span>
      )}
      <button type="submit" className="detail-save" disabled={readOnly || editing.getState().pendingSaves > 0}>
        {saveState === "saving" ? "Saving…" : "Save changes"}
      </button>
    </form>
  );
}

function formatLayoutValue(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(1);
}

function formatTimestamp(timestamp: string | null): string {
  if (!timestamp) return "—";
  return `${timestamp.slice(0, 16).replace("T", " ")} UTC`;
}
