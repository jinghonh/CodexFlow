import { useEffect, useMemo, useState } from "react";

import { ApiError, createApi } from "./api";
import type { DashboardApi } from "./api";
import type { Conversation, DashboardSnapshot, HealthResponse, ProjectView } from "./types";

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
          </div>
          <span className="ribbon-meta">{project.isGitProject ? "GIT PROJECT" : "LOCAL DIRECTORY"}</span>
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

      {snapshot && !sourceUnavailable && (
        <ConversationList
          conversations={snapshot.conversations}
          selectedId={selectedConversationId}
          onSelect={setSelectedConversationId}
        />
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
        {codex.archived ? <span className="archive-tag">Archived</span> : <span className="active-tag">Active</span>}
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
