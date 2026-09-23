mod analysis;
mod history;
pub use analysis::{
    analysis_isolation_issue, analyze_summary, configured_summary_model, AnalysisEvent,
    AnalysisOutput,
};

use codexflow_domain::{
    AppError, Capability, ErrorCode, GitMetadata, ListScopeStatus, SourceCapabilities,
    ThreadMetadata,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::sync::{Mutex as StdMutex, OnceLock};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const PROBE_THREAD_ID: &str = "00000000-0000-4000-8000-000000000000";
const SOURCES: [&str; 10] = [
    "cli",
    "vscode",
    "exec",
    "appServer",
    "subAgent",
    "subAgentReview",
    "subAgentCompact",
    "subAgentThreadSpawn",
    "subAgentOther",
    "unknown",
];
static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
static ANALYSIS_THREADS: OnceLock<StdMutex<HashSet<String>>> = OnceLock::new();

fn analysis_threads() -> &'static StdMutex<HashSet<String>> {
    ANALYSIS_THREADS.get_or_init(|| StdMutex::new(HashSet::new()))
}

fn exclude_analysis_thread(id: &str) {
    analysis_threads().lock().unwrap().insert(id.to_owned());
}

pub struct Diagnosis {
    pub resolved_binary: String,
    pub version: Option<String>,
    pub capabilities: SourceCapabilities,
    pub session: Session,
}

pub struct Collection {
    pub threads: Vec<ThreadMetadata>,
    pub scopes: Vec<ListScopeStatus>,
    pub cancelled: bool,
}

pub enum CollectionUpdate<'a> {
    Page(&'a [ThreadMetadata]),
    Scope(&'a ListScopeStatus),
}

impl Session {
    pub async fn collect_threads(&mut self, observed_at_unix_ms: i64) -> Collection {
        self.collect_threads_with(observed_at_unix_ms, &CancellationToken::new(), |_| Ok(()))
            .await
            .expect("no-op collection callbacks cannot fail")
    }

    pub async fn collect_threads_with(
        &mut self,
        observed_at_unix_ms: i64,
        cancel: &CancellationToken,
        mut on_update: impl FnMut(CollectionUpdate<'_>) -> Result<(), AppError>,
    ) -> Result<Collection, AppError> {
        let mut threads = BTreeMap::<String, ThreadMetadata>::new();
        let mut scopes = Vec::with_capacity(2);
        let mut cancelled = false;
        for archived in [false, true] {
            let mut cursor: Option<String> = None;
            let mut seen_cursors = HashSet::new();
            let mut errors = Vec::new();
            loop {
                if cancel.is_cancelled() {
                    cancelled = true;
                    errors.push("用户已取消列表刷新。".to_owned());
                    break;
                }
                let mut params =
                    json!({"limit": 100, "sourceKinds": SOURCES, "archived": archived});
                if let Some(ref cursor) = cursor {
                    params["cursor"] = json!(cursor);
                }
                let page = match tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { cancelled = true; None },
                    result = self.request("thread/list", params) => Some(result),
                } {
                    None => {
                        errors.push("用户已取消列表刷新。".to_owned());
                        break;
                    }
                    Some(Ok(page)) => page,
                    Some(Err(error)) => {
                        errors.push(format!("列表分页读取失败：{}", error.description()));
                        break;
                    }
                };
                let Some(data) = page.get("data").and_then(Value::as_array) else {
                    errors.push("列表响应缺少 data 数组。".to_owned());
                    break;
                };
                let mut page_threads = Vec::new();
                for value in data {
                    if cancel.is_cancelled() {
                        cancelled = true;
                        break;
                    }
                    if value.get("ephemeral").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    if value
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| analysis_threads().lock().unwrap().contains(id))
                    {
                        continue;
                    }
                    let Some(mut thread) = parse_thread(value, archived, observed_at_unix_ms)
                    else {
                        errors.push("某条会话的元数据无效，已跳过。".to_owned());
                        continue;
                    };
                    match tokio::select! {
                        biased;
                        _ = cancel.cancelled() => { cancelled = true; None },
                        result = self
                        .request(
                            "thread/read",
                            json!({"threadId": thread.id, "includeTurns": false}),
                        )
                        => Some(result),
                    } {
                        None => break,
                        Some(Ok(_)) => {}
                        Some(Err(error)) => {
                            thread.read_error =
                                Some(format!("会话读取失败：{}", error.description()))
                        }
                    }
                    let replace = threads.get(&thread.id).is_none_or(|old| {
                        thread.updated_at > old.updated_at
                            || (thread.updated_at == old.updated_at && thread.archived)
                    });
                    if replace {
                        threads.insert(thread.id.clone(), thread.clone());
                    }
                    page_threads.push(thread);
                }
                if cancelled {
                    errors.push("用户已取消列表刷新。".to_owned());
                    break;
                }
                on_update(CollectionUpdate::Page(&page_threads))?;
                let next = match page.get("nextCursor") {
                    Some(Value::String(value)) => Some(value.clone()),
                    None | Some(Value::Null) => None,
                    _ => {
                        errors.push("列表响应的游标无效。".to_owned());
                        break;
                    }
                };
                match next {
                    Some(next) if seen_cursors.insert(next.clone()) => cursor = Some(next),
                    Some(_) => {
                        errors.push("列表分页游标重复，已停止采集。".to_owned());
                        break;
                    }
                    None => break,
                }
            }
            let scope = ListScopeStatus {
                archived,
                complete: errors.is_empty(),
                attempted_at_unix_ms: Some(observed_at_unix_ms),
                completed_at_unix_ms: errors.is_empty().then_some(observed_at_unix_ms),
                error: (!errors.is_empty()).then(|| errors.join(" ")),
            };
            on_update(CollectionUpdate::Scope(&scope))?;
            scopes.push(scope);
            if cancelled {
                break;
            }
        }
        Ok(Collection {
            threads: threads.into_values().collect(),
            scopes,
            cancelled,
        })
    }
}

fn parse_thread(value: &Value, archived: bool, observed_at_unix_ms: i64) -> Option<ThreadMetadata> {
    let string = |key| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let source = value.get("source")?;
    let (source_kind, source_detail) = if let Some(kind) = source.as_str() {
        (kind.to_owned(), None)
    } else if let Some(sub_agent) = source.get("subAgent") {
        let detail = sub_agent.as_str().map(str::to_owned).or_else(|| {
            sub_agent
                .as_object()
                .and_then(|object| object.keys().next().cloned())
        });
        ("subAgent".to_owned(), detail)
    } else if let Some(custom) = source.get("custom").and_then(Value::as_str) {
        ("custom".to_owned(), Some(custom.to_owned()))
    } else {
        return None;
    };
    let git = value
        .get("gitInfo")
        .filter(|git| git.is_object())
        .map(|git| GitMetadata {
            branch: git.get("branch").and_then(Value::as_str).map(str::to_owned),
            sha: git.get("sha").and_then(Value::as_str).map(str::to_owned),
            origin_url: git
                .get("originUrl")
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    Some(ThreadMetadata {
        id: string("id")?,
        session_id: string("sessionId")?,
        title: string("name"),
        preview: string("preview")?,
        cwd: string("cwd")?,
        project_id: string("projectId"),
        source_kind,
        source_detail,
        thread_source: string("threadSource"),
        parent_thread_id: string("parentThreadId"),
        forked_from_id: string("forkedFromId"),
        git,
        created_at: value.get("createdAt")?.as_i64()?,
        updated_at: value.get("updatedAt")?.as_i64()?,
        archived,
        metadata_complete: true,
        turns_complete: false,
        items_complete: false,
        missing_from_source: false,
        content_complete: false,
        read_error: None,
        observed_at_unix_ms,
    })
}

pub fn resolve_binary(choice: Option<&str>) -> Result<PathBuf, AppError> {
    let input = choice.filter(|s| !s.trim().is_empty()).unwrap_or("codex");
    let candidate = if input.contains('/') {
        let path = PathBuf::from(input);
        if !path.is_absolute() {
            return Err(AppError::codex(
                ErrorCode::BinaryUnavailable,
                "请输入二进制的绝对路径，或使用不含斜杠的命令名称。",
                true,
            ));
        }
        path
    } else {
        env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(input))
            .find(|path| is_executable(path))
            .ok_or_else(|| {
                AppError::codex(
                    ErrorCode::BinaryUnavailable,
                    "找不到 Codex 命令。请选择本机 codex 可执行文件。",
                    true,
                )
            })?
    };
    let resolved = candidate.canonicalize().map_err(|_| {
        AppError::codex(
            ErrorCode::BinaryUnavailable,
            "所选二进制不存在。请重新选择本机 codex 可执行文件。",
            true,
        )
    })?;
    if !is_executable(&resolved) {
        return Err(AppError::codex(
            ErrorCode::BinaryUnavailable,
            "所选文件不可执行。请检查路径和执行权限。",
            true,
        ));
    }
    Ok(resolved)
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub async fn diagnose(choice: Option<&str>) -> Result<Diagnosis, AppError> {
    diagnose_with_timeouts(choice, Duration::from_secs(3), Duration::from_secs(8)).await
}

async fn diagnose_with_timeouts(
    choice: Option<&str>,
    version_timeout: Duration,
    schema_timeout: Duration,
) -> Result<Diagnosis, AppError> {
    let binary = resolve_binary(choice)?;
    let resolved_binary = binary.to_string_lossy().into_owned();
    let version = read_version(&binary, version_timeout).await;
    let (mut session, experimental) = match Session::start(&binary).await {
        Ok(mut first) => match first.initialize(true).await {
            Ok(()) => (first, true),
            Err(ProbeError::Rpc(-32602, _)) => {
                first.close().await;
                let mut second = Session::start(&binary).await?;
                second.initialize(false).await.map_err(initialize_error)?;
                (second, false)
            }
            Err(e) => {
                first.close().await;
                return Err(initialize_error(e));
            }
        },
        Err(e) => return Err(e),
    };

    let mut capabilities = SourceCapabilities::default();
    let list_params = json!({"limit":1,"sourceKinds":SOURCES,"archived":false});
    let normal = session.request("thread/list", list_params).await;
    let archived = if normal.is_ok() {
        session
            .request(
                "thread/list",
                json!({"limit":1,"sourceKinds":SOURCES,"archived":true}),
            )
            .await
    } else {
        Err(ProbeError::NotRun)
    };
    if matches!(normal, Err(ProbeError::Exited)) || matches!(archived, Err(ProbeError::Exited)) {
        session.close().await;
        return Err(process_exited());
    }
    if normal.is_ok() && archived.is_ok() {
        capabilities.metadata =
            Capability::available("未归档和归档列表接口已响应；仅检查元数据，未采集会话内容。")
    } else if normal.as_ref().err().is_some_and(ProbeError::unsupported)
        || archived.as_ref().err().is_some_and(ProbeError::unsupported)
    {
        session.close().await;
        return Err(AppError::codex(
            ErrorCode::ProtocolIncompatible,
            "该二进制缺少会话列表最低能力。请升级 Codex 或选择其他二进制。",
            false,
        ));
    } else {
        session.close().await;
        return Err(AppError::codex(
            ErrorCode::SourceReadFailed,
            "会话列表探测失败。请重试连接，或检查 Codex 本机数据权限。",
            true,
        ));
    }

    capabilities.history = match session
        .request(
            "thread/read",
            json!({"threadId":PROBE_THREAD_ID,"includeTurns":false}),
        )
        .await
    {
        Err(ref error) if error.unsupported() => {
            Capability::unavailable("未提供 thread/read 接口。")
        }
        Err(ProbeError::Exited) => {
            session.close().await;
            return Err(process_exited());
        }
        Err(ProbeError::Timeout) => {
            Capability::unverified("thread/read 探测超时；未读取真实历史。")
        }
        _ => Capability::available("thread/read 接口已响应；真实历史内容兼容性由后续采集验证。"),
    };

    capabilities.experimental_history = if experimental {
        let turns = session
            .request(
                "thread/turns/list",
                json!({"threadId":PROBE_THREAD_ID,"limit":1}),
            )
            .await;
        let items = session
            .request(
                "thread/items/list",
                json!({"threadId":PROBE_THREAD_ID,"limit":1}),
            )
            .await;
        if matches!(turns, Err(ProbeError::Exited)) || matches!(items, Err(ProbeError::Exited)) {
            session.close().await;
            return Err(process_exited());
        }
        if turns.as_ref().err().is_some_and(ProbeError::unsupported)
            || items.as_ref().err().is_some_and(ProbeError::unsupported)
        {
            Capability::unavailable("实验性回合或条目分页当前不可用。")
        } else if matches!(turns, Err(ProbeError::Timeout))
            || matches!(items, Err(ProbeError::Timeout))
        {
            Capability::unverified("实验性分页接口探测超时。")
        } else {
            Capability::available("已启用实验接口，回合和条目分页方法均已响应；未读取真实内容。")
        }
    } else {
        Capability::unavailable("所选二进制不接受实验接口初始化选项。")
    };

    let analysis = inspect_analysis_schema(&binary, schema_timeout).await;
    capabilities.codex_summary = analysis.clone();
    capabilities.codex_naming = analysis;
    if session.exited()? {
        session.close().await;
        return Err(process_exited());
    }
    Ok(Diagnosis {
        resolved_binary,
        version,
        capabilities,
        session,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::fs::PermissionsExt, process::Command as StdCommand};

    fn fake_binary(mode: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!(
            "codexflow-test-{}-{nonce}-{}",
            std::process::id(),
            NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("fake-{mode}.py"));
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(include_bytes!("../tests/fixtures/fake_codex.py"))
            .unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .unwrap();
        path
    }

    #[tokio::test]
    async fn handshake_and_safe_capability_probe() {
        let path = fake_binary("ok");
        let diagnosis = diagnose(Some(path.to_str().unwrap())).await.unwrap();
        assert!(matches!(
            diagnosis.capabilities.metadata.state,
            codexflow_domain::CapabilityState::Available
        ));
        assert!(matches!(
            diagnosis.capabilities.history.state,
            codexflow_domain::CapabilityState::Available
        ));
        assert!(matches!(
            diagnosis.capabilities.experimental_history.state,
            codexflow_domain::CapabilityState::Available
        ));
        assert!(
            matches!(
                diagnosis.capabilities.codex_summary.state,
                codexflow_domain::CapabilityState::Available
            ),
            "{:?}: {:?}",
            path,
            diagnosis.capabilities.codex_summary
        );
        let mut session = diagnosis.session;
        session.close().await;
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn ephemeral_summary_requires_schema_and_completed_turn() {
        let path = fake_binary("analysis-ok");
        let mut events = Vec::new();
        let output = analyze_summary(
            Some(path.to_str().unwrap()),
            None,
            path.parent(),
            "受控摘要".into(),
            CancellationToken::new(),
            |event| {
                events.push(event);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(output.model, "test-model");
        assert!(output.text.contains("实现测试"));
        assert!(events.iter().any(
            |event| matches!(event, AnalysisEvent::Thread(id) if id == "temporary-analysis-thread")
        ));
        assert!(events.iter().any(
            |event| matches!(event, AnalysisEvent::Turn(id) if id == "temporary-analysis-turn")
        ));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn interrupt_uses_temporary_ids_and_rejects_late_completion() {
        for mode in ["analysis-cancel", "analysis-late"] {
            let path = fake_binary(mode);
            let token = CancellationToken::new();
            let trigger = token.clone();
            let result = analyze_summary(
                Some(path.to_str().unwrap()),
                None,
                path.parent(),
                "受控摘要".into(),
                token,
                |event| {
                    if matches!(event, AnalysisEvent::Turn(_)) {
                        trigger.cancel();
                    }
                    Ok(())
                },
            )
            .await;
            assert!(matches!(
                result,
                Err(AppError {
                    code: ErrorCode::AnalysisCancelled,
                    ..
                })
            ));
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[tokio::test]
    async fn tool_activity_fails_closed() {
        let path = fake_binary("analysis-tool");
        let result = analyze_summary(
            Some(path.to_str().unwrap()),
            None,
            path.parent(),
            "受控摘要".into(),
            CancellationToken::new(),
            |_| Ok(()),
        )
        .await;
        assert!(matches!(
            result,
            Err(AppError {
                code: ErrorCode::AnalysisUnavailable,
                ..
            })
        ));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn temporary_id_is_excluded_even_if_a_source_lists_it_as_regular() {
        let path = fake_binary("analysis-list-pollution");
        analyze_summary(
            Some(path.to_str().unwrap()),
            None,
            path.parent(),
            "受控摘要".into(),
            CancellationToken::new(),
            |_| Ok(()),
        )
        .await
        .unwrap();
        let mut session = Session::start(&path).await.unwrap();
        session.initialize(false).await.unwrap();
        let collection = session.collect_threads(300_000).await;
        assert_eq!(collection.threads.len(), 1);
        assert_eq!(collection.threads[0].id, "thread-h");
        session.close().await;
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn unsupported_configured_model_has_actionable_error() {
        let path = fake_binary("analysis-model-fail");
        let result = analyze_summary(
            Some(path.to_str().unwrap()),
            Some("unsupported"),
            path.parent(),
            "受控摘要".into(),
            CancellationToken::new(),
            |_| Ok(()),
        )
        .await;
        assert!(matches!(
            result,
            Err(AppError {
                code: ErrorCode::AnalysisModelUnsupported,
                ..
            })
        ));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn file_backed_auth_and_unsafe_effective_config_fail_before_model_turn() {
        let path = fake_binary("analysis-ok");
        let auth_home = path.parent().unwrap().join("auth-home");
        fs::create_dir(&auth_home).unwrap();
        fs::write(auth_home.join("auth.json"), "synthetic credential").unwrap();
        let blocked = analyze_summary(
            Some(path.to_str().unwrap()),
            None,
            Some(&auth_home),
            "受控摘要".into(),
            CancellationToken::new(),
            |_| Ok(()),
        )
        .await;
        assert!(matches!(
            blocked,
            Err(AppError {
                code: ErrorCode::AnalysisUnavailable,
                ..
            })
        ));
        let unsafe_binary = fake_binary("analysis-unsafe-config");
        let unsafe_result = analyze_summary(
            Some(unsafe_binary.to_str().unwrap()),
            None,
            unsafe_binary.parent(),
            "受控摘要".into(),
            CancellationToken::new(),
            |_| Ok(()),
        )
        .await;
        assert!(matches!(
            unsafe_result,
            Err(AppError {
                code: ErrorCode::AnalysisUnavailable,
                ..
            })
        ));
        assert!(!unsafe_binary
            .parent()
            .unwrap()
            .join("unexpected-thread-start")
            .exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
        let _ = fs::remove_dir_all(unsafe_binary.parent().unwrap());
    }

    #[tokio::test]
    async fn effective_unified_exec_must_be_confirmed_disabled_before_thread_start() {
        let path = fake_binary("analysis-unified-exec-on");
        let result = analyze_summary(
            Some(path.to_str().unwrap()),
            None,
            path.parent(),
            "受控摘要".into(),
            CancellationToken::new(),
            |_| Ok(()),
        )
        .await;
        assert!(matches!(
            result,
            Err(AppError {
                code: ErrorCode::AnalysisUnavailable,
                ..
            })
        ));
        assert!(!path
            .parent()
            .unwrap()
            .join("unexpected-thread-start")
            .exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn network_access_requires_explicit_false_before_model_turn() {
        for mode in [
            "network-missing",
            "network-null",
            "network-string",
            "network-true",
        ] {
            let path = fake_binary(&format!("analysis-{mode}"));
            let result = analyze_summary(
                Some(path.to_str().unwrap()),
                None,
                path.parent(),
                "受控摘要".into(),
                CancellationToken::new(),
                |_| Ok(()),
            )
            .await;
            assert!(
                matches!(
                    result,
                    Err(AppError {
                        code: ErrorCode::AnalysisUnavailable,
                        ..
                    })
                ),
                "{mode}"
            );
            assert!(
                !path
                    .parent()
                    .unwrap()
                    .join("unexpected-turn-start")
                    .exists(),
                "{mode}"
            );
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[tokio::test]
    #[ignore = "手动受控真实 Codex 冒烟，会调用模型"]
    async fn real_ephemeral_summary_does_not_enter_regular_history() {
        if let Some(reason) = analysis_isolation_issue(None) {
            eprintln!("跳过真实模型冒烟：{reason}");
            return;
        }
        let token = CancellationToken::new();
        let stop = token.clone();
        let mut temporary_id = None;
        let output = tokio::time::timeout(Duration::from_secs(180), analyze_summary(
            None,
            None,
            None,
            "仅使用此合成材料填写五个总结字段，evidenceIds 填空数组：目标是检查摘要接口；活动是阅读本句；结果是完成检查；决定和问题均为未知。禁止使用工具。".into(),
            token,
            |event| { if let AnalysisEvent::Thread(id) = event { temporary_id = Some(id); } Ok(()) },
        )).await;
        if output.is_err() {
            stop.cancel();
        }
        let output = output.expect("真实分析超时").expect("真实临时分析失败");
        let parsed: serde_json::Value = serde_json::from_str(&output.text).unwrap();
        assert!(parsed["goal"].is_string());
        let id = temporary_id.expect("临时会话标识");
        let binary = resolve_binary(None).unwrap();
        let mut regular = Session::start(&binary).await.unwrap();
        regular.initialize(false).await.unwrap();
        let mut cursor: Option<String> = None;
        loop {
            let mut params = json!({"limit":100,"sourceKinds":SOURCES,"archived":false});
            if let Some(value) = &cursor {
                params["cursor"] = json!(value);
            }
            let page = regular.request("thread/list", params).await.unwrap();
            assert!(!page["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value["id"] == id));
            cursor = page["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        regular.close().await;
    }

    #[tokio::test]
    #[ignore = "手动受控真实 Codex 中断冒烟，会启动模型回合"]
    async fn real_ephemeral_interrupt_reaches_terminal_state() {
        if let Some(reason) = analysis_isolation_issue(None) {
            eprintln!("跳过真实中断冒烟：{reason}");
            return;
        }
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let mut terminal = None;
        let result = analyze_summary(
            None,
            None,
            None,
            "这是一条合成的中断验证材料。请仅根据这句话填写总结字段。".into(),
            cancel,
            |event| {
                match event {
                    AnalysisEvent::Turn(_) => trigger.cancel(),
                    AnalysisEvent::Terminal(status) => terminal = Some(status),
                    _ => {}
                }
                Ok(())
            },
        )
        .await;
        assert!(matches!(
            result,
            Err(AppError {
                code: ErrorCode::AnalysisCancelled,
                ..
            })
        ));
        assert_eq!(terminal.as_deref(), Some("interrupted"));
    }

    #[tokio::test]
    async fn paginated_history_uses_item_entries_and_preserves_unknown_identity() {
        let path = fake_binary("history-paged");
        let mut session = Session::start(&path).await.unwrap();
        session.initialize(true).await.unwrap();
        let history = session.collect_history("thread-h", 200, 300_000).await;
        assert!(history.coverage.turns_complete && history.coverage.items_complete);
        assert_eq!(
            history.coverage.path,
            codexflow_domain::HistoryReadPath::Paginated
        );
        assert_eq!(
            (history.coverage.turn_pages, history.coverage.item_pages),
            (2, 2)
        );
        assert_eq!(history.turns.len(), 2);
        assert_eq!(history.items.len(), 2);
        assert_eq!(history.items[0].turn_id, "turn-1");
        assert_eq!(history.items[0].text.as_deref(), Some("完整正文 item-1"));
        assert_eq!(history.items[1].turn_id, "turn-2");
        assert_eq!(history.items[1].id, "unknown-2");
        assert!(!history.items[1].supported);
        assert!(history.items.iter().all(|item| item.id != "summary-only"));
        session.close().await;
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn legacy_full_read_and_summary_only_history_are_distinguished() {
        for (mode, complete) in [("history-legacy", true), ("history-summary", false)] {
            let path = fake_binary(mode);
            let mut session = Session::start(&path).await.unwrap();
            session.initialize(true).await.unwrap();
            let history = session.collect_history("thread-h", 200, 300_000).await;
            assert_eq!(history.coverage.items_complete, complete);
            assert_eq!(history.items.len(), if complete { 2 } else { 0 });
            if complete {
                assert_eq!(
                    history.coverage.path,
                    codexflow_domain::HistoryReadPath::FullRead
                );
            } else {
                assert!(history.coverage.error.is_some());
            }
            session.close().await;
            let _ = fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[tokio::test]
    async fn interrupted_item_page_keeps_partial_content_and_next_thread_readable() {
        let path = fake_binary("history-partial");
        let mut session = Session::start(&path).await.unwrap();
        session.initialize(true).await.unwrap();
        let partial = session.collect_history("thread-h", 200, 300_000).await;
        assert!(partial.coverage.turns_complete);
        assert!(!partial.coverage.items_complete);
        assert_eq!(partial.items.len(), 1);
        assert_eq!(partial.coverage.loaded_items, 1);
        let failed = session.collect_history("thread-bad", 200, 300_001).await;
        assert!(failed.coverage.error.is_some());
        assert!(failed.items.is_empty());
        let again = session.collect_history("thread-h", 200, 300_002).await;
        assert_eq!(again.items.len(), 1);
        session.close().await;
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn cancelling_a_partial_response_keeps_the_session_readable() {
        use tokio::time::{sleep, timeout};

        let path = fake_binary("cancel-partial");
        let marker_dir = path.parent().unwrap();
        let started = marker_dir.join("partial-response-started");
        let resume = marker_dir.join("resume-partial-response");
        let mut session = Session::start(&path).await.unwrap();
        session.initialize(true).await.unwrap();
        for request_number in [1, 2] {
            let result = session
                .request("thread/list", json!({"limit": 1}))
                .await
                .unwrap();
            assert_eq!(result["marker"], format!("request-{request_number}"));
        }

        let cancel = CancellationToken::new();
        let mut request = Box::pin(session.request("thread/list", json!({"limit": 1})));
        tokio::select! {
            biased;
            result = &mut request => panic!("partial response unexpectedly completed: {result:?}"),
            _ = async {
                timeout(Duration::from_secs(2), async {
                    while !started.exists() {
                        sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                cancel.cancel();
                cancel.cancelled().await;
            } => {}
        }
        drop(request);
        assert!(!session.pending_line.is_empty());
        assert!(!session.pending_line.ends_with(b"\n"));

        fs::write(&resume, "").unwrap();
        let result = timeout(
            Duration::from_secs(2),
            session.request("thread/list", json!({"limit": 1})),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["marker"], "request-4");
        assert!(session.pending_line.is_empty());

        session.close().await;
        let _ = fs::remove_dir_all(marker_dir);
    }

    #[tokio::test]
    async fn missing_list_is_incompatible() {
        let path = fake_binary("missing-list");
        let error = diagnose(Some(path.to_str().unwrap()))
            .await
            .err()
            .expect("error");
        assert!(matches!(error.code, ErrorCode::ProtocolIncompatible));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn older_unknown_method_error_is_incompatible() {
        let path = fake_binary("unknown-method");
        let error = diagnose(Some(path.to_str().unwrap()))
            .await
            .err()
            .expect("error");
        assert!(matches!(error.code, ErrorCode::ProtocolIncompatible));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn exit_after_handshake_is_reported() {
        let path = fake_binary("exit");
        let error = diagnose(Some(path.to_str().unwrap()))
            .await
            .err()
            .expect("error");
        assert!(matches!(error.code, ErrorCode::ProcessExited));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn invalid_binary_is_actionable() {
        let error = diagnose(Some("/no/such/codexflow-test-binary"))
            .await
            .err()
            .expect("error");
        assert!(matches!(error.code, ErrorCode::BinaryUnavailable));
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn timed_out_auxiliary_processes_are_reaped_across_retries() {
        let path = fake_binary("slow-aux");
        for _ in 0..2 {
            let mut diagnosis = diagnose_with_timeouts(
                Some(path.to_str().unwrap()),
                Duration::from_millis(500),
                Duration::from_millis(500),
            )
            .await
            .unwrap();
            assert!(diagnosis.version.is_none());
            assert!(matches!(
                diagnosis.capabilities.codex_summary.state,
                codexflow_domain::CapabilityState::NotVerified
            ));
            diagnosis.session.close().await;
        }

        let marker = fs::read_to_string(path.parent().unwrap().join("auxiliary-pids.txt")).unwrap();
        let pids: Vec<&str> = marker
            .lines()
            .map(|line| line.split_once(' ').unwrap().1)
            .collect();
        assert_eq!(
            pids.len(),
            4,
            "two retries must launch both auxiliary probes"
        );
        let mut lingering = Vec::new();
        for pid in pids {
            let alive = StdCommand::new("/bin/kill")
                .args(["-0", pid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            if alive {
                lingering.push(pid);
                let _ = StdCommand::new("/bin/kill").args(["-9", pid]).status();
            }
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
        assert!(
            lingering.is_empty(),
            "auxiliary processes still running: {lingering:?}"
        );
    }
}

async fn read_version(binary: &Path, limit: Duration) -> Option<String> {
    let mut command = Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    AuxiliaryGroup::configure(&mut command);
    let mut child = command.spawn().ok()?;
    let mut group = AuxiliaryGroup::for_child(&child);
    let stdout = child.stdout.take().expect("piped stdout");
    let mut bytes = Vec::new();
    let result = timeout(limit, async {
        let mut limited_stdout = stdout.take(4096);
        let (exit, read) = tokio::join!(child.wait(), limited_stdout.read_to_end(&mut bytes));
        (exit, read)
    })
    .await;
    let (exit, read) = match result {
        Ok(result) => result,
        Err(_) => {
            terminate_and_reap(&mut child, &mut group).await;
            return None;
        }
    };
    let exit = match exit {
        Ok(status) => status,
        Err(_) => {
            terminate_and_reap(&mut child, &mut group).await;
            return None;
        }
    };
    group.kill();
    if !exit.success() || read.is_err() {
        return None;
    }
    let version = String::from_utf8_lossy(&bytes).trim().to_owned();
    (!version.is_empty()).then_some(version)
}

async fn inspect_analysis_schema(binary: &Path, limit: Duration) -> Capability {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = env::temp_dir().join(format!(
        "codexflow-schema-{}-{nonce}-{}",
        std::process::id(),
        NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut command = Command::new(binary);
    command
        .args([
            "app-server",
            "generate-json-schema",
            "--experimental",
            "--out",
        ])
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    AuxiliaryGroup::configure(&mut command);
    let output = command.spawn();
    let succeeded = if let Ok(mut child) = output {
        let mut group = AuxiliaryGroup::for_child(&child);
        let succeeded = match timeout(limit, child.wait()).await {
            Ok(Ok(status)) => status.success(),
            Ok(Err(_)) => {
                terminate_and_reap(&mut child, &mut group).await;
                false
            }
            Err(_) => {
                terminate_and_reap(&mut child, &mut group).await;
                false
            }
        };
        group.kill();
        succeeded
    } else {
        false
    };
    let available = if succeeded {
        let start = fs::read_to_string(dir.join("v2/ThreadStartParams.json")).ok();
        let turn = fs::read_to_string(dir.join("v2/TurnStartParams.json")).ok();
        match (start, turn) {
            (Some(start), Some(turn)) => {
                let start: Option<Value> = serde_json::from_str(&start).ok();
                let turn: Option<Value> = serde_json::from_str(&turn).ok();
                start
                    .as_ref()
                    .and_then(|v| v.pointer("/properties/ephemeral"))
                    .is_some()
                    && turn
                        .as_ref()
                        .and_then(|v| v.pointer("/properties/outputSchema"))
                        .is_some()
            }
            _ => false,
        }
    } else {
        false
    };
    let _ = fs::remove_dir_all(&dir);
    if available {
        Capability::available("所选二进制导出的协议包含临时会话和结构化输出；未调用模型。")
    } else {
        Capability::unverified("无法从所选二进制确认临时会话与结构化输出；未调用模型。")
    }
}

struct AuxiliaryGroup {
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl AuxiliaryGroup {
    fn configure(command: &mut Command) {
        #[cfg(unix)]
        command.as_std_mut().process_group(0);
        #[cfg(not(unix))]
        let _ = command;
    }

    fn for_child(child: &Child) -> Self {
        #[cfg(unix)]
        {
            Self {
                pgid: child.id().map(|pid| pid as i32),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child;
            Self {}
        }
    }

    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
    }
}

impl Drop for AuxiliaryGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

async fn terminate_and_reap(child: &mut Child, group: &mut AuxiliaryGroup) {
    group.kill();
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn initialize_error(error: ProbeError) -> AppError {
    match error {
        ProbeError::Exited => process_exited(),
        ProbeError::Rpc(_, true) => AppError::codex(
            ErrorCode::ProtocolIncompatible,
            "该二进制不支持 app-server 初始化。请升级 Codex 或选择其他二进制。",
            false,
        ),
        _ => AppError::codex(
            ErrorCode::InitializeFailed,
            "app-server 初始化失败。请重试连接或选择其他二进制。",
            true,
        ),
    }
}

fn process_exited() -> AppError {
    AppError::codex(
        ErrorCode::ProcessExited,
        "Codex app-server 已退出。请检查所选二进制并重试连接。",
        true,
    )
}

#[derive(Debug)]
enum ProbeError {
    Rpc(i64, bool),
    Timeout,
    Exited,
    InvalidResponse,
    NotRun,
}

impl ProbeError {
    fn unsupported(&self) -> bool {
        matches!(self, Self::Rpc(_, true))
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Rpc(_, true) => "当前二进制不支持所需接口",
            Self::Rpc(_, false) => "Codex 返回错误",
            Self::Timeout => "请求超时",
            Self::Exited => "app-server 已退出",
            Self::InvalidResponse => "响应格式无效",
            Self::NotRun => "请求未执行",
        }
    }
}

pub struct Session {
    child: Child,
    group: AuxiliaryGroup,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    // Keep read_until's partial bytes when its request future is cancelled.
    pending_line: Vec<u8>,
    pending_events: VecDeque<Value>,
    next_id: u64,
}

impl Session {
    async fn start(binary: &Path) -> Result<Self, AppError> {
        Self::start_with_home(binary, None, None).await
    }

    async fn start_with_home(
        binary: &Path,
        home: Option<&Path>,
        cwd: Option<&Path>,
    ) -> Result<Self, AppError> {
        let mut command = Command::new(binary);
        command
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(home) = home {
            command
                .env_clear()
                .env("CODEX_HOME", home)
                .env("HOME", cwd.unwrap_or(home))
                .env("PATH", env::var_os("PATH").unwrap_or_default())
                .env("LANG", env::var_os("LANG").unwrap_or_default())
                .current_dir(cwd.unwrap_or(home));
        }
        AuxiliaryGroup::configure(&mut command);
        let mut child = command.spawn().map_err(|_| {
            AppError::codex(
                ErrorCode::SpawnFailed,
                "启动 Codex app-server 失败。请检查二进制执行权限。",
                true,
            )
        })?;
        let group = AuxiliaryGroup::for_child(&child);
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(Self {
            child,
            group,
            stdin,
            stdout,
            pending_line: Vec::new(),
            pending_events: VecDeque::new(),
            next_id: 1,
        })
    }

    async fn initialize(&mut self, experimental: bool) -> Result<(), ProbeError> {
        let params = if experimental {
            json!({"clientInfo":{"name":"CodexFlow","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})
        } else {
            json!({"clientInfo":{"name":"CodexFlow","version":env!("CARGO_PKG_VERSION")}})
        };
        let response = self.request("initialize", params).await?;
        if !response.get("userAgent").is_some_and(Value::is_string) {
            return Err(ProbeError::InvalidResponse);
        }
        self.write(&json!({"method":"initialized"})).await?;
        Ok(())
    }

    async fn write(&mut self, message: &Value) -> Result<(), ProbeError> {
        let mut bytes = serde_json::to_vec(message).map_err(|_| ProbeError::InvalidResponse)?;
        bytes.push(b'\n');
        timeout(Duration::from_secs(8), self.stdin.write_all(&bytes))
            .await
            .map_err(|_| ProbeError::Timeout)?
            .map_err(|_| ProbeError::Exited)
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, ProbeError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"id":id,"method":method,"params":params}))
            .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        loop {
            let count = timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                self.stdout.read_until(b'\n', &mut self.pending_line),
            )
            .await
            .map_err(|_| ProbeError::Timeout)?
            .map_err(|_| ProbeError::Exited)?;
            if count == 0 {
                return Err(ProbeError::Exited);
            }
            let line = std::mem::take(&mut self.pending_line);
            let line = std::str::from_utf8(&line).map_err(|_| ProbeError::InvalidResponse)?;
            let response: Value =
                serde_json::from_str(line).map_err(|_| ProbeError::InvalidResponse)?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
                if response.get("method").is_some() && response.get("id").is_some() {
                    self.write(&json!({"id":response["id"],"error":{"code":-32000,"message":"CodexFlow does not approve server requests"}})).await?;
                    return Err(ProbeError::InvalidResponse);
                }
                if response.get("method").is_some() {
                    if self.pending_events.len() >= 512 {
                        self.pending_events.pop_front();
                    }
                    self.pending_events.push_back(response);
                }
                continue;
            }
            if let Some(error) = response.get("error") {
                let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32000);
                let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                let unsupported = code == -32601
                    || message.contains("unknown variant")
                    || message.contains("not supported yet");
                return Err(ProbeError::Rpc(code, unsupported));
            }
            return response
                .get("result")
                .cloned()
                .ok_or(ProbeError::InvalidResponse);
        }
    }

    pub fn exited(&mut self) -> Result<bool, AppError> {
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|_| process_exited())
    }

    pub async fn close(&mut self) {
        self.group.kill();
        let _ = self.child.start_kill();
        let _ = timeout(Duration::from_secs(3), self.child.wait()).await;
    }
}
