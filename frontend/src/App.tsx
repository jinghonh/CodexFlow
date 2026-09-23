import { useEffect, useMemo, useRef, useState } from "react";

import { ApiError, createApi } from "./api";
import type { DashboardApi } from "./api";
import {
  DEFAULT_CONVERSATION_FILTERS,
  filterConversations,
  isDefaultConversationFilters,
} from "./filters";
import { Workspace } from "./Workspace";
import { automaticPositions, edgeGeometry, relationNames, statusNames } from "./graphGeometry";
import { TimelineView } from "./Timeline";
import { useGraphEditing } from "./graphEditing";
import type { ConversationDraft, GraphConflict, GraphEditing, GraphEditingState } from "./graphEditing";
import type {
  Conversation,
  ConversationFilters,
  GraphEdge,
  GraphNode,
  HealthResponse,
  UserStatus,
} from "./types";

interface AppProps {
  api?: DashboardApi;
}

const RECENT_PROJECTS_KEY = "codexflow.recent-projects.v1";

function readRecentProjects(): string[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(RECENT_PROJECTS_KEY) ?? "[]");
    return Array.isArray(value)
      ? value.filter((path): path is string => typeof path === "string" && path.startsWith("/")).slice(0, 5)
      : [];
  } catch {
    return [];
  }
}

export function App({ api }: AppProps) {
  const apiClient = useMemo(() => api ?? createApi(), [api]);
  const [state, editing] = useGraphEditing(apiClient);
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [healthError, setHealthError] = useState<Error | null>(null);
  const [projectPath, setProjectPath] = useState("");
  const [recentProjects, setRecentProjects] = useState(readRecentProjects);
  const [pickingDirectory, setPickingDirectory] = useState(false);
  const [pickerError, setPickerError] = useState<string | null>(null);
  const restoredProject = useRef(false);
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
      if (active) setHealthError(reason instanceof Error ? reason : new Error("本地服务发生意外错误。"));
    });
    return () => { active = false; };
  }, [apiClient]);

  useEffect(() => {
    if (!health || restoredProject.current) return;
    restoredProject.current = true;
    if (project) return;
    const path = recentProjects[0] ?? health.project?.realPath;
    if (path) {
      setProjectPath(path);
      void editing.requestProject(path);
    }
  }, [health, editing, project, recentProjects]);

  useEffect(() => {
    if (!project) return;
    setProjectPath(project.realPath);
    setRecentProjects((current) => {
      const next = [project.realPath, ...current.filter((path) => path !== project.realPath)].slice(0, 5);
      try { localStorage.setItem(RECENT_PROJECTS_KEY, JSON.stringify(next)); } catch { /* 禁用存储时仍可选择项目。 */ }
      return next;
    });
  }, [project?.realPath]);

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
    setPickerError(null);
    if (projectPath.trim()) void editing.requestProject(projectPath);
    else void handlePickDirectory();
  }

  async function handlePickDirectory() {
    setPickerError(null);
    setPickingDirectory(true);
    try {
      const { path } = await apiClient.pickProjectDirectory();
      if (path) {
        setProjectPath(path);
        void editing.requestProject(path);
      }
    } catch (reason) {
      setPickerError(reason instanceof Error ? reason.message : "无法打开目录选择窗口。");
    } finally {
      setPickingDirectory(false);
    }
  }

  const sourceUnavailable =
    ((error instanceof ApiError && error.status === 503) && !snapshot) ||
    snapshot?.source.status === "unavailable" ||
    snapshot?.source.status === "incompatible";
  const sourceStale = snapshot?.source.status === "stale";
  const graphFileStatus = snapshot?.graph.fileStatus ?? project?.graphFileStatus;
  const graphReadOnly = editing.readOnly;
  const runtimeLabel = health ? "本地服务已就绪" : error && !project ? "本地服务不可用" : "正在检查本地服务";
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
          <p className="eyebrow">CODEXFLOW / 本地任务索引</p>
          <h1>任务关系工作台</h1>
        </div>
        <div className={`runtime-chip ${health ? "is-ready" : ""}`}>
          <span className="status-dot" aria-hidden="true" />
          <span>{runtimeLabel}</span>
        </div>
      </header>

      <section className="hero-grid" aria-labelledby="hero-title">
        <div className="hero-copy">
          <p className="section-kicker">看清任务之间的联系</p>
          <h2 id="hero-title">连接任务<br />理解进展。</h2>
          <p className="hero-description">
            选择本地项目，查看任务关系、时间分布与工作记录。
          </p>
        </div>
        <form className="project-card" onSubmit={handleProjectSelect}>
          <div className="card-index">01 <span>/ PROJECT ROOT</span></div>
          <label htmlFor="project-path">项目目录</label>
          <div className="path-input-row">
            <input
              id="project-path"
              value={projectPath}
              onChange={(event) => setProjectPath(event.target.value)}
              placeholder="/Users/you/Code/project"
              autoComplete="off"
            />
            <button type="submit" disabled={state.phase !== "idle"}>
              {loadState === "loading" ? "加载中…" : "加载项目"}
            </button>
          </div>
          <div className="project-shortcuts">
            <button type="button" className="directory-picker-button" onClick={() => void handlePickDirectory()} disabled={pickingDirectory || state.phase !== "idle"}>
              {pickingDirectory ? "等待选择…" : "选择目录…"}
            </button>
            {recentProjects.length > 0 && (
              <label className="recent-project-control">
                <span>最近项目</span>
                <select value="" onChange={(event) => {
                  if (!event.target.value) return;
                  setProjectPath(event.target.value);
                  setPickerError(null);
                  void editing.requestProject(event.target.value);
                }} disabled={state.phase !== "idle"}>
                  <option value="">选择最近项目</option>
                  {recentProjects.map((path) => <option key={path} value={path}>{path}</option>)}
                </select>
              </label>
            )}
          </div>
          {pickerError && <p className="picker-error" role="alert">{pickerError}</p>}
          <p className="card-footnote">仅在本机读取项目。</p>
        </form>
      </section>

      {project && (
        <div className="project-ribbon">
          <div>
            <span className="ribbon-label">当前项目</span>
            <strong>{project.realPath}</strong>
            {project.originalPath !== project.realPath && (
              <span className="project-alias">别名 · {project.originalPath}</span>
            )}
          </div>
          <div className="ribbon-facts">
            <span className="ribbon-meta">{project.isGitProject ? "版本库项目" : "本地目录"}</span>
            {project.gitRoot && <span>版本库根目录 · {project.gitRoot}</span>}
            {project.worktreeRoot && <span>工作树根目录 · {project.worktreeRoot}</span>}
            <span>关系数据 · {({ absent: "尚未创建", ready: "可用", legacy: "需要迁移", future: "较新版本", corrupt: "文件损坏" } as Record<string, string>)[snapshot?.project.graphFileStatus ?? project.graphFileStatus] ?? "未知状态"}</span>
          </div>
          {project && (
            <div className="ribbon-actions">
              <span className={`draft-status ${dirtyDraftCount > 0 ? "is-dirty" : ""}`}>
                {dirtyDraftCount > 0
                  ? "存在未保存修改"
                  : snapshot
                  ? "来源同步就绪"
                  : "来源加载需要重试"}
              </span>
              <button
                type="button"
                className="refresh-button"
                onClick={() => void editing.requestRefresh()}
                disabled={state.phase !== "idle"}
              >
                {state.phase === "refreshing" ? "刷新中…" : "刷新来源"}
              </button>
              {state.refreshError && <span className="refresh-error" role="alert">刷新失败 · {state.refreshError}</span>}
            </div>
          )}
        </div>
      )}

      {loadState === "loading" && (
        <div className="loading-panel" role="status" aria-live="polite">
          <span className="loading-mark" aria-hidden="true">↗</span>
          <span>正在加载任务…</span>
        </div>
      )}

      {sourceUnavailable && (
        <div className="source-alert" role="alert">
          <div className="alert-symbol" aria-hidden="true">!</div>
          <div>
            <strong>来源不可用</strong>
            <p>{error?.message ?? snapshot?.source.error?.message ?? "Codex 来源服务尚不可用。"}</p>
            <span className="alert-note">不会将不完整的列表当作完整快照展示。</span>
          </div>
        </div>
      )}

      {sourceStale && !sourceUnavailable && (
        <div className="source-stale" role="status" aria-live="polite">
          <span className="stale-mark" aria-hidden="true">↻</span>
          <div>
            <strong>来源数据已过期</strong>
            <p>
              {snapshot.source.error?.message ?? "保留上次完整快照，刷新未能确认最新数据。"}
            </p>
            <span className="alert-note">已保留上次完整快照，可在来源恢复后重试刷新。</span>
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

      {snapshot && !sourceUnavailable && (
        <>
          <Workspace panels={{
            graph: (<ConversationGraph
            key={project?.realPath}
            nodes={snapshot.graph.nodes}
            conversations={snapshot.conversations}
            edges={snapshot.graph.edges}
            visibleIds={filteredConversationIds}
            editing={editing}
            layoutDrafts={state.layoutDrafts}
            layoutError={state.layoutError}
            selectedId={selectedConversationId}
            onSelect={setSelectedConversationId}
            readOnly={graphReadOnly}
          />),
            timeline: (<TimelineView
            timeline={snapshot.timeline}
            conversations={filteredConversations}
            selectedId={selectedConversationId}
            loading={timelineLoading || state.phase !== "idle" || state.pendingSaves > 0}
            error={timelineError}
            onSelect={setSelectedConversationId}
            onOptionsChange={(options) => editing.changeTimeline(options)}
          />),
            list: (<ConversationList
            conversations={filteredConversations}
            filters={filters}
            tags={availableTags}
            totalCount={snapshot.conversations.length}
            onFilterChange={(changes) => setFilters((current) => ({ ...current, ...changes }))}
            onFilterReset={() => setFilters(DEFAULT_CONVERSATION_FILTERS)}
            selectedId={selectedConversationId}
            onSelect={setSelectedConversationId}
          />),
          }} detail={selectedConversationId ? <>
            <button className="detail-close" onClick={() => setSelectedConversationId(null)}>关闭详情 ×</button>
            <ConversationDetail conversation={snapshot.conversations.find(({ id }) => id === selectedConversationId) ?? null} editing={editing} readOnly={graphReadOnly} />
          </> : null} />
        </>
      )}

      {loadState === "error" && !sourceUnavailable && error && (
        <div className="source-alert" role="alert">
          <div className="alert-symbol" aria-hidden="true">!</div>
          <div>
            <strong>无法加载项目</strong>
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
  const nextAction = switchingProject ? "切换项目" : "刷新";
  return (
    <div className="refresh-dialog-backdrop">
      <section className="refresh-dialog" role="dialog" aria-modal="true" aria-labelledby="refresh-dialog-title">
        <p className="section-kicker">刷新来源 / 未保存修改</p>
        <h2 id="refresh-dialog-title">如何处理未保存的修改？</h2>
        <p>
          共 {dirtyCount} 项未保存修改。请先选择如何处理，再{nextAction}。
        </p>
        <ul className="draft-summary" aria-label="未保存修改">
          {drafts.map((draft) => <li key={draft.key}>{draft.label}</li>)}
        </ul>
        {error && <p className="refresh-dialog-error" role="alert">保存失败 · {error} 修改仍保留，尚未保存。</p>}
        <div className="refresh-dialog-actions">
          <button type="button" className="refresh-dialog-save" onClick={onSave} disabled={working}>
            {action === "saving" ? "保存中…" : `保存并${nextAction}`}
          </button>
          <button type="button" className="refresh-dialog-discard" onClick={onDiscard} disabled={working}>
            {working && action !== "saving" ? "加载中…" : `丢弃修改并${nextAction}`}
          </button>
          <button type="button" className="refresh-dialog-cancel" onClick={onCancel} disabled={working}>
            取消
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
    <section className="graph-conflict-panel" role="alert" aria-label="关系数据冲突">
      <div className="conflict-symbol" aria-hidden="true">!</div>
      <div className="conflict-copy">
        <p className="section-kicker">保存冲突 / 外部修改</p>
        <strong>关系数据已被其他程序修改</strong>
        <p>{conflict.message}</p>
        <span className="conflict-note">
          已保留全部 {drafts.length} 项修改。保存副本和覆盖保存包含当前修改，之后的编辑仍需保存。
        </span>
        {(conflict.expectedEtag || conflict.currentEtag) && (
          <div className="conflict-version" aria-label="版本对比">
            <span>修改基于版本 · <code>{conflict.expectedEtag ?? "未知"}</code></span>
            <span>当前文件版本 · <code>{conflict.currentEtag ?? "未知"}</code></span>
          </div>
        )}
        {copyPath && <span className="conflict-result">副本已保存 · {copyPath}</span>}
        {actionError && <span className="conflict-action-error">无法完成操作: {actionError}</span>}
        <div className="conflict-actions">
          <button type="button" onClick={() => void onReload()} disabled={working}>
            丢弃全部修改并重新加载
          </button>
          <button type="button" onClick={() => void onSaveCopy()} disabled={working}>
            {action === "saved" ? "另存一个副本" : "保存副本"}
          </button>
          <button type="button" className="is-danger" onClick={() => void onOverwrite()} disabled={working}>
            覆盖保存
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
    <section className={`graph-status-notice ${isFuture ? "is-future" : isMigrated ? "is-migrated" : "is-legacy"}`} role="status" aria-label="关系数据状态">
      <div className="status-symbol" aria-hidden="true">{isFuture ? "↗" : isMigrated ? "✓" : "↻"}</div>
      <div>
        <p className="section-kicker">关系数据 / {isFuture ? "较新版本" : isMigrated ? "迁移完成" : "可以迁移"}</p>
        <strong>{isFuture ? "关系数据只读" : isMigrated ? "关系数据已安全迁移" : "关系数据需要迁移"}</strong>
        <p>
          {isFuture
            ? "当前版本可以显示该文件，但不能覆盖较新格式的数据。"
            : isMigrated
            ? "升级前已备份原始文件。"
            : "升级数据格式前会先备份文件。"}
        </p>
        {backupPath && <span className="status-result">备份已创建 · {backupPath}</span>}
        {migrationError && <span className="status-error">迁移失败 · {migrationError}</span>}
        {!isFuture && !isMigrated && (
          <button type="button" onClick={() => void onMigrate()} disabled={migrationState === "working"}>
            {migrationState === "working" ? "正在迁移…" : "备份并迁移"}
          </button>
        )}
      </div>
    </section>
  );
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
    <section className="filter-section" aria-label="任务筛选">
      <div className="filter-heading">
        <div>
          <p className="section-kicker">三个视图 · 同步筛选</p>
          <h2>筛选任务</h2>
        </div>
        <div className="filter-summary" aria-live="polite">
          <strong>{String(visibleCount).padStart(2, "0")}</strong>
          <span>/ {totalCount} 个任务</span>
        </div>
      </div>
      <div className="filter-controls">
        <label className="filter-search">
          <span>搜索任务</span>
          <input
            type="search"
            value={filters.search}
            onChange={(event) => onChange({ search: event.target.value })}
            placeholder="标题、摘要或任务标识"
            autoComplete="off"
          />
        </label>
        <FilterSelect
          label="标签"
          value={filters.tag ?? ""}
          options={[{ value: "", label: "全部标签" }, ...tags.map((tag) => ({ value: tag, label: tag }))]}
          onChange={(value) => onChange({ tag: value || null })}
        />
        <FilterSelect
          label="任务状态"
          value={filters.userStatus}
          options={[
            { value: "all", label: "全部任务状态" },
            { value: "none", label: "未设置" },
            { value: "active", label: "进行中" },
            { value: "done", label: "已完成" },
            { value: "blocked", label: "阻塞" },
          ]}
          onChange={(value) => onChange({ userStatus: value as ConversationFilters["userStatus"] })}
        />
        <FilterSelect
          label="归档状态"
          value={filters.archived}
          options={[
            { value: "all", label: "全部归档状态" },
            { value: "active", label: "仅未归档" },
            { value: "archived", label: "仅已归档" },
          ]}
          onChange={(value) => onChange({ archived: value as ConversationFilters["archived"] })}
        />
        <FilterSelect
          label="排序方式"
          value={filters.sortBy}
          options={[
            { value: "source", label: "来源顺序" },
            { value: "updatedAt", label: "最近更新优先" },
            { value: "createdAt", label: "最近创建优先" },
          ]}
          onChange={(value) => onChange({ sortBy: value as ConversationFilters["sortBy"] })}
        />
        <button type="button" className="filter-reset" onClick={onReset}>清空筛选</button>
      </div>
      <details className="filter-more">
        <summary>更多筛选条件{filters.missing !== "all" || filters.unlinked !== "all" || filters.hidden !== "all" ? " · 已启用" : ""}</summary>
        <div className="filter-extra-controls">
          <FilterSelect
            label="来源可用性"
            value={filters.missing}
            options={[
              { value: "all", label: "全部来源状态" },
              { value: "present", label: "来源存在" },
              { value: "missing", label: "来源缺失" },
            ]}
            onChange={(value) => onChange({ missing: value as ConversationFilters["missing"] })}
          />
          <FilterSelect
            label="关系状态"
            value={filters.unlinked}
            options={[
              { value: "all", label: "全部关系状态" },
              { value: "linked", label: "已有关系" },
              { value: "unlinked", label: "没有关系" },
            ]}
            onChange={(value) => onChange({ unlinked: value as ConversationFilters["unlinked"] })}
          />
          <FilterSelect
            label="图中可见性"
            value={filters.hidden}
            options={[
              { value: "all", label: "全部可见状态" },
              { value: "visible", label: "图中可见" },
              { value: "hidden", label: "图中隐藏" },
            ]}
            onChange={(value) => onChange({ hidden: value as ConversationFilters["hidden"] })}
          />
        </div>
      </details>
      <p className="filter-caption">
        隐藏仅影响关系图，任务仍可在列表和时间线中查看。
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
  conversations: Conversation[];
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
  conversations,
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
  const initiallyFitted = useRef(false);
  const [manual, setManual] = useState(false);
  const viewport = useRef<HTMLDivElement>(null);
  const automatic = useMemo(() => automaticPositions(nodes, edges), [nodes, edges]);
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
      const next = new Map(visibleNodes.map((node, index) => [node.id, manual ? graphPosition(node, nodes.indexOf(node)) : automatic.get(node.id)!]));
      for (const [id, position] of manual ? layoutDrafts : []) {
        if (next.has(id)) next.set(id, position);
      }
      if (draggedPosition) next.set(draggedPosition.id, draggedPosition.point);
      return next;
    },
    [draggedPosition, visibleNodes, layoutDrafts, manual, automatic, nodes],
  );
  const visibleEdges = edges.filter((edge) => positions.has(edge.source) && positions.has(edge.target));

  function adjustZoom(delta: number) {
    setZoom((current) => Math.max(0.05, Math.min(2.5, Math.round((current + delta) * 10) / 10)));
  }

  function fitView(selectedOnly = false) {
    const points = selectedOnly && selectedId && positions.has(selectedId) ? [positions.get(selectedId)!] : [...positions.values()];
    if (!points.length || !viewport.current) return;
    const minX = Math.min(...points.map(p => p.x)), minY = Math.min(...points.map(p => p.y));
    const width = Math.max(...points.map(p => p.x)) - minX + 220;
    const height = Math.max(...points.map(p => p.y)) - minY + 116;
    const box = viewport.current.getBoundingClientRect();
    const next = Math.max(.05, Math.min(1, (box.width - 64) / width, (box.height - 64) / height));
    setZoom(next); setPan({ x: (box.width - width * next) / 2 - minX * next, y: (box.height - height * next) / 2 - minY * next });
  }
  function resetView() { fitView(); }
  useEffect(() => {
    if (!initiallyFitted.current && viewport.current && viewport.current.getBoundingClientRect().width > 0 && positions.size) {
      initiallyFitted.current = true; fitView();
    }
  }, [positions]);

  const fitCurrentView = useRef(fitView);
  fitCurrentView.current = fitView;
  useEffect(() => {
    if (!viewport.current || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => { if (viewport.current!.clientWidth > 0) fitCurrentView.current(); });
    observer.observe(viewport.current);
    return () => observer.disconnect();
  }, [manual]);

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
      drag.moved = drag.moved || Math.hypot(x - drag.startX, y - drag.startY) >= 4;
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
    <section className="graph-section" aria-label="任务关系图">
      <div className="graph-heading">
        <div>
          <p className="section-kicker">关联分组 · 人工关系</p>
          <h2>关系图</h2>
        </div>
        <div className="graph-toolbar" role="toolbar" aria-label="关系图操作">
          <button aria-pressed={!manual} onClick={() => setManual(false)}>自动布局</button>
          <button aria-pressed={manual} onClick={() => setManual(true)}>手动布局</button>
          <button disabled={!selectedId || !positions.has(selectedId)} onClick={() => fitView(true)}>定位选中</button>
          <button disabled={!selectedId || readOnly} onClick={() => { editing.selectRelationship(null); editing.editRelationship({ source: selectedId!, target: "", type: "related_to", label: "" }); document.getElementById("relationship-target")?.focus(); }}>从选中任务建立关系</button>
          <button type="button" aria-label="缩小" onClick={() => adjustZoom(-0.1)}>
            −
          </button>
          <span aria-live="polite">{Math.round(zoom * 100)}%</span>
          <button type="button" aria-label="放大" onClick={() => adjustZoom(0.1)}>
            +
          </button>
          <button type="button" aria-label="显示全部" onClick={resetView}>
            显示全部
          </button>
        </div>
      </div>
      <div
        ref={viewport}
        className={`graph-viewport ${isPanning ? "is-panning" : ""}`}
        role="application"
        aria-label="关系画布"
        tabIndex={0}
        data-zoom={zoom}
        data-pan-x={pan.x}
        data-pan-y={pan.y}
        onPointerDown={(event) => {
          if (event.button === 0) { event.currentTarget.setPointerCapture?.(event.pointerId); beginPan(event.clientX, event.clientY); }
        }}
        onPointerMove={(event) => movePan(event.clientX, event.clientY)}
        onPointerUp={endPan}
        onPointerCancel={endPan}
      >
        {visibleNodes.length === 0 ? (
          <div className="graph-empty">没有可见任务，请调整筛选或隐藏状态。</div>
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
                const siblings = visibleEdges.filter(item => [edge.source, edge.target].sort().join("|") === [item.source, item.target].sort().join("|"));
                const offset = (siblings.indexOf(edge) - (siblings.length - 1) / 2) * 90 * (edge.source < edge.target ? 1 : -1);
                const geometry = edgeGeometry(source, target, offset);
                return <g key={edge.id}>
                  <path className={`graph-edge ${edge.type === "related_to" ? "is-related" : ""}`} d={geometry.path} fill="none" markerEnd={edge.type === "related_to" ? undefined : "url(#graph-arrow)"} />
                  <text className="graph-edge-label" x={geometry.label.x} y={geometry.label.y - 8} textAnchor="middle">{relationNames[edge.type] ?? edge.type}<title>{edge.label || relationNames[edge.type] || edge.type}</title></text>
                </g>;
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
                  aria-label={`任务 ${node.displayTitle} (${node.id})`}
                  aria-pressed={node.id === selectedId}
                  onPointerDown={(event) => {
                    event.stopPropagation();
                    if (readOnly || !manual || event.button !== 0) return;
                    event.currentTarget.setPointerCapture?.(event.pointerId);
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
                  <span className="graph-node-kind">{node.missing ? "来源缺失" : "任务"}</span>
                  <strong title={node.displayTitle}>{node.displayTitle}</strong>
                  <span className="node-state">{statusNames[conversations.find(c => c.id === node.id)?.overlay.status ?? "none"]} · {conversations.find(c => c.id === node.id)?.overlay.tags.join(" · ") || "无标签"}</span>
                  <code>{node.id}</code>
                </button>
              );
            })}
          </div>
        )}
      </div>
      {layoutError && (
        <p className="graph-error" role="alert">
          无法保存节点位置: {layoutError}
        </p>
      )}
      <p className="graph-caption">
        {visibleNodes.length} 个可见任务 · {manual ? "拖动节点保存位置" : "自动按关联成组，筛选保留位置"} · 拖动空白区域平移
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
    <section className="relationship-editor" aria-label="关系编辑器">
      <div className="relationship-heading">
        <div>
          <p className="section-kicker">人工关系 / 起点 → 终点</p>
          <h3>任务关系</h3>
        </div>
        <span className="relationship-count">{String(edges.length).padStart(2, "0")}</span>
      </div>
      <div className="relationship-layout">
        <form className="relationship-form" onSubmit={submitRelationship}>
          <p className="relationship-form-kicker">{editingEdgeId ? "编辑关系" : "添加关系"}</p>
          <label htmlFor="relationship-source">来源</label>
          <input
            id="relationship-source"
            list="conversation-id-options"
          value={source}
          onChange={(event) => editing.editRelationship({ ...value, source: event.target.value })}
          placeholder="任务标识"
          autoComplete="off"
          disabled={readOnly}
          />

          <label htmlFor="relationship-target">目标</label>
          <input
            id="relationship-target"
            list="conversation-id-options"
          value={target}
          onChange={(event) => editing.editRelationship({ ...value, target: event.target.value })}
          placeholder="任务标识"
          autoComplete="off"
          disabled={readOnly}
          />

          <label htmlFor="relationship-type">关系类型</label>
          <select
            id="relationship-type"
          value={relationType}
          onChange={(event) => editing.editRelationship({ ...value, type: event.target.value === CUSTOM_RELATION_VALUE ? "" : event.target.value })}
          disabled={readOnly}
          >
            {BUILT_IN_RELATION_TYPES.map((type) => (
              <option key={type} value={type}>{relationNames[type] ?? type}</option>
            ))}
            <option value={CUSTOM_RELATION_VALUE}>自定义关系…</option>
          </select>
          {relationType === CUSTOM_RELATION_VALUE && (
            <>
              <label htmlFor="custom-relationship-type">自定义类型</label>
              <input
                id="custom-relationship-type"
                value={customType}
                onChange={(event) => editing.editRelationship({ ...value, type: event.target.value })}
                placeholder="例如：提供背景"
                autoComplete="off"
                disabled={readOnly}
              />
            </>
          )}

          <label htmlFor="relationship-label">说明 <span>（可选）</span></label>
          <input
            id="relationship-label"
            value={label}
            onChange={(event) => editing.editRelationship({ ...value, label: event.target.value })}
            placeholder="描述这条关系"
            disabled={readOnly}
          />
          <div className="relationship-form-actions">
            <button type="submit" className="relationship-save" disabled={readOnly || pendingSave}>
              {mutationState === "saving" ? "保存中…" : editingEdgeId ? "保存关系" : "添加关系"}
            </button>
            {editingEdgeId && (
              <button type="button" className="relationship-cancel" onClick={() => editing.selectRelationship(null)} disabled={readOnly}>
                新建关系
              </button>
            )}
            {editingEdgeId && (
              <button type="button" className="relationship-cancel" onClick={() => editing.discardRelationship()} disabled={readOnly || pendingSave}>
                取消编辑
              </button>
            )}
          </div>
          {mutationState === "saved" && <span className="relationship-success" role="status">关系已保存</span>}
          {mutationState === "error" && mutationError && (
            <span className="relationship-error" role="alert">无法保存关系: {mutationError}</span>
          )}
        </form>

        <div className="relationship-list">
          <div className="relationship-list-heading">
            <span>已保存关系</span>
            <span>箭头表示关系方向</span>
          </div>
          {edges.length === 0 ? (
            <p className="relationship-empty">还没有人工关系，可从选中任务开始建立。</p>
          ) : (
            <ul>
              {edges.map((edge) => {
                const sourceNode = nodeById.get(edge.source);
                const targetNode = nodeById.get(edge.target);
                const connector = edge.type === "related_to" ? "↔" : "→";
                return (
                  <li key={edge.id} aria-label={`关系 ${edge.source} ${edge.type} ${edge.target}`}>
                    <div className="relationship-flow">
                      <strong>{sourceNode?.displayTitle ?? edge.source}</strong>
                      <span aria-label={edge.type === "related_to" ? "无向关系" : "有向关系"}>{connector}</span>
                      <strong>{targetNode?.displayTitle ?? edge.target}</strong>
                    </div>
                    <div className="relationship-meta">
                      <span>{relationNames[edge.type] ?? edge.type}</span>
                      {edge.label && <span>· {edge.label}</span>}
                    </div>
                    <code>{edge.source} {connector} {edge.target} · {edge.id}</code>
                    <div className="relationship-actions">
                      <button type="button" onClick={() => editing.selectRelationship(edge.id)} disabled={readOnly} aria-label={`编辑关系 ${edge.id}`}>
                        Edit
                      </button>
                      <button type="button" onClick={() => void editing.deleteRelationship(edge.id)} disabled={readOnly || pendingSave} aria-label={`删除关系 ${edge.id}`}>
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
  filters: ConversationFilters;
  tags: string[];
  totalCount: number;
  onFilterChange: (changes: Partial<ConversationFilters>) => void;
  onFilterReset: () => void;
  selectedId: string | null;
  onSelect: (id: string) => void;
}

function ConversationList({ conversations, filters, tags, totalCount, onFilterChange, onFilterReset, selectedId, onSelect }: ConversationListProps) {
  return (
    <section className="list-section" aria-labelledby="conversation-list-title">
      <div className="list-heading">
        <div>
          <p className="section-kicker">当前来源快照</p>
          <h2 id="conversation-list-title">任务列表</h2>
        </div>
        <div className="list-count">
          <strong>{String(conversations.length).padStart(2, "0")}</strong>
          <span>个任务</span>
        </div>
      </div>

      <ConversationFilterBar
        filters={filters}
        tags={tags}
        visibleCount={conversations.length}
        totalCount={totalCount}
        onChange={onFilterChange}
        onReset={onFilterReset}
      />

      {conversations.length === 0 ? (
        <div className="empty-panel">没有匹配的任务，请检查项目或筛选条件。</div>
      ) : (
        <div className="table-wrap">
          <table aria-label="任务列表">
            <thead>
              <tr>
                <th>任务</th>
                <th>观测区间</th>
                <th>工作目录</th>
                <th>来源</th>
                <th>状态</th>
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
        <div className="conversation-title" title={conversation.displayTitle}>{conversation.displayTitle}</div><span className="task-metadata">{statusNames[conversation.overlay.status]} · {conversation.overlay.tags.join(" · ") || "无标签"}</span>
        <code>{conversation.id}</code>
      </td>
      <td>
          <div className="time-stack">
            <time dateTime={codex.createdAt ?? undefined}>{formatTimestamp(codex.createdAt)}</time>
            <span>至 {formatTimestamp(codex.updatedAt)}</span>
            {!conversation.derived.validObservationRange && (
              <strong className="time-warning">无效的观测区间</strong>
            )}
          </div>
      </td>
      <td><span className="cwd-value" title={codex.cwd}>{codex.cwd || "—"}</span></td>
      <td><span className="source-tag">{codex.source}</span></td>
      <td>
        {conversation.derived.missing ? (
          <span className="missing-tag">来源缺失</span>
        ) : codex.archived ? (
          <span className="archive-tag">已归档</span>
        ) : (
          <span className="active-tag">未归档</span>
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
    <form className="detail-panel" role="region" aria-label="任务详情" onSubmit={saveChanges}>
      <span className="ribbon-label">任务详情</span>
      <strong>{conversation.displayTitle}</strong>
      <code>{conversation.id}</code>
      {conversation.derived.missing && <span className="missing-detail">来源记录已不可用。</span>}
      <span className="detail-source">{conversation.codex.cwd || "没有来源工作目录"}</span>

      <label htmlFor="conversation-title">自定义标题</label>
      <input
        id="conversation-title"
        value={draft.title}
        onChange={(event) => updateDraft({ ...draft, title: event.target.value })}
        placeholder={conversation.codex.title ?? "默认使用来源标题"}
        disabled={readOnly}
      />

      <label htmlFor="conversation-tags">标签</label>
      <input
        id="conversation-tags"
        value={draft.tags}
        onChange={(event) => updateDraft({ ...draft, tags: event.target.value })}
        placeholder="设计, 发布, 调研"
        disabled={readOnly}
      />

      <label htmlFor="conversation-status">任务状态</label>
      <select
        id="conversation-status"
        value={draft.status}
        onChange={(event) => updateDraft({ ...draft, status: event.target.value as UserStatus })}
        disabled={readOnly}
      >
        <option value="none">未设置</option>
        <option value="active">进行中</option>
        <option value="done">已完成</option>
        <option value="blocked">阻塞</option>
      </select>

      <label htmlFor="conversation-note">备注</label>
      <textarea
        id="conversation-note"
        value={draft.note}
        onChange={(event) => updateDraft({ ...draft, note: event.target.value })}
        rows={4}
        placeholder="记录补充说明。"
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
        <span>在关系图中隐藏</span>
      </label>

      {conversation.overlay.layout && (
        <span className="detail-layout">
          位置 · x {formatLayoutValue(conversation.overlay.layout.x)} / y {formatLayoutValue(conversation.overlay.layout.y)}
        </span>
      )}
      {saveState === "saved" && <span className="detail-success" role="status">修改已保存</span>}
      {saveState === "error" && saveError && (
        <span className="detail-error" role="alert">无法保存修改: {saveError}</span>
      )}
      <button type="submit" className="detail-save" disabled={readOnly || editing.getState().pendingSaves > 0}>
        {saveState === "saving" ? "保存中…" : "保存修改"}
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
