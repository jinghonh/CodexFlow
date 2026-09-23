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

  useEffect(() => {
    let active = true;
    invoke<Settings>("get_settings")
      .then(async (settings) => {
        if (!active) return;
        setTheme(settings.theme);
        setPath(settings.source.selectedBinary ?? "");
        setSource(settings.source);
        const next = await invoke<SourceStatus>("connect_source", { selectedBinary: settings.source.selectedBinary });
        if (active) setSource(next);
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
    } catch (error) { setPageError(errorText(error)); }
    finally { setBusy(false); }
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

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><span className="brand-mark">C<span>F</span></span><div><strong>CodexFlow</strong><small>本地工作过程</small></div></div>
      <div className="side-group"><span className="side-caption">工作空间</span><div className="side-link active"><span className="side-dot" />来源连接 <span className="side-index">01</span></div></div>
      <div className="side-note"><span className="side-note-line" />此阶段只验证本机 Codex 数据源。项目、会话与分析功能会在后续逐步接入。</div>
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
        <p className="disclaimer">诊断只检查协议，不会恢复会话、读取会话正文或发起模型调用。完整历史兼容性由后续采集流程验证。</p>
      </div>
    </main>
  </div>;
}

createRoot(document.getElementById("root")!).render(<App />);
