use super::{exclude_analysis_thread, resolve_binary, ProbeError, Session, NEXT_PROBE};
use codexflow_domain::{AppError, ErrorCode};
use serde_json::{json, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncBufReadExt, time::timeout};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum AnalysisEvent {
    Thread(String),
    Turn(String),
    Model(String),
    Cancelling,
    Terminal(String),
}

pub struct AnalysisOutput {
    pub text: String,
    pub model: String,
}

fn failure(code: ErrorCode, message: &str, retryable: bool) -> AppError {
    AppError::codex(code, message, retryable)
}

fn protocol(error: ProbeError) -> AppError {
    match error {
        ProbeError::Rpc(_, true) => failure(
            ErrorCode::ProtocolIncompatible,
            "所选 Codex 不支持临时结构化分析，请更换二进制。",
            false,
        ),
        ProbeError::Timeout => failure(
            ErrorCode::AnalysisUnavailable,
            "Codex 分析请求超时，请稍后重试。",
            true,
        ),
        ProbeError::Exited => failure(
            ErrorCode::ProcessExited,
            "Codex 分析进程已退出，旧总结已保留。",
            true,
        ),
        _ => failure(
            ErrorCode::AnalysisUnavailable,
            "Codex 分析协议失败，旧总结已保留。",
            true,
        ),
    }
}

fn turn_failure(error: Option<&Value>) -> AppError {
    let nested = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .and_then(|message| serde_json::from_str::<Value>(message).ok());
    let info = error
        .and_then(|error| error.get("codexErrorInfo"))
        .or_else(|| {
            nested
                .as_ref()
                .and_then(|value| value.pointer("/error/codexErrorInfo"))
        });
    let status = info
        .and_then(|info| info.as_object())
        .and_then(|object| object.values().next())
        .and_then(|value| value.get("httpStatusCode"))
        .and_then(Value::as_u64);
    match info.and_then(Value::as_str) {
        Some("unauthorized") => failure(
            ErrorCode::AnalysisAuthenticationFailed,
            "Codex 认证失败，请检查登录状态。",
            false,
        ),
        Some("usageLimitExceeded" | "sessionBudgetExceeded") => failure(
            ErrorCode::AnalysisQuotaExceeded,
            "Codex 使用额度不足，已暂停分析。",
            false,
        ),
        Some("serverOverloaded") => failure(
            ErrorCode::AnalysisOverloaded,
            "Codex 服务暂时过载，请稍后重试。",
            true,
        ),
        Some("badRequest" | "contextWindowExceeded") => failure(
            ErrorCode::AnalysisUnavailable,
            "Codex 拒绝本次分析请求，请检查模型与输入配置。",
            false,
        ),
        _ => match status {
            Some(401 | 403) => failure(
                ErrorCode::AnalysisAuthenticationFailed,
                "Codex 认证失败，请检查登录状态。",
                false,
            ),
            Some(402) => failure(
                ErrorCode::AnalysisQuotaExceeded,
                "Codex 使用额度不足，已暂停分析。",
                false,
            ),
            Some(429 | 529) => failure(
                ErrorCode::AnalysisOverloaded,
                "Codex 服务暂时受限，请稍后重试。",
                true,
            ),
            Some(422) => failure(
                ErrorCode::AnalysisUnavailable,
                "Codex 拒绝本次分析请求，请检查模型与输入配置。",
                false,
            ),
            _ => failure(
                ErrorCode::AnalysisUnavailable,
                "分析回合未成功完成，旧总结已保留。",
                true,
            ),
        },
    }
}

fn original_codex_home(override_home: Option<&Path>) -> Option<PathBuf> {
    override_home.map(Path::to_path_buf).or_else(|| {
        env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
    })
}

pub fn analysis_isolation_issue(override_home: Option<&Path>) -> Option<String> {
    // Read-only permits file inspection. Reject file-backed login before a model
    // turn; observing a tool event afterwards would be too late to protect it.
    if original_codex_home(override_home).is_some_and(|home| home.join("auth.json").exists()) {
        return Some("当前 Codex 认证保存在 auth.json 文件中，无法保证模型工具不能读取。请改用系统钥匙串保存认证，并移除该文件后再启用总结。".into());
    }
    if ["OPENAI_API_KEY", "CODEX_API_KEY"]
        .iter()
        .any(|key| env::var_os(key).is_some())
    {
        return Some(
            "当前进程含有模型可读取的认证环境变量，已禁用临时总结。请使用系统钥匙串认证。".into(),
        );
    }
    None
}

struct AnalysisWorkspace {
    root: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

fn isolated_workspace(override_home: Option<&Path>) -> Result<AnalysisWorkspace, AppError> {
    if let Some(message) = analysis_isolation_issue(override_home) {
        return Err(failure(ErrorCode::AnalysisUnavailable, &message, false));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = env::temp_dir().join(format!(
        "codexflow-analysis-{}-{nonce}-{}",
        std::process::id(),
        NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).map_err(|_| {
        failure(
            ErrorCode::AnalysisUnavailable,
            "无法创建隔离分析目录。",
            true,
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|_| {
            failure(
                ErrorCode::AnalysisUnavailable,
                "无法保护隔离分析目录。",
                true,
            )
        })?;
    }
    // The model's cwd never contains authentication material. Keyring login is
    // required, and the effective config is checked before thread/start.
    let home = root.join("codex-home");
    let cwd = root.join("workspace");
    fs::create_dir(&home)
        .and_then(|_| fs::create_dir(&cwd))
        .map_err(|_| {
            failure(
                ErrorCode::AnalysisUnavailable,
                "无法创建独立分析工作区。",
                true,
            )
        })?;
    fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store = \"keyring\"\nweb_search = \"disabled\"\n[features]\nshell_tool = false\nunified_exec = false\napps = false\nhooks = false\nmulti_agent = false\nremote_plugin = false\nplugins = false\nview_image = false\nbrowser_use = false\nbrowser_use_external = false\nbrowser_use_full_cdp_access = false\ncomputer_use = false\nin_app_browser = false\nin_app_local_automation = false\nimage_generation = false\nshell_snapshot = false\nskill_search = false\nskill_mcp_dependency_install = false\ntool_call_mcp_elicitation = false\ntool_suggest = false\nworkspace_dependencies = false\ngoals = false\n",
    )
    .map_err(|_| {
        failure(
            ErrorCode::AnalysisUnavailable,
            "无法写入隔离分析配置。",
            true,
        )
    })?;
    Ok(AnalysisWorkspace { root, home, cwd })
}

pub fn configured_summary_model() -> Option<String> {
    configured_summary_model_at(None)
}

pub fn configured_summary_model_at(override_home: Option<&Path>) -> Option<String> {
    let home = original_codex_home(override_home)?;
    let config = fs::read_to_string(home.join("config.toml")).ok()?;
    let parsed: toml::Value = toml::from_str(&config).ok()?;
    parsed
        .get("model")
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
}

async fn frame(session: &mut Session) -> Result<Value, ProbeError> {
    if let Some(event) = session.pending_events.pop_front() {
        return Ok(event);
    }
    let count = timeout(
        Duration::from_secs(600),
        session.stdout.read_until(b'\n', &mut session.pending_line),
    )
    .await
    .map_err(|_| ProbeError::Timeout)?
    .map_err(|_| ProbeError::Exited)?;
    if count == 0 {
        return Err(ProbeError::Exited);
    }
    let line = std::mem::take(&mut session.pending_line);
    serde_json::from_slice(&line).map_err(|_| ProbeError::InvalidResponse)
}

async fn verify_isolated_config(session: &mut Session) -> Result<(), AppError> {
    let response = session
        .request("config/read", json!({"includeLayers":false}))
        .await
        .map_err(protocol)?;
    let config = response.get("config").ok_or_else(|| {
        failure(
            ErrorCode::AnalysisUnavailable,
            "无法确认分析进程的有效配置，已禁用临时总结。",
            false,
        )
    })?;
    let disabled = [
        "shell_tool",
        "unified_exec",
        "apps",
        "hooks",
        "multi_agent",
        "remote_plugin",
        "plugins",
        "view_image",
        "browser_use",
        "browser_use_external",
        "browser_use_full_cdp_access",
        "computer_use",
        "in_app_browser",
        "in_app_local_automation",
        "image_generation",
        "shell_snapshot",
        "skill_search",
        "skill_mcp_dependency_install",
        "tool_call_mcp_elicitation",
        "tool_suggest",
        "workspace_dependencies",
        "goals",
    ];
    let safe = disabled
        .iter()
        .all(|key| config.pointer(&format!("/features/{key}")) == Some(&Value::Bool(false)))
        && config.get("web_search").and_then(Value::as_str) == Some("disabled")
        && config
            .get("cli_auth_credentials_store")
            .and_then(Value::as_str)
            == Some("keyring")
        && config
            .get("mcp_servers")
            .and_then(Value::as_object)
            .is_some_and(|servers| servers.is_empty());
    if !safe {
        return Err(failure(
            ErrorCode::AnalysisUnavailable,
            "Codex 未确认关闭分析工具、连接器或网络；已禁用此分析配置。",
            false,
        ));
    }
    Ok(())
}

async fn reject_server_request(session: &mut Session, event: &Value) -> Result<(), ProbeError> {
    if let Some(id) = event.get("id") {
        session.write(&json!({"id":id,"error":{"code":-32000,"message":"CodexFlow analysis does not approve tools or permissions"}})).await?;
    }
    Ok(())
}

fn is_for_turn(event: &Value, thread_id: &str, turn_id: &str) -> bool {
    event
        .get("params")
        .and_then(|params| params.get("threadId"))
        .and_then(Value::as_str)
        == Some(thread_id)
        && event
            .get("params")
            .and_then(|params| params.get("turnId").or_else(|| params.pointer("/turn/id")))
            .and_then(Value::as_str)
            == Some(turn_id)
}

fn final_text(item: &Value) -> Option<String> {
    (item.get("type").and_then(Value::as_str) == Some("agentMessage")
        && item
            .get("phase")
            .and_then(Value::as_str)
            .is_none_or(|phase| phase == "final_answer"))
    .then(|| {
        item.get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    })
    .filter(|text| !text.is_empty())
}

async fn await_turn(
    session: &mut Session,
    thread_id: &str,
    turn_id: &str,
    cancel: &CancellationToken,
    on_event: &mut impl FnMut(AnalysisEvent) -> Result<(), AppError>,
) -> Result<String, AppError> {
    let mut final_message = None;
    let mut cancelling = false;
    let mut interrupt_id = None;
    let mut cancel_deadline = None;
    let mut last_error: Option<Value> = None;
    loop {
        let event = if cancelling {
            match timeout(
                cancel_deadline
                    .unwrap_or_else(tokio::time::Instant::now)
                    .saturating_duration_since(tokio::time::Instant::now()),
                frame(session),
            )
            .await
            {
                Ok(Ok(event)) => event,
                _ => {
                    return Err(failure(
                        ErrorCode::AnalysisCancelled,
                        "已终止专用分析进程，取消完成。",
                        false,
                    ))
                }
            }
        } else {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    cancelling = true;
                    on_event(AnalysisEvent::Cancelling)?;
                    let id = session.next_id;
                    session.next_id += 1;
                    interrupt_id = Some(id);
                    if session.write(&json!({"id":id,"method":"turn/interrupt","params":{"threadId":thread_id,"turnId":turn_id}})).await.is_err() {
                        return Err(failure(ErrorCode::AnalysisCancelled, "专用分析进程已退出，取消完成。", false));
                    }
                    cancel_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(4));
                    continue;
                }
                result = frame(session) => result.map_err(protocol)?,
            }
        };
        if interrupt_id.is_some() && event.get("id").and_then(Value::as_u64) == interrupt_id {
            if event.get("result").is_none() {
                return Err(failure(
                    ErrorCode::AnalysisCancelled,
                    "中断响应失败，已终止专用分析进程。",
                    false,
                ));
            }
            continue;
        }
        if event.get("id").is_some() {
            reject_server_request(session, &event)
                .await
                .map_err(protocol)?;
            return Err(failure(
                ErrorCode::AnalysisUnavailable,
                "分析会话请求额外权限或工具，已拒绝并终止。",
                false,
            ));
        }
        if !is_for_turn(&event, thread_id, turn_id) {
            continue;
        }
        match event.get("method").and_then(Value::as_str) {
            Some("error") => {
                last_error = event.pointer("/params/error").cloned();
                if event.pointer("/params/willRetry").and_then(Value::as_bool) == Some(true) {
                    // A model retry inside app-server has no separate budget ticket.
                    // Interrupt this turn instead of accepting an uncounted retry.
                    cancelling = true;
                    on_event(AnalysisEvent::Cancelling)?;
                    let id = session.next_id;
                    session.next_id += 1;
                    interrupt_id = Some(id);
                    if session.write(&json!({"id":id,"method":"turn/interrupt","params":{"threadId":thread_id,"turnId":turn_id}})).await.is_err() {
                        return Err(failure(ErrorCode::AnalysisCancelled, "专用分析进程已退出，内部重试已停止。", false));
                    }
                    cancel_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(4));
                }
            }
            Some("item/started") | Some("item/completed") => {
                let item = &event["params"]["item"];
                let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(kind, "userMessage" | "agentMessage" | "reasoning") {
                    return Err(failure(
                        ErrorCode::AnalysisUnavailable,
                        "分析尝试使用工具，已终止专用进程；请检查隔离配置。",
                        false,
                    ));
                }
                if event["method"] == "item/completed" {
                    if let Some(text) = final_text(item) {
                        final_message = Some(text);
                    }
                }
            }
            Some("turn/completed") => {
                let turn = &event["params"]["turn"];
                let status = turn
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                if !matches!(status, "completed" | "interrupted" | "failed") {
                    continue;
                }
                on_event(AnalysisEvent::Terminal(status.to_owned()))?;
                if cancelling {
                    return Err(failure(
                        ErrorCode::AnalysisCancelled,
                        "分析回合已终结，取消完成。",
                        false,
                    ));
                }
                if turn.get("status").and_then(Value::as_str) != Some("completed") {
                    let unsupported_model = turn
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .and_then(|message| serde_json::from_str::<Value>(message).ok())
                        .and_then(|value| {
                            value
                                .pointer("/error/message")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .is_some_and(|message| {
                            message.contains("model is not supported")
                                || message.contains("model is not available")
                        });
                    if unsupported_model {
                        return Err(failure(
                            ErrorCode::AnalysisModelUnsupported,
                            "当前 Codex 账户不支持所配置的模型；请在 Codex 配置中更换模型后重试。",
                            false,
                        ));
                    }
                    return Err(turn_failure(turn.get("error").or(last_error.as_ref())));
                }
                if final_message.is_none() {
                    final_message = turn
                        .get("items")
                        .and_then(Value::as_array)
                        .and_then(|items| items.iter().rev().find_map(final_text));
                }
                return final_message.ok_or_else(|| {
                    failure(
                        ErrorCode::AnalysisInvalidResult,
                        "分析完成但未返回结构化最终消息。",
                        true,
                    )
                });
            }
            _ => {}
        }
    }
}

pub async fn analyze_summary(
    binary_choice: Option<&str>,
    model_choice: Option<&str>,
    origin_home: Option<&Path>,
    prompt: String,
    cancel: CancellationToken,
    mut on_event: impl FnMut(AnalysisEvent) -> Result<(), AppError>,
) -> Result<AnalysisOutput, AppError> {
    let schema = json!({"type":"object","additionalProperties":false,
        "properties":{"goal":{"type":"string"},"activity":{"type":"string"},
            "outcome":{"type":"string"},"decisions":{"type":"string"},"issues":{"type":"string"},
            "evidenceIds":{"type":"array","items":{"type":"string"}}},
        "required":["goal","activity","outcome","decisions","issues","evidenceIds"]});
    analyze_structured(
        binary_choice,
        model_choice,
        origin_home,
        prompt,
        schema,
        cancel,
        &mut on_event,
    )
    .await
}

pub async fn analyze_workstream_name(
    binary_choice: Option<&str>,
    model_choice: Option<&str>,
    origin_home: Option<&Path>,
    prompt: String,
    cancel: CancellationToken,
    mut on_event: impl FnMut(AnalysisEvent) -> Result<(), AppError>,
) -> Result<AnalysisOutput, AppError> {
    let schema = json!({"type":"object","additionalProperties":false,
        "properties":{"name":{"type":"string"}},"required":["name"]});
    analyze_structured(
        binary_choice,
        model_choice,
        origin_home,
        prompt,
        schema,
        cancel,
        &mut on_event,
    )
    .await
}

async fn analyze_structured(
    binary_choice: Option<&str>,
    model_choice: Option<&str>,
    origin_home: Option<&Path>,
    prompt: String,
    schema: Value,
    cancel: CancellationToken,
    on_event: &mut impl FnMut(AnalysisEvent) -> Result<(), AppError>,
) -> Result<AnalysisOutput, AppError> {
    if cancel.is_cancelled() {
        return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false));
    }
    let binary = resolve_binary(binary_choice)?;
    let workspace = isolated_workspace(origin_home)?;
    let result = analyze_in_home(
        &binary,
        &workspace.home,
        &workspace.cwd,
        model_choice,
        prompt,
        schema,
        &cancel,
        on_event,
    )
    .await;
    let _ = fs::remove_dir_all(&workspace.root);
    result
}

async fn analyze_in_home(
    binary: &Path,
    home: &Path,
    cwd: &Path,
    model_choice: Option<&str>,
    prompt: String,
    schema: Value,
    cancel: &CancellationToken,
    on_event: &mut impl FnMut(AnalysisEvent) -> Result<(), AppError>,
) -> Result<AnalysisOutput, AppError> {
    let mut session = Session::start_with_home(binary, Some(home), Some(cwd)).await?;
    let result = async {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)),
            result = session.initialize(false) => result.map_err(protocol)?,
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)),
            result = verify_isolated_config(&mut session) => result?,
        }
        if cancel.is_cancelled() { return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)); }
        let started = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)),
            result = session.request("thread/start", json!({
            "ephemeral":true,"cwd":cwd,"sandbox":"read-only","approvalPolicy":"never","model":model_choice,
            "config":{"web_search":"disabled","features":{"multi_agent":false}},
            })) => result.map_err(protocol)?,
        };
        let thread = started.get("thread").ok_or_else(|| failure(ErrorCode::AnalysisUnavailable, "临时分析会话响应缺少会话。", false))?;
        let thread_id = thread.get("id").and_then(Value::as_str).ok_or_else(|| failure(ErrorCode::AnalysisUnavailable, "临时分析会话标识缺失。", false))?.to_owned();
        exclude_analysis_thread(&thread_id);
        if thread.get("ephemeral").and_then(Value::as_bool) != Some(true)
            || started.pointer("/sandbox/type").and_then(Value::as_str) != Some("readOnly")
            || started.pointer("/sandbox/networkAccess").and_then(Value::as_bool) != Some(false)
            || started.get("approvalPolicy").and_then(Value::as_str) != Some("never") {
            return Err(failure(ErrorCode::AnalysisUnavailable, "Codex 未确认临时只读、禁网络且无需批准的隔离配置，已停止分析。", false));
        }
        on_event(AnalysisEvent::Thread(thread_id.clone()))?;
        if cancel.is_cancelled() { return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)); }
        let started_turn = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)),
            result = session.request("turn/start", json!({"threadId":thread_id,
            "input":[{"type":"text","text":prompt}],"outputSchema":schema,
            "approvalPolicy":"never"})) => result.map_err(protocol)?,
        };
        let turn_id = started_turn.pointer("/turn/id").and_then(Value::as_str)
            .ok_or_else(|| failure(ErrorCode::AnalysisUnavailable, "分析回合标识缺失。", false))?.to_owned();
        on_event(AnalysisEvent::Turn(turn_id.clone()))?;
        let model = started.get("model").and_then(Value::as_str).unwrap_or("Codex 默认模型").to_owned();
        on_event(AnalysisEvent::Model(model.clone()))?;
        let text = await_turn(&mut session, &thread_id, &turn_id, cancel, on_event).await?;
        Ok(AnalysisOutput { text, model })
    }.await;
    session.close().await;
    result
}
