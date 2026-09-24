import React, { useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { ProjectGraphView } from "./ProjectGraphView";
import { CandidatePreviewView } from "./CandidatePreviewView";
import { ProjectAnalysisView } from "./ProjectAnalysisView";
import { ProjectTimelineView } from "./ProjectTimelineView";
import { ProjectWorkstreamsView } from "./ProjectWorkstreamsView";
import { ThreadHistoryView } from "./ThreadHistoryView";
import { formatAppError } from "./appError";
import { ProjectThreadDetailsView } from "./ProjectThreadDetailsView";
import "./style.css";

type Theme = "system" | "light" | "dark";
type Capability = { state: "available" | "unavailable" | "notVerified"; detail: string };
type AppError = { code: string; message: string; retryable: boolean; cachePreserved: boolean; backend: string; nextStep?: string };
type SourceStatus = {
  selectedBinary: string | null;
  resolvedBinary: string | null;
  version: string | null;
  connection: "notChecked" | "connecting" | "connected" | "failed";
  capabilities: {
    metadata: Capability;
    history: Capability;
    experimentalHistory: Capability;
    codexSummary: Capability;
    codexNaming: Capability;
  };
  error: AppError | null;
  checkedAtUnixMs: number | null;
};
type Settings = { theme: Theme; source: SourceStatus };
type JevStatus = { config: { baseUrl: string; model: string }; credentialConfigured: boolean; credentialError: AppError | null };
type JevConnectionResult = { models: string[]; requestedModel: string };
type JevInferenceResult = {
  requestedModel: string;
  actualModel: string;
  answer: { choice: "resolved" | "unresolved"; confidence: number; probabilities: { resolved: number; unresolved: number } };
  inputTokens: number;
  outputTokens: number;
};
type Thread = {
  id: string; sessionId: string; title: string | null; preview: string; cwd: string;
  projectId: string | null; sourceKind: string; sourceDetail: string | null;
  threadSource: string | null; parentThreadId: string | null; forkedFromId: string | null;
  git: { branch: string | null; sha: string | null; originUrl: string | null } | null;
  createdAt: number; updatedAt: number; archived: boolean; metadataComplete: boolean;
  turnsComplete: boolean; itemsComplete: boolean; missingFromSource: boolean;
  contentComplete: boolean; readError: string | null; observedAtUnixMs: number;
};
type Scope = { archived: boolean; complete: boolean; attemptedAtUnixMs: number | null; completedAtUnixMs: number | null; error: string | null };
type SessionList = { threads: Thread[]; scopes: Scope[] };
type Project = { id: string; name: string; root: string; gitCommonDir: string | null };
type Attribution = { threadId: string; projectId: string | null; workspaceRoot: string | null; basis: string; detail: string; diagnostic: string | null; sourceProjectId: string | null };
type AttributedThread = { thread: Thread; attribution: Attribution };
type ProjectCatalog = { projects: Project[]; selectedProjectId: string | null; recentProjectIds: string[]; unassigned: AttributedThread[]; scopes: Scope[] };
type ProjectSessions = { project: Project; workspaces: string[]; threads: AttributedThread[]; scopes: Scope[] };
type Workstream = { id: string; name: string; members: string[] };
type WorkstreamView = { workstreams: Workstream[]; ungroupedThreadIds: string[]; revision: number };
type ThreadMatches = { matches: { threadId: string; summary: string | null }[]; total: number };
type ThreadMatchesState = { queryKey: string; value: ThreadMatches };
type IndexRun = { id: string; projectId: string | null; state: "queued" | "running" | "complete" | "partial" | "failed" | "cancelled"; startedAtUnixMs: number; finishedAtUnixMs: number | null; pagesSaved: number; threadsSeen: number; error: AppError | null; interrupted: boolean };
const runLabels: Record<IndexRun["state"], string> = { queued: "待执行", running: "执行中", complete: "完成", partial: "部分完成", failed: "失败", cancelled: "已取消" };

const labels: { key: keyof SourceStatus["capabilities"]; title: string; number: string }[] = [
  { key: "metadata", title: "会话元数据", number: "01" },
  { key: "history", title: "历史读取", number: "02" },
  { key: "experimentalHistory", title: "实验性分页", number: "03" },
  { key: "codexSummary", title: "Codex 总结", number: "04" },
  { key: "codexNaming", title: "工作流命名", number: "05" },
];

function errorText(error: unknown): string {
  return formatAppError(error, "操作失败。请检查桌面应用状态后重试。");
}

export function App() {
  const [source, setSource] = useState<SourceStatus | null>(null);
  const [path, setPath] = useState("");
  const [theme, setTheme] = useState<Theme>("system");
  const [busy, setBusy] = useState(true);
  const [pageError, setPageError] = useState("");
  const [projectCatalog, setProjectCatalog] = useState<ProjectCatalog | null>(null);
  const [projectSessions, setProjectSessions] = useState<ProjectSessions | null>(null);
  const [graphVersion, setGraphVersion] = useState(0);
  const refreshGraphForAnalysis = React.useCallback(() => setGraphVersion((version) => version + 1), []);
  const [projectPath, setProjectPath] = useState("");
  const [projectError, setProjectError] = useState("");
  const [showUnassigned, setShowUnassigned] = useState(false);
  const [jevStatus, setJevStatus] = useState<JevStatus | null>(null);
  const [jevBaseUrl, setJevBaseUrl] = useState("https://api.typesafe.ai");
  const [jevModel, setJevModel] = useState("jev-latest");
  const [jevKey, setJevKey] = useState("");
  const [jevSaving, setJevSaving] = useState(false);
  const [jevRequestBusy, setJevRequestBusy] = useState(false);
  const [jevDeleting, setJevDeleting] = useState(false);
  const [jevError, setJevError] = useState("");
  const [jevConnection, setJevConnection] = useState<JevConnectionResult | null>(null);
  const [jevInference, setJevInference] = useState<JevInferenceResult | null>(null);
  const [analysisSettingsRevision, setAnalysisSettingsRevision] = useState(0);
  const jevEpoch = useRef(0);
  const [listError, setListError] = useState("");
  const [indexRun, setIndexRun] = useState<IndexRun | null>(null);
  const refreshing = indexRun?.state === "queued" || indexRun?.state === "running";
  const [query, setQuery] = useState("");
  const [workstreamFilter, setWorkstreamFilter] = useState("");
  const [workspaceFilter, setWorkspaceFilter] = useState("");
  const [archiveFilter, setArchiveFilter] = useState("all");
  const [completeFilter, setCompleteFilter] = useState("all");
  const [viewMode, setViewMode] = useState<"timeline" | "graph">("timeline");
  const [relationSource, setRelationSource] = useState("all");
  const [relationKind, setRelationKind] = useState("all");
  const [minimumConfidence, setMinimumConfidence] = useState(0.7);
  const [workstreams, setWorkstreams] = useState<WorkstreamView | null>(null);
  const [workstreamError, setWorkstreamError] = useState("");
  const [threadMatches, setThreadMatches] = useState<ThreadMatchesState | null>(null);
  const [threadLimit, setThreadLimit] = useState(40);
  const [queryError, setQueryError] = useState<{ queryKey: string; message: string } | null>(null);
  const [analysisState, setAnalysisState] = useState<string | null>(null);
  const [selectedThreadId, setSelectedThreadId] = useState<string | null>(null);
  const [evidenceLocation, setEvidenceLocation] = useState<{ threadId: string; turnId: string; itemId: string; nonce: number } | null>(null);
  const evidenceNonce = useRef(0);
  const projectEpoch = useRef(0);
  const workstreamProjectId = useRef<string | null>(null);

  useEffect(() => {
    if (selectedThreadId) document.getElementById("thread-detail")?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, [selectedThreadId]);

  useEffect(() => {
    const projectId = projectSessions?.project.id;
    if (!projectId || showUnassigned) {
      workstreamProjectId.current = null;
      setWorkstreams(null);
      setWorkstreamError("");
      return;
    }
    let active = true;
    if (workstreamProjectId.current !== projectId) {
      workstreamProjectId.current = projectId;
      setWorkstreams(null);
    }
    invoke<WorkstreamView>("get_project_workstreams", { projectId })
      .then((value) => { if (active) { setWorkstreams(value); setWorkstreamError(""); } })
      .catch((error) => { if (active) setWorkstreamError(errorText(error)); });
    return () => { active = false; };
  }, [projectSessions?.project.id, showUnassigned, graphVersion]);

  useEffect(() => {
    const projectId = projectSessions?.project.id;
    if (!projectId || showUnassigned) { setThreadMatches(null); setQueryError(null); return; }
    let active = true;
    const queryKey = JSON.stringify([projectId, showUnassigned, query, workstreamFilter, workspaceFilter, archiveFilter, completeFilter, workstreams?.revision ?? null]);
    setQueryError(null);
    const timer = window.setTimeout(() => {
      invoke<ThreadMatches>("query_project_threads", { projectId, query: {
        text: query, workstreamId: workstreamFilter || null, workspaceRoot: workspaceFilter || null,
        archived: archiveFilter === "all" ? null : archiveFilter === "archived",
        complete: completeFilter === "all" ? null : completeFilter === "complete",
      } }).then((value) => { if (active) { setThreadMatches({ queryKey, value }); setQueryError(null); } })
        .catch((error) => { if (active) setQueryError({ queryKey, message: errorText(error) }); });
    }, 180);
    return () => { active = false; window.clearTimeout(timer); };
  }, [projectSessions?.project.id, showUnassigned, query, workstreamFilter, workspaceFilter, archiveFilter, completeFilter, graphVersion, workstreams?.revision]);

  function selectEvidence(evidence: { threadId: string; turnId: string; itemId: string }) {
    setSelectedThreadId(evidence.threadId);
    setEvidenceLocation({ threadId: evidence.threadId, turnId: evidence.turnId, itemId: evidence.itemId, nonce: ++evidenceNonce.current });
  }

  function recordRun(run: IndexRun | null) {
    setIndexRun((previous) => {
      if (!run) return previous;
      if (previous && run.startedAtUnixMs < previous.startedAtUnixMs) return previous;
      if (previous?.id === run.id && previous.finishedAtUnixMs !== null && run.finishedAtUnixMs === null) return previous;
      return run;
    });
  }

  async function loadProjects() {
    const epoch = projectEpoch.current;
    const catalog = await invoke<ProjectCatalog>("get_project_catalog");
    const sessions = catalog.selectedProjectId
      ? await invoke<ProjectSessions>("get_project_sessions", { projectId: catalog.selectedProjectId })
      : null;
    if (epoch !== projectEpoch.current) return;
    setProjectCatalog(catalog);
    setProjectSessions(sessions);
    setGraphVersion((version) => version + 1);
  }

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    listen<IndexRun>("index-run", (event) => {
      if (!active) return;
      recordRun(event.payload);
      void loadProjects().catch((error) => setListError(errorText(error)));
    }).then((stop) => { if (active) unlisten = stop; else stop(); }).catch(() => {});
    invoke<Settings>("get_settings")
      .then(async (settings) => {
        if (!active) return;
        let selectedProjectId: string | null = null;
        try {
          const catalog = await invoke<ProjectCatalog>("get_project_catalog");
          if (active) {
            setProjectCatalog(catalog);
            selectedProjectId = catalog.selectedProjectId;
            if (catalog.selectedProjectId) setProjectSessions(await invoke<ProjectSessions>("get_project_sessions", { projectId: catalog.selectedProjectId }));
            setGraphVersion((version) => version + 1);
          }
        } catch (error) { if (active) setListError(errorText(error)); }
        if (!active) return;
        recordRun(await invoke<IndexRun | null>("get_latest_index_run"));
        setTheme(settings.theme);
        setPath(settings.source.selectedBinary ?? "");
        setSource(settings.source);
        setBusy(false);
        const next = await invoke<SourceStatus>("connect_source", { selectedBinary: settings.source.selectedBinary });
        if (active) setSource(next);
        if (active && next.connection === "connected" && selectedProjectId) await startRefresh(selectedProjectId);
      })
      .catch((error) => { if (active) setPageError(errorText(error)); })
      .finally(() => { if (active) setBusy(false); });
    const timer = window.setInterval(() => {
      invoke<SourceStatus>("get_source_status").then((next) => { if (active) setSource(next); }).catch(() => {});
      invoke<IndexRun | null>("get_latest_index_run").then((run) => { if (active) recordRun(run); }).catch(() => {});
    }, 4000);
    return () => { active = false; unlisten?.(); window.clearInterval(timer); };
  }, []);

  useEffect(() => {
    const projectId = projectSessions?.project.id;
    if (!projectId) { setAnalysisState(null); return; }
    let active = true;
    let unlisten: (() => void) | undefined;
    invoke<{ state: string } | null>("get_latest_analysis_run", { projectId })
      .then((run) => { if (active) setAnalysisState(run?.state ?? null); }).catch(() => {});
    listen<{ projectId: string; state: string }>("analysis-run", (event) => {
      if (active && event.payload.projectId === projectId) setAnalysisState(event.payload.state);
    }).then((stop) => { if (active) unlisten = stop; else stop(); }).catch(() => {});
    return () => { active = false; unlisten?.(); };
  }, [projectSessions?.project.id]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    listen<{ state: string }>("analysis-run", (event) => {
      if (active && ["paused", "complete", "partial", "failed", "cancelled"].includes(event.payload.state)) {
        setGraphVersion((version) => version + 1);
      }
    }).then((stop) => { if (active) unlisten = stop; else stop(); }).catch(() => {});
    return () => { active = false; unlisten?.(); };
  }, []);

  useEffect(() => {
    invoke<JevStatus>("get_jev_status").then((status) => {
      setJevStatus(status);
      setJevBaseUrl(status.config.baseUrl);
      setJevModel(status.config.model);
      setAnalysisSettingsRevision((revision) => revision + 1);
    }).catch((error) => setJevError(errorText(error)));
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  async function connect(value = path) {
    setBusy(true);
    setPageError("");
    try {
      const next = await invoke<SourceStatus>("connect_source", { selectedBinary: value.trim() || null });
      setSource(next);
      setPath(value);
      if (next.connection === "connected" && projectCatalog?.selectedProjectId) await startRefresh(projectCatalog.selectedProjectId);
    } catch (error) { setPageError(errorText(error)); }
    finally { setBusy(false); }
  }

  async function startRefresh(projectId: string | null) {
    setListError("");
    try {
      const queued = await invoke<IndexRun>("start_index_run", { projectId });
      recordRun(await invoke<IndexRun>("get_index_run", { id: queued.id }));
    }
    catch (error) {
      const latest = await invoke<IndexRun | null>("get_latest_index_run").catch(() => null);
      if (latest?.state === "queued" || latest?.state === "running") recordRun(latest);
      else setListError(errorText(error));
    }
  }

  async function cancelRefresh() {
    if (!indexRun || !refreshing) return;
    try { await invoke<IndexRun>("cancel_index_run", { id: indexRun.id }); }
    catch (error) { setListError(errorText(error)); }
  }

  async function browse() {
    try {
      const selected = await open({ multiple: false, directory: false, title: "选择 Codex 可执行文件" });
      if (typeof selected === "string") setPath(selected);
    } catch (error) { setPageError(errorText(error)); }
  }

  async function changeTheme(value: Theme) {
    try {
      await invoke<Theme>("set_display_theme", { theme: value });
      setTheme(value);
    } catch (error) { setPageError(errorText(error)); }
  }

  async function selectDirectory() {
    try {
      const selected = await open({ multiple: false, directory: true, title: "选择本地项目目录" });
      if (typeof selected === "string") setProjectPath(selected);
    } catch (error) { setProjectError(errorText(error)); }
  }

  async function chooseProject() {
    if (!projectPath.trim()) return;
    projectEpoch.current += 1;
    try {
      const catalog = await invoke<ProjectCatalog>("choose_project", { path: projectPath.trim() });
      const sessions = catalog.selectedProjectId
        ? await invoke<ProjectSessions>("get_project_sessions", { projectId: catalog.selectedProjectId })
        : null;
      projectEpoch.current += 1;
      setProjectCatalog(catalog);
      setProjectSessions(sessions);
      setQuery(""); setWorkstreamFilter(""); setWorkspaceFilter(""); setArchiveFilter("all"); setCompleteFilter("all"); setSelectedThreadId(null);
      setGraphVersion((version) => version + 1);
      setShowUnassigned(false);
      setProjectError("");
      await startRefresh(catalog.selectedProjectId);
    } catch (error) { setProjectError(errorText(error)); }
  }

  async function chooseExistingProject(projectId: string) {
    projectEpoch.current += 1;
    try {
      const catalog = await invoke<ProjectCatalog>("choose_existing_project", { projectId });
      const sessions = await invoke<ProjectSessions>("get_project_sessions", { projectId });
      projectEpoch.current += 1;
      setProjectCatalog(catalog);
      setProjectSessions(sessions);
      setQuery(""); setWorkstreamFilter(""); setWorkspaceFilter(""); setArchiveFilter("all"); setCompleteFilter("all"); setSelectedThreadId(null);
      setGraphVersion((version) => version + 1);
      setShowUnassigned(false);
      setProjectError("");
      await startRefresh(projectId);
    } catch (error) { setProjectError(errorText(error)); }
  }

  async function saveJev() {
    jevEpoch.current += 1;
    setJevSaving(true);
    setJevError("");
    try {
      const next = await invoke<JevStatus>("save_jev_settings", {
        baseUrl: jevBaseUrl, model: jevModel, apiKey: jevKey || null,
      });
      setJevStatus(next);
      setJevBaseUrl(next.config.baseUrl);
      setJevModel(next.config.model);
      setJevKey("");
      setJevConnection(null);
      setJevInference(null);
      setAnalysisSettingsRevision((revision) => revision + 1);
    } catch (error) { setJevError(errorText(error)); }
    finally { setJevSaving(false); }
  }

  async function refreshJevStatus() {
    try {
      setJevStatus(await invoke<JevStatus>("get_jev_status"));
      setAnalysisSettingsRevision((revision) => revision + 1);
      setJevError("");
    } catch (error) { setJevError(errorText(error)); }
  }

  async function deleteJev() {
    if (!window.confirm("删除钥匙串中的 Jev API Key？已有本地结果会保留。")) return;
    jevEpoch.current += 1;
    setJevDeleting(true);
    setJevError("");
    try {
      setJevStatus(await invoke<JevStatus>("delete_jev_credential"));
      setJevKey("");
      setJevConnection(null);
      setJevInference(null);
      setAnalysisSettingsRevision((revision) => revision + 1);
    } catch (error) { setJevError(errorText(error)); }
    finally { setJevDeleting(false); }
  }

  async function runJev(kind: "connection" | "inference") {
    const epoch = jevEpoch.current;
    setJevRequestBusy(true);
    setJevError("");
    if (kind === "connection") setJevConnection(null);
    else setJevInference(null);
    try {
      if (kind === "connection") {
        const result = await invoke<JevConnectionResult>("check_jev_connection");
        if (epoch === jevEpoch.current) setJevConnection(result);
      } else {
        const result = await invoke<JevInferenceResult>("test_jev_inference");
        if (epoch === jevEpoch.current) setJevInference(result);
      }
    } catch (error) { if (epoch === jevEpoch.current) setJevError(errorText(error)); }
    finally { setJevRequestBusy(false); }
  }

  async function cancelJev() {
    const epoch = ++jevEpoch.current;
    try {
      await invoke("cancel_jev_request");
      if (epoch === jevEpoch.current) setJevError("Jev 本地请求已取消；远端计算或计费可能仍在进行。");
    } catch (error) { if (epoch === jevEpoch.current) setJevError(errorText(error)); }
  }

  const connected = source?.connection === "connected";
  const failed = source?.connection === "failed";
  const checkedAt = source?.checkedAtUnixMs ? new Date(source.checkedAtUnixMs).toLocaleString("zh-CN") : "尚未检查";
  const jevBusy = jevSaving || jevRequestBusy || jevDeleting;
  const jevUnsaved = !jevStatus || jevBaseUrl !== jevStatus.config.baseUrl ||
    jevModel !== jevStatus.config.model || jevKey.length > 0;
  const scopes = projectSessions?.scopes ?? projectCatalog?.scopes ?? [];
  const attempted = scopes.some((scope) => scope.attemptedAtUnixMs !== null);
  const complete = scopes.length === 2 && scopes.every((scope) => scope.complete);
  const threadQueryKey = JSON.stringify([projectSessions?.project.id ?? null, showUnassigned, query, workstreamFilter, workspaceFilter, archiveFilter, completeFilter, workstreams?.revision ?? null]);
  useEffect(() => { setThreadLimit(40); }, [threadQueryKey]);
  const currentThreadMatches = threadMatches?.queryKey === threadQueryKey ? threadMatches.value : null;
  const currentQueryError = queryError?.queryKey === threadQueryKey ? queryError.message : "";
  const visibleIds = React.useMemo(() => new Set(currentThreadMatches?.matches.map((item) => item.threadId) ?? []), [currentThreadMatches]);
  const visibleThreads = (showUnassigned ? projectCatalog?.unassigned ?? [] : projectSessions?.threads ?? []).filter(({ thread }) =>
    showUnassigned ? [thread.title, thread.preview, thread.id].some((value) => value?.toLowerCase().includes(query.toLowerCase())) : visibleIds.has(thread.id)
  );
  const shownThreads = visibleThreads.slice(0, threadLimit);
  const selectedThread = (showUnassigned ? projectCatalog?.unassigned ?? [] : projectSessions?.threads ?? [])
    .find(({ thread }) => thread.id === selectedThreadId)?.thread;
  const selectedAttribution = (showUnassigned ? projectCatalog?.unassigned ?? [] : projectSessions?.threads ?? [])
    .find(({ thread }) => thread.id === selectedThreadId)?.attribution;
  const selectionHidden = !!selectedThread && (showUnassigned || !!currentThreadMatches) && !visibleThreads.some(({ thread }) => thread.id === selectedThread.id);
  const clearFilters = () => { setQuery(""); setWorkstreamFilter(""); setWorkspaceFilter(""); setArchiveFilter("all"); setCompleteFilter("all"); setRelationSource("all"); setRelationKind("all"); setMinimumConfidence(0.7); };
  const projectState = !projectSessions ? "首次使用：请选择本地项目并连接数据源。"
    : refreshing ? "读取中：正在刷新来源；已有缓存仍可查看。"
    : indexRun?.state === "failed" ? "读取失败：已保存缓存仍可查看，请检查来源并重试。"
    : projectSessions.threads.length === 0 ? attempted ? "无会话：当前项目没有已索引会话。" : "首次使用：此项目尚未采集，连接来源后刷新。"
    : !complete ? "来源不完整：部分历史或列表未取得，继续查看已保存事实。"
    : ["queued", "running", "cancelling"].includes(analysisState ?? "") ? "分析中：事实和已有结果仍可查看。"
    : analysisState === "failed" ? "分析失败：事实与此前有效结果仍可查看，可继续未完成项。"
    : !connected ? "缓存可能过期：来源当前不可用，显示上次保存的结果。"
    : !analysisState ? "未分析：来源事实已可查看，模型分析需手动启动。" : "来源事实与已保存分析可查看。";
  const projects = [...(projectCatalog?.projects ?? [])].sort((a, b) => {
    const recent = projectCatalog?.recentProjectIds ?? [];
    const aIndex = recent.indexOf(a.id);
    const bIndex = recent.indexOf(b.id);
    return (aIndex < 0 ? 1000 : aIndex) - (bIndex < 0 ? 1000 : bIndex) || a.name.localeCompare(b.name);
  });

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><span className="brand-mark">C<span>F</span></span><div><strong>CodexFlow</strong><small>本地工作过程</small></div></div>
      <div className="side-group"><span className="side-caption">工作空间</span><div className="side-link active"><span className="side-dot" />来源与分析连接 <span className="side-index">01</span></div><a className="side-link" href="#projects"><span className="side-dot" />本地项目 <span className="side-index">02</span></a><a className="side-link" href="#sessions"><span className="side-dot" />项目会话 <span className="side-index">03</span></a><a className="side-link" href="#workstreams"><span className="side-dot" />工作流 <span className="side-index">04</span></a><a className="side-link" href="#relations"><span className="side-dot" />关系图 <span className="side-index">05</span></a></div>
      <div className="side-note"><span className="side-note-line" />同一仓库的主工作区与 worktree 合并展示。会话的实际工作区和归属依据仍可逐条查看。</div>
      <div className="sidebar-bottom"><span className="sidebar-bottom-symbol">↗</span><div>本机运行<br /><strong>数据留在你的设备</strong></div></div>
    </aside>

    <main className="content">
      <header className="topbar"><span>设置 / 连接</span><div className="topbar-right"><span className="topbar-pulse" />本地桌面应用</div></header>
      <div className="page-body">
        <div className="eyebrow">SOURCE / 01 <span /></div>
        <div className="page-heading"><div><h1>连接 Codex 数据源<span className="accent">.</span></h1><p>确认应用实际使用的二进制，以及可以安全读取的能力。</p></div><div className="heading-badge">本机连接诊断<br /><strong>不会启动模型</strong></div></div>

        <section className="status-banner" data-state={failed ? "failed" : connected ? "connected" : "pending"} aria-live="polite">
          <div className="status-icon">{failed ? "!" : connected ? "✓" : "·"}</div>
          <div><span className="status-caption">连接状态</span><strong>{busy ? "正在检查来源…" : failed ? "连接失败" : connected ? "已连接到 app-server" : "等待连接"}</strong><small>{failed ? source?.error?.message : connected ? "初始化与基础来源能力已完成检查" : "选择二进制并开始诊断"}</small></div>
          <span className="status-time">{checkedAt}</span>
        </section>

        {pageError && <div className="page-error" role="alert">{pageError}</div>}
        {source?.error && <div className="error-detail" role="alert"><span className="error-code">{source.error.code}</span><span>{source.error.retryable ? "可重试。" : "需先修正原因。"} {source.error.cachePreserved ? "已有缓存保留。" : "请重新确认缓存状态。"} {source.error.nextStep}</span></div>}

        <div className="columns">
          <section className="panel choose-panel">
            <div className="panel-kicker">01 / 选择来源</div>
            <h2>Codex 可执行文件</h2>
            <p className="panel-intro">选择你实际使用的 <code>codex</code>。留空时从应用可见的 <code>PATH</code> 查找。</p>
            <label htmlFor="binary">二进制路径或命令名称</label>
            <div className="path-row"><input id="binary" spellCheck={false} value={path} onChange={(event) => setPath(event.target.value)} placeholder="codex 或 /绝对路径/codex" /><button className="browse-button" onClick={browse} disabled={busy}>浏览…</button></div>
            <div className="action-row"><button className="primary-button" onClick={() => connect()} disabled={busy}>{busy ? "正在诊断…" : connected ? "重新连接" : "保存并诊断"}<span>↗</span></button><button className="plain-button" disabled={busy} onClick={() => { setPath(""); void connect(""); }}>使用系统命令</button></div>
            <div className="path-details"><div><span>实际路径</span><strong title={source?.resolvedBinary ?? undefined}>{source?.resolvedBinary ?? "等待解析"}</strong></div><div><span>版本输出</span><strong>{source?.version ?? "尚未取得"}</strong></div></div>
          </section>

          <section className="panel capability-panel">
            <div className="panel-kicker">02 / 能力报告</div>
            <h2>当前可用能力</h2>
            <p className="panel-intro">来自所选二进制的协议响应与导出模式。历史内容和模型输出尚未验证。</p>
            <div className="capabilities">{labels.map(({ key, title, number }) => {
              const capability = source?.capabilities[key];
              const state = capability?.state ?? "notVerified";
              return <div className="capability" key={key}><span className="cap-number">{number}</span><div><strong>{title}</strong><small>{capability?.detail ?? "尚未连接"}</small></div><span className={`cap-pill ${state}`}>{state === "available" ? "已探测" : state === "unavailable" ? "不可用" : "未验证"}</span></div>;
            })}</div>
          </section>
        </div>

        <section className="panel jev-panel">
          <div className="panel-kicker">03 / 关系分析服务</div>
          <h2>Jev 连接设置</h2>
          <p className="panel-intro">连接检查只查询模型列表。固定合成推理单独运行，不读取项目历史。</p>
          <div className="jev-fields">
            <label htmlFor="jev-url">服务根地址<input id="jev-url" spellCheck={false} value={jevBaseUrl} onChange={(event) => setJevBaseUrl(event.target.value)} placeholder="https://api.typesafe.ai" /></label>
            <label htmlFor="jev-model">模型 ID<input id="jev-model" spellCheck={false} value={jevModel} onChange={(event) => setJevModel(event.target.value)} placeholder="jev-latest" /></label>
            <label htmlFor="jev-key">API Key<input id="jev-key" type="password" autoComplete="off" spellCheck={false} value={jevKey} onChange={(event) => setJevKey(event.target.value)} placeholder={jevStatus?.credentialError ? "钥匙串不可用；请先解锁" : jevStatus?.credentialConfigured ? "已保存；留空则保留现有密钥" : "填写后存入 macOS 钥匙串"} /></label>
          </div>
          <p className="jev-key-state">钥匙串状态：{jevStatus?.credentialError ? "暂时无法读取" : jevStatus?.credentialConfigured ? "已保存地址已配置密钥" : "已保存地址未配置密钥"}。更换服务地址时需填写新密钥。{jevUnsaved ? "请先保存修改，再运行验证。" : ""}</p>
          {jevStatus?.credentialError && <div className="page-error" role="alert">{errorText(jevStatus.credentialError)}</div>}
          {jevError && <div className="page-error" role="alert">{jevError}</div>}
          <div className="jev-actions">
            <button className="primary-button" disabled={jevBusy} onClick={saveJev}>保存设置</button>
            {jevStatus?.credentialError && <button className="browse-button" disabled={jevBusy} onClick={refreshJevStatus}>重查钥匙串</button>}
            <button className="browse-button" disabled={jevBusy || jevUnsaved || !jevStatus?.credentialConfigured} onClick={() => runJev("connection")}>验证连接</button>
            <button className="browse-button" disabled={jevBusy || jevUnsaved || !jevStatus?.credentialConfigured} onClick={() => runJev("inference")}>测试固定合成推理</button>
            {jevRequestBusy && <button className="plain-button" onClick={cancelJev}>取消请求</button>}
            <button className="plain-button" disabled={jevSaving || jevDeleting || !jevStatus?.credentialConfigured} onClick={deleteJev}>删除密钥</button>
          </div>
          <p className="jev-cost-note">测试推理会向所填服务发送固定合成材料，并消耗一次推理调用。取消仅确认本地请求结束，远端计算或计费可能继续。</p>
          {!jevUnsaved && jevConnection && <div className="jev-result" role="status"><strong>连接已验证</strong><span>可用名称：{jevConnection.models.join("、") || "列表为空"}。所填版本化模型 ID 仍可单独测试。</span></div>}
          {!jevUnsaved && jevInference && <div className="jev-result" role="status"><strong>合成推理已验证</strong><span>判定：{jevInference.answer.choice === "resolved" ? "已处理" : "尚未处理"}；置信度 {(jevInference.answer.confidence * 100).toFixed(1)}%；选项概率：已处理 {(jevInference.answer.probabilities.resolved * 100).toFixed(1)}%、尚未处理 {(jevInference.answer.probabilities.unresolved * 100).toFixed(1)}%。实际模型：{jevInference.actualModel}；输入 {jevInference.inputTokens}，输出 {jevInference.outputTokens} 个令牌。</span></div>}
        </section>

        <section className="footer-panel"><div><div className="panel-kicker">显示偏好</div><h3>界面外观</h3></div><div className="theme-picker" role="group" aria-label="界面外观">{(["system", "light", "dark"] as const).map((value) => <button key={value} className={theme === value ? "selected" : ""} onClick={() => changeTheme(value)}>{value === "system" ? "跟随系统" : value === "light" ? "浅色" : "深色"}</button>)}</div><small>保存于应用管理的本机用户数据目录</small></section>
        <section id="projects" className="panel project-panel">
          <div className="panel-kicker">02 / 本地项目</div><h2>选择项目</h2>
          <p className="panel-intro">选择真实目录。Git 项目按共享 Git 目录识别，独立克隆分别显示；非 Git 项目按所选目录归属。</p>
          <div className="project-picker"><input aria-label="本地项目目录" spellCheck={false} value={projectPath} onChange={(event) => setProjectPath(event.target.value)} placeholder="/本机/项目目录" /><button className="browse-button" onClick={() => void selectDirectory()}>浏览…</button><button className="primary-button" onClick={() => void chooseProject()} disabled={!projectPath.trim()}>选择目录<span>↗</span></button></div>
          {projectError && <div className="page-error" role="alert">{projectError}</div>}
          <div className="project-list">{projects.length === 0 ? <p className="empty-list">尚无项目。选择目录，或连接来源并刷新以发现 Git 项目。</p> : projects.map((project) => <button key={project.id} className={`project-choice ${project.id === projectCatalog?.selectedProjectId && !showUnassigned ? "selected" : ""}`} onClick={() => void chooseExistingProject(project.id)}><strong>{project.name}</strong><small>{project.root}</small><span>{project.gitCommonDir ? "Git 仓库" : "非 Git 目录"}{projectCatalog?.recentProjectIds.includes(project.id) ? " · 最近打开" : ""}</span></button>)}</div>
          <button className={`unassigned-choice ${showUnassigned ? "selected" : ""}`} onClick={() => setShowUnassigned(true)}>未归属会话：{projectCatalog?.unassigned.length ?? 0} 条</button>
        </section>
        <section id="sessions" className="panel session-panel">
          <div className="session-heading"><div><div className="panel-kicker">03 / 项目会话</div><h2>{showUnassigned ? "未归属会话" : projectSessions?.project.name ?? "请先选择项目"}</h2><p className="panel-intro">{showUnassigned ? "这些会话没有可确认的本地项目；逐条查看原因。" : projectSessions ? projectSessions.project.root : "项目选择会保存，重新打开应用时先显示缓存。"}</p></div><div className="refresh-actions"><button className="primary-button" disabled={!connected || refreshing} onClick={() => void startRefresh(projectCatalog?.selectedProjectId ?? null)}>{refreshing ? "正在刷新…" : "刷新列表"}<span>↻</span></button>{refreshing && <button className="browse-button" onClick={() => void cancelRefresh()}>取消刷新</button>}</div></div>
          {indexRun && <div className="index-run" role="status"><strong>索引{runLabels[indexRun.state]}</strong><span>运行 {indexRun.id}</span><span>已保存 {indexRun.pagesSaved} 页，读取 {indexRun.threadsSeen} 条</span>{indexRun.interrupted && <span>上次运行中断，可重新刷新</span>}{indexRun.error && <em>{errorText(indexRun.error)}</em>}</div>}
          {!showUnassigned && projectSessions && <div className="workspace-list"><strong>实际工作区</strong>{projectSessions.workspaces.length ? projectSessions.workspaces.map((workspace) => <code key={workspace}>{workspace}</code>) : <span>当前没有可验证的工作区</span>}</div>}
          <div className="list-summary"><strong>{showUnassigned ? projectCatalog?.unassigned.length ?? 0 : projectSessions?.threads.length ?? 0} 条会话</strong><span>{refreshing ? "刷新中；缓存可浏览" : !attempted ? "尚未采集" : complete ? connected ? "上次列表刷新完整" : "缓存上次列表完整；来源当前不可用" : "最近刷新未完成；旧缓存仍在"}</span><span>选择会话后按需读取回合与条目</span></div>
          {!showUnassigned && <p className="project-state" role="status">{projectState}</p>}
          {scopes.map((scope) => <div className="scope-line" key={String(scope.archived)}><strong>{scope.archived ? "已归档" : "未归档"}</strong><span>{scope.attemptedAtUnixMs === null ? "尚未读取" : scope.complete ? "上次列表完整" : "最近读取未完成"}</span><small>{scope.completedAtUnixMs ? `上次完整读取 ${new Date(scope.completedAtUnixMs).toLocaleString("zh-CN")}` : "没有完整读取记录"}</small>{scope.error && <em>{scope.error}</em>}</div>)}
          {listError && <div className="page-error" role="alert">{listError} 已保存的会话仍可浏览。</div>}
          <label className="session-search">查找会话<input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="标题、预览、Thread ID 或已生成总结" /></label>
          {!showUnassigned && projectSessions && <div className="explorer-filters" aria-label="会话过滤">
            <label>工作流<select aria-label="按工作流过滤" value={workstreamFilter} onChange={(event) => setWorkstreamFilter(event.target.value)}><option value="">全部工作流</option>{workstreams?.workstreams.map((stream) => <option key={stream.id} value={stream.id}>{stream.name}</option>)}<option value="ungrouped">未分组</option></select></label>
            <label>工作区<select aria-label="按工作区过滤" value={workspaceFilter} onChange={(event) => setWorkspaceFilter(event.target.value)}><option value="">全部工作区</option>{projectSessions.workspaces.map((root) => <option key={root} value={root}>{root}</option>)}</select></label>
            <label>归档<select aria-label="按归档过滤" value={archiveFilter} onChange={(event) => setArchiveFilter(event.target.value)}><option value="all">全部</option><option value="active">未归档</option><option value="archived">已归档</option></select></label>
            <label>完整性<select aria-label="按完整性过滤" value={completeFilter} onChange={(event) => setCompleteFilter(event.target.value)}><option value="all">全部</option><option value="complete">完整</option><option value="incomplete">不完整</option></select></label>
            <button className="browse-button" onClick={clearFilters}>清除过滤</button>
          </div>}
          {workstreamError && <p className="page-error" role="alert">{workstreamError}</p>}{currentQueryError && <p className="page-error" role="alert">{currentQueryError}</p>}
          {selectedThread && selectionHidden && <div className="selection-hidden" role="status">当前选择的会话被过滤条件隐藏；详情仍可查看。<button className="browse-button" onClick={clearFilters}>清除过滤</button></div>}
          <div className="thread-list">{visibleThreads.length === 0 ? <p className="empty-list">{!showUnassigned && !currentThreadMatches && !currentQueryError ? "正在查找会话…" : query || workstreamFilter || workspaceFilter || archiveFilter !== "all" || completeFilter !== "all" ? "没有匹配的会话；可以清除过滤。" : refreshing ? "正在刷新；缓存中暂无会话。" : !connected && attempted ? "来源当前不可用；缓存中暂无会话。" : showUnassigned ? "当前没有未归属会话。" : projectSessions ? "此项目暂无会话。" : "请先选择一个本地项目。"}</p> : shownThreads.map(({ thread, attribution }) => <article className={`thread-row ${selectedThreadId === thread.id ? "selected" : ""}`} key={thread.id}><div className="thread-main"><strong>{thread.title || thread.preview || thread.id}</strong><button className="browse-button history-open" onClick={() => setSelectedThreadId(thread.id)} aria-label={`查看会话 ${thread.id} 的历史`}>查看回合与条目</button><div className="thread-badges"><span>{thread.archived ? "已归档" : "未归档"}</span><span>{thread.sourceKind}{thread.sourceDetail ? ` / ${thread.sourceDetail}` : ""}</span>{thread.missingFromSource && <span className="thread-warning">完整列表中未再次出现</span>}{thread.readError && <span className="thread-warning" title={thread.readError}>单条读取不可用</span>}</div><small>{thread.id}</small></div><div className="thread-meta"><div><span>工作目录</span><code>{thread.cwd}</code></div><div><span>工作区根</span><code>{attribution.workspaceRoot ?? "无法确认"}</code></div><div><span>归属依据</span><code>{attribution.detail}</code></div>{attribution.diagnostic && <div className="attribution-diagnostic"><span>归属诊断</span><strong>{attribution.diagnostic}</strong></div>}<div><span>来源项目标识</span><code>{thread.projectId ?? "未提供"}</code></div><div><span>父会话 / 派生自</span><code>{thread.parentThreadId ?? thread.forkedFromId ?? "—"}</code></div><div><span>Git 分支</span><code>{thread.git?.branch ?? "—"}</code></div><div><span>最近更新</span><time>{new Date(thread.updatedAt * 1000).toLocaleString("zh-CN")}</time></div><div><span>元数据 / 回合 / 条目 / 内容</span><code>{thread.metadataComplete ? "完整" : "不完整"} / {thread.turnsComplete ? "完整" : "待采集"} / {thread.itemsComplete ? "完整" : "待采集"} / {thread.contentComplete ? "完整" : "不完整"}</code></div><div><span>列表采集</span><time>{new Date(thread.observedAtUnixMs).toLocaleString("zh-CN")}</time></div></div></article>)}</div>
          {threadLimit < visibleThreads.length && <button className="browse-button" onClick={() => setThreadLimit((limit) => limit + 40)}>显示更多会话（{shownThreads.length} / {visibleThreads.length}）</button>}
        </section>
        {!showUnassigned && projectSessions && <ProjectWorkstreamsView projectId={projectSessions.project.id}
          refreshVersion={graphVersion} onSelectThread={setSelectedThreadId} onSelectEvidence={selectEvidence} activeWorkstreamId={workstreamFilter} onFilterWorkstream={setWorkstreamFilter} onChanged={refreshGraphForAnalysis} />}
        {!showUnassigned && projectSessions && <div className="project-view-switch" role="group" aria-label="项目视图"><button aria-pressed={viewMode === "timeline"} onClick={() => setViewMode("timeline")}>时间线</button><button aria-pressed={viewMode === "graph"} onClick={() => setViewMode("graph")}>关系图</button></div>}
        {!showUnassigned && projectSessions && viewMode === "timeline" && <ProjectTimelineView key={projectSessions.project.id} projectId={projectSessions.project.id} refreshVersion={String(graphVersion)} connected={connected} onSelectThread={setSelectedThreadId} selectedThreadId={selectedThreadId} visibleThreadIds={visibleIds} workstreams={workstreams?.workstreams ?? []} onHistoryLoaded={() => void loadProjects().catch((error) => setListError(errorText(error)))} />}
        {selectedThread && selectedAttribution && projectSessions && !showUnassigned && <ProjectThreadDetailsView projectId={projectSessions.project.id} thread={selectedThread} attribution={selectedAttribution} refreshVersion={graphVersion} hidden={selectionHidden} onSelectThread={setSelectedThreadId} onSelectEvidence={selectEvidence} relationSource={relationSource} relationKind={relationKind} minimumConfidence={minimumConfidence} />}
        {selectedThread && <ThreadHistoryView key={selectedThread.id} threadId={selectedThread.id} updatedAt={selectedThread.updatedAt} connected={connected} locationRequest={evidenceLocation} onHistoryLoaded={() => void loadProjects().catch((error) => setListError(errorText(error)))} />}
        {!showUnassigned && projectSessions && viewMode === "graph" && <ProjectGraphView projectId={projectSessions.project.id} refreshVersion={graphVersion} onSelectEvidence={selectEvidence} selectedThreadId={selectedThreadId} onSelectThread={setSelectedThreadId} visibleThreadIds={visibleIds} relationSource={relationSource} onRelationSourceChange={setRelationSource} relationKind={relationKind} onRelationKindChange={setRelationKind} minimumConfidence={minimumConfidence} onMinimumConfidenceChange={setMinimumConfidence} />}
        {!showUnassigned && projectSessions && <CandidatePreviewView projectId={projectSessions.project.id} refreshVersion={String(graphVersion)} onSelectEvidence={selectEvidence} />}
        {!showUnassigned && projectSessions && <ProjectAnalysisView projectId={projectSessions.project.id} refreshVersion={String(graphVersion)} settingsRevision={analysisSettingsRevision} onRelationResultsChanged={refreshGraphForAnalysis} />}
        <p className="disclaimer">连接诊断不运行模型；列表刷新读取元数据，不恢复会话或读取会话正文。Jev 连接检查不运行推理；只有点击“测试固定合成推理”才会发起该次模型调用。</p>
      </div>
    </main>
  </div>;
}

const root = document.getElementById("root");
if (root) createRoot(root).render(<App />);
