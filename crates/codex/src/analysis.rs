use super::{exclude_analysis_thread, resolve_binary, ProbeError, Session};
use codexflow_domain::{AppError, ErrorCode};
use serde_json::{json, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
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

fn isolated_home() -> Result<PathBuf, AppError> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = env::temp_dir().join(format!("codexflow-analysis-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).map_err(|_| {
        failure(
            ErrorCode::AnalysisUnavailable,
            "无法创建隔离分析目录。",
            true,
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|_| {
            failure(
                ErrorCode::AnalysisUnavailable,
                "无法保护隔离分析目录。",
                true,
            )
        })?;
    }
    // A separate home removes configured MCP servers and skills. Authentication is
    // read through the existing file, never copied into CodexFlow storage.
    let original = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")));
    #[cfg(unix)]
    if let Some(auth) = original
        .map(|home| home.join("auth.json"))
        .filter(|path| path.is_file())
    {
        std::os::unix::fs::symlink(auth, dir.join("auth.json")).map_err(|_| {
            failure(
                ErrorCode::AnalysisUnavailable,
                "无法引用现有 Codex 认证。",
                true,
            )
        })?;
    }
    fs::write(
        dir.join("config.toml"),
        "web_search = \"disabled\"\n[features]\nmulti_agent = false\n",
    )
    .map_err(|_| {
        failure(
            ErrorCode::AnalysisUnavailable,
            "无法写入隔离分析配置。",
            true,
        )
    })?;
    Ok(dir)
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
                    return Err(failure(
                        ErrorCode::AnalysisUnavailable,
                        "分析回合未成功完成，旧总结已保留。",
                        true,
                    ));
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
    prompt: String,
    cancel: CancellationToken,
    mut on_event: impl FnMut(AnalysisEvent) -> Result<(), AppError>,
) -> Result<AnalysisOutput, AppError> {
    if cancel.is_cancelled() {
        return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false));
    }
    let binary = resolve_binary(binary_choice)?;
    let home = isolated_home()?;
    let result = analyze_in_home(&binary, &home, prompt, &cancel, &mut on_event).await;
    let _ = fs::remove_dir_all(&home);
    result
}

async fn analyze_in_home(
    binary: &Path,
    home: &Path,
    prompt: String,
    cancel: &CancellationToken,
    on_event: &mut impl FnMut(AnalysisEvent) -> Result<(), AppError>,
) -> Result<AnalysisOutput, AppError> {
    let mut session = Session::start_with_home(binary, Some(home)).await?;
    let result = async {
        session.initialize(false).await.map_err(protocol)?;
        if cancel.is_cancelled() { return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)); }
        let started = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)),
            result = session.request("thread/start", json!({
            "ephemeral":true,"cwd":home,"sandbox":"read-only","approvalPolicy":"never",
            "config":{"web_search":"disabled","features":{"multi_agent":false}},
            })) => result.map_err(protocol)?,
        };
        let thread = started.get("thread").ok_or_else(|| failure(ErrorCode::AnalysisUnavailable, "临时分析会话响应缺少会话。", false))?;
        let thread_id = thread.get("id").and_then(Value::as_str).ok_or_else(|| failure(ErrorCode::AnalysisUnavailable, "临时分析会话标识缺失。", false))?.to_owned();
        exclude_analysis_thread(&thread_id);
        if thread.get("ephemeral").and_then(Value::as_bool) != Some(true)
            || started.pointer("/sandbox/type").and_then(Value::as_str) != Some("readOnly")
            || started.pointer("/sandbox/networkAccess").and_then(Value::as_bool) == Some(true)
            || started.get("approvalPolicy").and_then(Value::as_str) != Some("never") {
            return Err(failure(ErrorCode::AnalysisUnavailable, "Codex 未确认临时只读且无需批准的隔离配置，已停止分析。", false));
        }
        on_event(AnalysisEvent::Thread(thread_id.clone()))?;
        if cancel.is_cancelled() { return Err(failure(ErrorCode::AnalysisCancelled, "分析已取消。", false)); }
        let schema = json!({"type":"object","additionalProperties":false,
            "properties":{"goal":{"type":"string"},"activity":{"type":"string"},
                "outcome":{"type":"string"},"decisions":{"type":"string"},"issues":{"type":"string"},
                "evidenceIds":{"type":"array","items":{"type":"string"}}},
            "required":["goal","activity","outcome","decisions","issues","evidenceIds"]});
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
