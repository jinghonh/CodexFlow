import React, { useEffect, useRef, useState } from "react";
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
type JevStatus = { config: { baseUrl: string; model: string }; credentialConfigured: boolean };
type JevConnectionResult = { models: string[]; requestedModel: string };
type JevInferenceResult = { requestedModel: string; actualModel: string; inputTokens: number; outputTokens: number };

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
  const jevEpoch = useRef(0);

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
    invoke<JevStatus>("get_jev_status").then((status) => {
      setJevStatus(status);
      setJevBaseUrl(status.config.baseUrl);
      setJevModel(status.config.model);
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
    } catch (error) { setJevError(errorText(error)); }
    finally { setJevSaving(false); }
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
    } catch (error) { setJevError(errorText(error)); }
    finally { setJevDeleting(false); }
  }

  async function runJev(kind: "connection" | "inference") {
    const epoch = jevEpoch.current;
    setJevRequestBusy(true);
    setJevError("");
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

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><span className="brand-mark">C<span>F</span></span><div><strong>CodexFlow</strong><small>本地工作过程</small></div></div>
      <div className="side-group"><span className="side-caption">工作空间</span><div className="side-link active"><span className="side-dot" />来源与分析连接 <span className="side-index">01</span></div></div>
      <div className="side-note"><span className="side-note-line" />在这里配置本机 Codex 来源和 Jev 关系分析服务。项目分析由后续流程手动启动。</div>
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

        <section className="panel jev-panel">
          <div className="panel-kicker">03 / 关系分析服务</div>
          <h2>Jev 连接设置</h2>
          <p className="panel-intro">连接检查只查询模型列表。固定合成推理单独运行，不读取项目历史。</p>
          <div className="jev-fields">
            <label htmlFor="jev-url">服务根地址<input id="jev-url" spellCheck={false} value={jevBaseUrl} onChange={(event) => setJevBaseUrl(event.target.value)} placeholder="https://api.typesafe.ai" /></label>
            <label htmlFor="jev-model">模型 ID<input id="jev-model" spellCheck={false} value={jevModel} onChange={(event) => setJevModel(event.target.value)} placeholder="jev-latest" /></label>
            <label htmlFor="jev-key">API Key<input id="jev-key" type="password" autoComplete="off" spellCheck={false} value={jevKey} onChange={(event) => setJevKey(event.target.value)} placeholder={jevStatus?.credentialConfigured ? "已保存；留空则保留现有密钥" : "填写后存入 macOS 钥匙串"} /></label>
          </div>
          <p className="jev-key-state">钥匙串状态：{jevStatus?.credentialConfigured ? "已保存地址已配置密钥" : "已保存地址未配置密钥"}。更换服务地址时需填写新密钥。{jevUnsaved ? "请先保存修改，再运行验证。" : ""}</p>
          {jevError && <div className="page-error" role="alert">{jevError}</div>}
          <div className="jev-actions">
            <button className="primary-button" disabled={jevBusy} onClick={saveJev}>保存设置</button>
            <button className="browse-button" disabled={jevBusy || jevUnsaved || !jevStatus?.credentialConfigured} onClick={() => runJev("connection")}>验证连接</button>
            <button className="browse-button" disabled={jevBusy || jevUnsaved || !jevStatus?.credentialConfigured} onClick={() => runJev("inference")}>测试固定合成推理</button>
            {jevRequestBusy && <button className="plain-button" onClick={cancelJev}>取消请求</button>}
            <button className="plain-button" disabled={jevSaving || jevDeleting || !jevStatus?.credentialConfigured} onClick={deleteJev}>删除密钥</button>
          </div>
          <p className="jev-cost-note">测试推理会向所填服务发送固定合成材料，并消耗一次推理调用。取消仅确认本地请求结束，远端计算或计费可能继续。</p>
          {!jevUnsaved && jevConnection && <div className="jev-result" role="status"><strong>连接已验证</strong><span>可用名称：{jevConnection.models.join("、") || "列表为空"}。所填版本化模型 ID 仍可单独测试。</span></div>}
          {!jevUnsaved && jevInference && <div className="jev-result" role="status"><strong>合成推理已验证</strong><span>实际模型：{jevInference.actualModel}；输入 {jevInference.inputTokens}，输出 {jevInference.outputTokens} 个令牌。</span></div>}
        </section>

        <section className="footer-panel"><div><div className="panel-kicker">显示偏好</div><h3>界面外观</h3></div><div className="theme-picker" role="group" aria-label="界面外观">{(["system", "light", "dark"] as const).map((value) => <button key={value} className={theme === value ? "selected" : ""} onClick={() => changeTheme(value)}>{value === "system" ? "跟随系统" : value === "light" ? "浅色" : "深色"}</button>)}</div><small>保存于应用管理的本机用户数据目录</small></section>
        <p className="disclaimer">来源诊断不会恢复会话或读取正文。Jev 连接检查不运行推理；只有点击“测试固定合成推理”才会发起该次模型调用。</p>
      </div>
    </main>
  </div>;
}

createRoot(document.getElementById("root")!).render(<App />);
