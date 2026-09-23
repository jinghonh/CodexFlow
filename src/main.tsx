import React, { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./style.css";

type Theme = "system" | "light" | "dark";
type Capability = { state: "available" | "unavailable" | "notVerified"; detail: string };
type AppError = { code: string; message: string; retryable: boolean; cachePreserved: boolean; backend: string };
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
type Thread = {
  id: string; sessionId: string; title: string | null; preview: string; cwd: string;
  projectId: string | null; sourceKind: string; sourceDetail: string | null;
  threadSource: string | null; parentThreadId: string | null; forkedFromId: string | null;
  git: { branch: string | null; sha: string | null; originUrl: string | null } | null;
  createdAt: number; updatedAt: number; archived: boolean; metadataComplete: boolean;
  contentComplete: boolean; readError: string | null; observedAtUnixMs: number;
};
type Scope = { archived: boolean; complete: boolean; attemptedAtUnixMs: number | null; completedAtUnixMs: number | null; error: string | null };
type SessionList = { threads: Thread[]; scopes: Scope[] };

const labels: { key: keyof SourceStatus["capabilities"]; title: string; number: string }[] = [
  { key: "metadata", title: "会话元数据", number: "01" },
  { key: "history", title: "历史读取", number: "02" },
  { key: "experimentalHistory", title: "实验性分页", number: "03" },
  { key: "codexSummary", title: "Codex 总结", number: "04" },
  { key: "codexNaming", title: "工作流命名", number: "05" },
];

function errorText(error: unknown): string {
  if (typeof error === "object" && error && "message" in error && typeof error.message === "string") return error.message;
  return "操作失败。请检查桌面应用状态后重试。";
}

function App() {
  const [source, setSource] = useState<SourceStatus | null>(null);
  const [path, setPath] = useState("");
  const [theme, setTheme] = useState<Theme>("system");
  const [busy, setBusy] = useState(true);
  const [pageError, setPageError] = useState("");
  const [sessionList, setSessionList] = useState<SessionList | null>(null);
  const [listError, setListError] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [query, setQuery] = useState("");

  useEffect(() => {
    let active = true;
    invoke<Settings>("get_settings")
      .then(async (settings) => {
        if (!active) return;
        try {
          const cached = await invoke<SessionList>("get_session_list");
          if (active) setSessionList(cached);
        } catch (error) { if (active) setListError(errorText(error)); }
        if (!active) return;
        setTheme(settings.theme);
        setPath(settings.source.selectedBinary ?? "");
        setSource(settings.source);
        const next = await invoke<SourceStatus>("connect_source", { selectedBinary: settings.source.selectedBinary });
        if (active) setSource(next);
        if (active && next.connection === "connected") {
          setRefreshing(true);
          try {
            const list = await invoke<SessionList>("refresh_session_list");
            if (active) { setSessionList(list); setListError(""); }
          } catch (error) { if (active) setListError(errorText(error)); }
          finally { if (active) setRefreshing(false); }
        }
      })
      .catch((error) => { if (active) setPageError(errorText(error)); })
      .finally(() => { if (active) setBusy(false); });
    const timer = window.setInterval(() => {
      invoke<SourceStatus>("get_source_status").then((next) => { if (active) setSource(next); }).catch(() => {});
    }, 4000);
    return () => { active = false; window.clearInterval(timer); };
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
      if (next.connection === "connected") await refreshSessions();
    } catch (error) { setPageError(errorText(error)); }
    finally { setBusy(false); }
  }

  async function refreshSessions() {
    setRefreshing(true);
    setListError("");
    try { setSessionList(await invoke<SessionList>("refresh_session_list")); }
    catch (error) { setListError(errorText(error)); }
    finally { setRefreshing(false); }
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

  const connected = source?.connection === "connected";
  const failed = source?.connection === "failed";
  const checkedAt = source?.checkedAtUnixMs ? new Date(source.checkedAtUnixMs).toLocaleString("zh-CN") : "尚未检查";
  const scopes = sessionList?.scopes ?? [];
  const attempted = scopes.some((scope) => scope.attemptedAtUnixMs !== null);
  const complete = scopes.length === 2 && scopes.every((scope) => scope.complete);
  const visibleThreads = (sessionList?.threads ?? []).filter((thread) =>
    [thread.title, thread.preview, thread.id, thread.cwd].some((value) => value?.toLowerCase().includes(query.toLowerCase()))
  );

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><span className="brand-mark">C<span>F</span></span><div><strong>CodexFlow</strong><small>本地工作过程</small></div></div>
      <div className="side-group"><span className="side-caption">工作空间</span><div className="side-link active"><span className="side-dot" />来源连接 <span className="side-index">01</span></div><a className="side-link" href="#sessions"><span className="side-dot" />会话列表 <span className="side-index">02</span></a></div>
      <div className="side-note"><span className="side-note-line" />本机持久会话会保存到应用缓存。项目归属与历史内容在后续阶段接入。</div>
      <div className="sidebar-bottom"><span className="sidebar-bottom-symbol">↗</span><div>本机运行<br /><strong>数据留在你的设备</strong></div></div>
    </aside>

    <main className="content">
      <header className="topbar"><span>设置 / 来源连接</span><div className="topbar-right"><span className="topbar-pulse" />本地桌面应用</div></header>
      <div className="page-body">
        <div className="eyebrow">SOURCE / 01 <span /></div>
        <div className="page-heading"><div><h1>连接 Codex 数据源<span className="accent">.</span></h1><p>确认应用实际使用的二进制，以及可以安全读取的能力。</p></div><div className="heading-badge">本机连接诊断<br /><strong>不会启动模型</strong></div></div>

        <section className="status-banner" data-state={failed ? "failed" : connected ? "connected" : "pending"} aria-live="polite">
          <div className="status-icon">{failed ? "!" : connected ? "✓" : "·"}</div>
          <div><span className="status-caption">连接状态</span><strong>{busy ? "正在检查来源…" : failed ? "连接失败" : connected ? "已连接到 app-server" : "等待连接"}</strong><small>{failed ? source?.error?.message : connected ? "初始化与基础来源能力已完成检查" : "选择二进制并开始诊断"}</small></div>
          <span className="status-time">{checkedAt}</span>
        </section>

        {pageError && <div className="page-error" role="alert">{pageError}</div>}
        {source?.error && <div className="error-detail" role="alert"><span className="error-code">{source.error.code}</span><span>{source.error.retryable ? "可以修正后重试。" : "请更换或升级二进制。"} 已有本地数据保持不变。</span></div>}

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

        <section className="footer-panel"><div><div className="panel-kicker">显示偏好</div><h3>界面外观</h3></div><div className="theme-picker" role="group" aria-label="界面外观">{(["system", "light", "dark"] as const).map((value) => <button key={value} className={theme === value ? "selected" : ""} onClick={() => changeTheme(value)}>{value === "system" ? "跟随系统" : value === "light" ? "浅色" : "深色"}</button>)}</div><small>保存于应用管理的本机用户数据目录</small></section>
        <section id="sessions" className="panel session-panel">
          <div className="session-heading"><div><div className="panel-kicker">02 / 持久会话</div><h2>已发现的会话</h2><p className="panel-intro">按 Codex Thread ID 保存；包括归档与子代理。重新打开应用时先显示本地缓存。</p></div><button className="primary-button" disabled={!connected || refreshing} onClick={() => void refreshSessions()}>{refreshing ? "正在刷新…" : "刷新列表"}<span>↻</span></button></div>
          <div className="list-summary"><strong>{sessionList?.threads.length ?? 0} 条会话</strong><span>{!attempted ? "尚未采集" : complete ? "列表完整" : "列表部分完成；旧缓存仍在"}</span><span>历史内容：待采集</span></div>
          {scopes.map((scope) => <div className="scope-line" key={String(scope.archived)}><strong>{scope.archived ? "已归档" : "未归档"}</strong><span>{scope.attemptedAtUnixMs === null ? "尚未读取" : scope.complete ? "列表完整" : "读取不完整"}</span><small>{scope.completedAtUnixMs ? `上次完整读取 ${new Date(scope.completedAtUnixMs).toLocaleString("zh-CN")}` : "没有完整读取记录"}</small>{scope.error && <em>{scope.error}</em>}</div>)}
          {listError && <div className="page-error" role="alert">{listError} 已保存的会话仍可浏览。</div>}
          <label className="session-search">查找会话<input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="标题、预览、Thread ID 或工作目录" /></label>
          <div className="thread-list">{visibleThreads.length === 0 ? <p className="empty-list">{query ? "没有匹配的会话。" : "缓存中尚无会话。连接来源后可以刷新。"}</p> : visibleThreads.map((thread) => <article className="thread-row" key={thread.id}><div className="thread-main"><strong>{thread.title || thread.preview || thread.id}</strong><div className="thread-badges"><span>{thread.archived ? "已归档" : "未归档"}</span><span>{thread.sourceKind}{thread.sourceDetail ? ` / ${thread.sourceDetail}` : ""}</span>{thread.readError && <span className="thread-warning" title={thread.readError}>单条读取不可用</span>}</div><small>{thread.id}</small></div><div className="thread-meta"><div><span>工作目录</span><code>{thread.cwd}</code></div><div><span>来源项目</span><code>{thread.projectId ?? "未提供"}</code></div><div><span>父会话 / 派生自</span><code>{thread.parentThreadId ?? thread.forkedFromId ?? "—"}</code></div><div><span>Git 分支</span><code>{thread.git?.branch ?? "—"}</code></div><div><span>最近更新</span><time>{new Date(thread.updatedAt * 1000).toLocaleString("zh-CN")}</time></div><div><span>列表采集</span><time>{new Date(thread.observedAtUnixMs).toLocaleString("zh-CN")}</time></div></div></article>)}</div>
        </section>
        <p className="disclaimer">连接诊断不运行模型；列表刷新读取元数据，不恢复会话或读取会话正文。历史内容完整性由后续采集流程验证。</p>
      </div>
    </main>
  </div>;
}

createRoot(document.getElementById("root")!).render(<App />);
