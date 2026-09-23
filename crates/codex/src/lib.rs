use codexflow_domain::{AppError, Capability, ErrorCode, SourceCapabilities};
use serde_json::{json, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

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

pub struct Diagnosis {
    pub resolved_binary: String,
    pub version: Option<String>,
    pub capabilities: SourceCapabilities,
    pub session: Session,
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
    let binary = resolve_binary(choice)?;
    let resolved_binary = binary.to_string_lossy().into_owned();
    let version = read_version(&binary).await;
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

    let analysis = inspect_analysis_schema(&binary).await;
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
    use std::{io::Write, os::unix::fs::PermissionsExt};

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
}

async fn read_version(binary: &Path) -> Option<String> {
    let output = timeout(
        Duration::from_secs(3),
        Command::new(binary)
            .arg("--version")
            .stderr(Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!version.is_empty()).then_some(version)
}

async fn inspect_analysis_schema(binary: &Path) -> Capability {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = env::temp_dir().join(format!(
        "codexflow-schema-{}-{nonce}-{}",
        std::process::id(),
        NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
    ));
    let output = timeout(
        Duration::from_secs(8),
        Command::new(binary)
            .args([
                "app-server",
                "generate-json-schema",
                "--experimental",
                "--out",
            ])
            .arg(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
    let available = if matches!(output, Ok(Ok(status)) if status.success()) {
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
}

pub struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Session {
    async fn start(binary: &Path) -> Result<Self, AppError> {
        let mut child = Command::new(binary)
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                AppError::codex(
                    ErrorCode::SpawnFailed,
                    "启动 Codex app-server 失败。请检查二进制执行权限。",
                    true,
                )
            })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(Self {
            child,
            stdin,
            stdout,
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
            let mut line = String::new();
            let count = timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                self.stdout.read_line(&mut line),
            )
            .await
            .map_err(|_| ProbeError::Timeout)?
            .map_err(|_| ProbeError::Exited)?;
            if count == 0 {
                return Err(ProbeError::Exited);
            }
            let response: Value =
                serde_json::from_str(&line).map_err(|_| ProbeError::InvalidResponse)?;
            if response.get("id").and_then(Value::as_u64) != Some(id) {
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
        let _ = self.child.start_kill();
        let _ = timeout(Duration::from_secs(3), self.child.wait()).await;
    }
}
