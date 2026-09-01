import { useEffect, useMemo, useRef, useState } from "react";

import { ApiError, createApi } from "./api";
import type { DashboardApi } from "./api";
import type {
  Conversation,
  DashboardSnapshot,
  ExcludedConversation,
  GraphEdge,
  GraphNode,
  HealthResponse,
  ProjectView,
} from "./types";

interface AppProps {
  api?: DashboardApi;
}

type LoadState = "idle" | "loading" | "ready" | "error";

export function App({ api }: AppProps) {
  const apiClient = useMemo(() => api ?? createApi(), [api]);
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [projectPath, setProjectPath] = useState("");
  const [project, setProject] = useState<ProjectView | null>(null);
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [loadState, setLoadState] = useState<LoadState>("idle");
  const [error, setError] = useState<ApiError | Error | null>(null);
  const [selectedConversationId, setSelectedConversationId] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    void apiClient
      .health()
      .then((response) => {
        if (active) setHealth(response);
      })
      .catch((reason: unknown) => {
        if (active) setError(asError(reason));
      });
    return () => {
      active = false;
    };
  }, [apiClient]);

  async function handleProjectSelect(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmedPath = projectPath.trim();
    if (!trimmedPath) {
      setError(new Error("Choose a Project root first."));
      return;
    }
    setLoadState("loading");
    setError(null);
    setSnapshot(null);
    setSelectedConversationId(null);
    try {
      const selected = await apiClient.selectProject(trimmedPath);
      setProject(selected.project);
      const loaded = await apiClient.snapshot();
      setSnapshot(loaded);
      setLoadState("ready");
    } catch (reason: unknown) {
      setLoadState("error");
      setError(asError(reason));
    }
  }

  const sourceUnavailable =
    (error instanceof ApiError && error.status === 503) ||
    snapshot?.source.status === "unavailable" ||
    snapshot?.source.status === "incompatible";
  const sourceStale = snapshot?.source.status === "stale";
  const runtimeLabel = health ? "Local runtime ready" : error && !project ? "Local runtime unavailable" : "Checking local runtime";

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
            <button type="submit" disabled={loadState === "loading"}>
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
          </div>
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
            <p>The last complete source snapshot is still shown; refresh could not confirm newer data.</p>
          </div>
        </div>
      )}

      {snapshot && !sourceUnavailable && snapshot.excludedConversations.length > 0 && (
        <ProjectMembershipNotice
          excluded={snapshot.excludedConversations}
          isGitProject={project?.isGitProject ?? snapshot.project.isGitProject}
        />
      )}

      {snapshot && !sourceUnavailable && (
        <>
          <ConversationGraph
            nodes={snapshot.graph.nodes}
            edges={snapshot.graph.edges}
            selectedId={selectedConversationId}
            onSelect={setSelectedConversationId}
          />
          <ConversationList
            conversations={snapshot.conversations}
            selectedId={selectedConversationId}
            onSelect={setSelectedConversationId}
          />
        </>
      )}

      {selectedConversationId && snapshot && (
        <ConversationDetail
          conversation={snapshot.conversations.find(({ id }) => id === selectedConversationId) ?? null}
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

interface ConversationGraphProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

interface GraphPoint {
  x: number;
  y: number;
}

interface PanOrigin extends GraphPoint {
  panX: number;
  panY: number;
}

function ConversationGraph({ nodes, edges, selectedId, onSelect }: ConversationGraphProps) {
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState<GraphPoint>({ x: 0, y: 0 });
  const [isPanning, setIsPanning] = useState(false);
  const panOrigin = useRef<PanOrigin | null>(null);
  const visibleNodes = useMemo(() => nodes.filter((node) => !node.hidden), [nodes]);
  const positions = useMemo(
    () => new Map(visibleNodes.map((node, index) => [node.id, graphPosition(node, index)])),
    [visibleNodes],
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
    if (!panOrigin.current) return;
    setPan({
      x: panOrigin.current.panX + x - panOrigin.current.x,
      y: panOrigin.current.panY + y - panOrigin.current.y,
    });
  }

  function endPan() {
    panOrigin.current = null;
    setIsPanning(false);
  }

  return (
    <section className="graph-section" aria-label="Conversation graph">
      <div className="graph-heading">
        <div>
          <p className="section-kicker">Relationship surface / read only</p>
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
                  aria-label={`Conversation ${node.displayTitle} (${node.id})`}
                  aria-pressed={node.id === selectedId}
                  onMouseDown={(event) => event.stopPropagation()}
                  onClick={() => onSelect(node.id)}
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
      <p className="graph-caption">
        {visibleNodes.length} visible node{visibleNodes.length === 1 ? "" : "s"} · drag the field to pan · source metadata stays upstream
      </p>
    </section>
  );
}

function graphPosition(node: GraphNode, index: number): GraphPoint {
  if (node.layout) return node.layout;
  const column = index % 3;
  const row = Math.floor(index / 3);
  return { x: 32 + column * 292, y: 34 + row * 150 };
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

function ConversationDetail({ conversation }: { conversation: Conversation | null }) {
  if (!conversation) return null;
  return (
    <aside className="detail-panel" role="region" aria-label="Conversation detail">
      <span className="ribbon-label">SELECTED CONVERSATION</span>
      <strong>{conversation.displayTitle}</strong>
      <code>{conversation.id}</code>
      {conversation.derived.missing && <span className="missing-detail">Source record is no longer available.</span>}
      <span>{conversation.codex.cwd}</span>
    </aside>
  );
}

function formatTimestamp(timestamp: string | null): string {
  if (!timestamp) return "—";
  return `${timestamp.slice(0, 16).replace("T", " ")} UTC`;
}

function asError(reason: unknown): ApiError | Error {
  if (reason instanceof ApiError || reason instanceof Error) return reason;
  return new Error("Unexpected local runtime error.");
}
