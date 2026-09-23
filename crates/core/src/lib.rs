mod projects;

use codexflow_codex::{diagnose, Session};
use codexflow_domain::{
    AppError, ConnectionState, DisplayTheme, ErrorCode, JevConfig, JevConnectionResult,
    JevInferenceResult, JevStatus, Preferences, ProjectCatalog, ProjectSessions, SessionList,
    SourceStatus,
};
use codexflow_jev::{
    normalize_base_url, system_credentials, Credential, CredentialStore, JevClient,
};
use codexflow_store::{PreferenceStore, SessionStore};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, RwLock, RwLockWriteGuard};
use tokio_util::sync::CancellationToken;

pub struct SourceService {
    store: PreferenceStore,
    sessions: SessionStore,
    // Source I/O may hold state for a full refresh; settings must remain
    // available to Jev requests, cancellation and credential deletion.
    preferences: Mutex<Preferences>,
    state: Mutex<State>,
    credentials: Arc<dyn CredentialStore>,
    project_updates: std::sync::Mutex<()>,
    jev_gate: RwLock<()>,
    jev_cancel: Mutex<CancellationToken>,
}

struct State {
    status: SourceStatus,
    session: Option<Session>,
}

impl SourceService {
    pub fn new(app_data_dir: PathBuf) -> Result<Self, AppError> {
        Self::with_credentials(app_data_dir, system_credentials())
    }

    pub fn with_credentials(
        app_data_dir: PathBuf,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self, AppError> {
        let store = PreferenceStore::new(app_data_dir.clone());
        let sessions = SessionStore::new(app_data_dir)?;
        let preferences = store.load()?;
        let status = SourceStatus::new(preferences.selected_binary.clone());
        let service = Self {
            store,
            sessions,
            preferences: Mutex::new(preferences),
            state: Mutex::new(State {
                status,
                session: None,
            }),
            credentials,
            project_updates: std::sync::Mutex::new(()),
            jev_gate: RwLock::new(()),
            jev_cancel: Mutex::new(CancellationToken::new()),
        };
        service.reconcile_projects()?;
        Ok(service)
    }

    pub async fn jev_status(&self) -> Result<JevStatus, AppError> {
        let config = self.preferences.lock().await.jev.clone();
        let (credential_configured, credential_error) = match self.credentials.load() {
            Ok(credential) => (
                credential
                    .as_ref()
                    .is_some_and(|item| item.base_url == config.base_url),
                None,
            ),
            Err(error) => (false, Some(error)),
        };
        Ok(JevStatus {
            credential_configured,
            credential_error,
            config,
        })
    }

    pub async fn save_jev(
        &self,
        base_url: String,
        model: String,
        api_key: Option<String>,
    ) -> Result<JevStatus, AppError> {
        let base_url = normalize_base_url(&base_url)?;
        let model = model.trim().to_owned();
        if model.is_empty() {
            return Err(AppError::jev(
                ErrorCode::JevModelUnsupported,
                "请填写 Jev 模型 ID。",
                false,
            ));
        }
        if api_key
            .as_ref()
            .is_some_and(|key| key.trim().is_empty() || key.chars().any(char::is_control))
        {
            return Err(AppError::jev(
                ErrorCode::JevNotConfigured,
                "API Key 不能为空。",
                false,
            ));
        }
        let _gate = self.stop_jev().await;
        let mut preferences = self.preferences.lock().await;
        if preferences.jev.base_url != base_url && api_key.is_none() {
            return Err(AppError::jev(
                ErrorCode::JevNotConfigured,
                "更换服务地址时请填写新 API Key；删除旧密钥需单独操作。",
                false,
            ));
        }
        if let Some(key) = api_key {
            self.credentials.save(&Credential {
                base_url: base_url.clone(),
                key: key.trim().to_owned(),
            })?;
        }
        let mut next = preferences.clone();
        next.jev = JevConfig {
            base_url: base_url.clone(),
            model,
        };
        self.store.save(&next)?;
        *preferences = next;
        drop(preferences);
        self.jev_status().await
    }

    pub async fn delete_jev_credential(&self) -> Result<JevStatus, AppError> {
        let _gate = self.stop_jev().await;
        self.credentials.delete()?;
        self.jev_status().await
    }

    pub async fn cancel_jev(&self) {
        let _gate = self.stop_jev().await;
    }

    async fn stop_jev(&self) -> RwLockWriteGuard<'_, ()> {
        self.jev_cancel.lock().await.cancel();
        let gate = self.jev_gate.write().await;
        *self.jev_cancel.lock().await = CancellationToken::new();
        gate
    }

    pub async fn check_jev_connection(&self) -> Result<JevConnectionResult, AppError> {
        let _gate = self.jev_gate.read().await;
        let token = self.jev_cancel.lock().await.clone();
        tokio::select! {
            biased;
            _ = token.cancelled() => Err(jev_cancelled()),
            result = async {
                let (credential, model) = self.jev_request_settings().await?;
                JevClient::new()?.check_connection(&credential, &model).await
            } => result,
        }
    }

    pub async fn test_jev_inference(&self) -> Result<JevInferenceResult, AppError> {
        let _gate = self.jev_gate.read().await;
        let token = self.jev_cancel.lock().await.clone();
        tokio::select! {
            biased;
            _ = token.cancelled() => Err(jev_cancelled()),
            result = async {
                let (credential, model) = self.jev_request_settings().await?;
                JevClient::new()?.test_inference(&credential, &model).await
            } => result,
        }
    }

    async fn jev_request_settings(&self) -> Result<(Credential, String), AppError> {
        let config = self.preferences.lock().await.jev.clone();
        let credential = self
            .credentials
            .load()?
            .filter(|item| item.base_url == config.base_url)
            .ok_or_else(|| {
                AppError::jev(
                    ErrorCode::JevNotConfigured,
                    "请先保存当前服务地址的 Jev API Key。",
                    false,
                )
            })?;
        Ok((credential, config.model))
    }

    pub async fn settings(&self) -> (DisplayTheme, SourceStatus) {
        let mut state = self.state.lock().await;
        check_process(&mut state);
        (
            self.preferences.lock().await.theme.clone(),
            state.status.clone(),
        )
    }

    pub async fn status(&self) -> SourceStatus {
        let mut state = self.state.lock().await;
        check_process(&mut state);
        state.status.clone()
    }

    pub async fn set_theme(&self, theme: DisplayTheme) -> Result<DisplayTheme, AppError> {
        let mut preferences = self.preferences.lock().await;
        let next = Preferences {
            selected_binary: preferences.selected_binary.clone(),
            theme,
            jev: preferences.jev.clone(),
        };
        self.store.save(&next)?;
        *preferences = next;
        Ok(preferences.theme.clone())
    }

    pub async fn connect(&self, selected_binary: Option<String>) -> Result<SourceStatus, AppError> {
        let mut state = self.state.lock().await;
        let choice = selected_binary
            .map(|path| path.trim().to_owned())
            .filter(|path| !path.is_empty());
        {
            let mut preferences = self.preferences.lock().await;
            let next = Preferences {
                selected_binary: choice.clone(),
                theme: preferences.theme.clone(),
                jev: preferences.jev.clone(),
            };
            self.store.save(&next)?;
            *preferences = next;
        }
        if let Some(mut previous) = state.session.take() {
            previous.close().await;
        }
        state.status = SourceStatus::new(choice.clone());
        state.status.connection = ConnectionState::Connecting;
        match diagnose(choice.as_deref()).await {
            Ok(result) => {
                state.status.resolved_binary = Some(result.resolved_binary);
                state.status.version = result.version;
                state.status.capabilities = result.capabilities;
                state.status.connection = ConnectionState::Connected;
                state.session = Some(result.session);
            }
            Err(error) => {
                state.status.connection = ConnectionState::Failed;
                state.status.error = Some(error);
            }
        }
        state.status.checked_at_unix_ms = Some(now_ms());
        Ok(state.status.clone())
    }

    pub async fn shutdown(&self) {
        self.cancel_jev().await;
        let mut state = self.state.lock().await;
        if let Some(mut session) = state.session.take() {
            session.close().await;
        }
    }

    pub fn cached_sessions(&self) -> Result<SessionList, AppError> {
        self.sessions.list()
    }

    fn reconcile_projects(&self) -> Result<(), AppError> {
        self.reconcile_projects_with(|| {})
    }

    // Keep the hook inside the critical section so tests can pause a computed
    // reconciliation before its write and exercise the read/compute/write race.
    fn reconcile_projects_with(&self, before_save: impl FnOnce()) -> Result<(), AppError> {
        let _updates = self.lock_project_updates();
        self.reconcile_projects_locked(before_save)
    }

    fn reconcile_projects_locked(&self, before_save: impl FnOnce()) -> Result<(), AppError> {
        let threads = self.sessions.list()?.threads;
        let projects = self.sessions.projects()?;
        let previous = self.sessions.attributions()?;
        let (projects, attributions) = projects::reconcile(&threads, projects, &previous);
        before_save();
        self.sessions
            .save_projects_and_attributions(&projects, &attributions)
    }

    fn lock_project_updates(&self) -> std::sync::MutexGuard<'_, ()> {
        self.project_updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn project_catalog(&self) -> Result<ProjectCatalog, AppError> {
        self.sessions.catalog()
    }

    pub fn project_sessions(&self, project_id: &str) -> Result<ProjectSessions, AppError> {
        self.sessions.project_sessions(project_id)
    }

    pub fn choose_project(&self, path: &str) -> Result<ProjectCatalog, AppError> {
        let project = projects::selected_project(path).map_err(AppError::project)?;
        let _updates = self.lock_project_updates();
        self.sessions
            .save_projects_and_attributions(&[project.clone()], &[])?;
        self.reconcile_projects_locked(|| {})?;
        self.sessions.select_project(&project.id)?;
        self.sessions.catalog()
    }

    pub fn choose_existing_project(&self, project_id: &str) -> Result<ProjectCatalog, AppError> {
        let _updates = self.lock_project_updates();
        self.sessions.select_project(project_id)?;
        self.sessions.catalog()
    }

    pub async fn refresh_sessions(&self) -> Result<SessionList, AppError> {
        let mut state = self.state.lock().await;
        let attempted_at = now_ms() as i64;
        self.sessions.begin_refresh(attempted_at)?;
        check_process(&mut state);
        let session = match state.session.as_mut() {
            Some(session) => session,
            None => {
                self.sessions
                    .fail_refresh(attempted_at, "来源当前不可用；旧缓存已保留。")?;
                return Err(AppError::codex(
                    ErrorCode::SourceReadFailed,
                    "Codex 来源当前不可用。请先连接，已有会话缓存仍可浏览。",
                    true,
                ));
            }
        };
        let collection = session.collect_threads(attempted_at).await;
        if let Err(error) = self
            .sessions
            .save_collection(&collection.threads, &collection.scopes)
        {
            let _ = self
                .sessions
                .fail_refresh(attempted_at, "保存会话列表失败；旧缓存已保留。");
            return Err(error);
        }
        self.reconcile_projects()?;
        self.sessions.list()
    }
}

fn jev_cancelled() -> AppError {
    AppError::jev(
        ErrorCode::JevCancelled,
        "Jev 本地请求已取消；远端计算或计费可能仍在进行。",
        false,
    )
}

fn check_process(state: &mut State) {
    if let Some(session) = state.session.as_mut() {
        match session.exited() {
            Ok(false) => return,
            _ => {
                state.session = None;
                state.status.connection = ConnectionState::Failed;
                state.status.error = Some(AppError::codex(
                    ErrorCode::ProcessExited,
                    "Codex app-server 已退出。请重试连接。",
                    true,
                ));
                state.status.checked_at_unix_ms = Some(now_ms());
            }
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write, os::unix::fs::PermissionsExt};

    #[derive(Default)]
    struct MemoryCredentials(std::sync::Mutex<Option<(String, String)>>);

    impl CredentialStore for MemoryCredentials {
        fn load(&self) -> Result<Option<Credential>, AppError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .as_ref()
                .map(|(url, key)| Credential {
                    base_url: url.clone(),
                    key: key.clone(),
                }))
        }
        fn save(&self, credential: &Credential) -> Result<(), AppError> {
            *self.0.lock().unwrap() = Some((credential.base_url.clone(), credential.key.clone()));
            Ok(())
        }
        fn delete(&self) -> Result<(), AppError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    struct UnavailableCredentials;
    impl CredentialStore for UnavailableCredentials {
        fn load(&self) -> Result<Option<Credential>, AppError> {
            Err(AppError::jev(
                ErrorCode::JevCredentialFailed,
                "钥匙串不可用。",
                true,
            ))
        }
        fn save(&self, _: &Credential) -> Result<(), AppError> {
            Err(AppError::jev(
                ErrorCode::JevCredentialFailed,
                "钥匙串不可用。",
                true,
            ))
        }
        fn delete(&self) -> Result<(), AppError> {
            Err(AppError::jev(
                ErrorCode::JevCredentialFailed,
                "钥匙串不可用。",
                true,
            ))
        }
    }

    fn temp_data_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "codexflow-jev-test-{}-{nonce}-{id}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn jev_credentials_are_separate_from_persisted_settings_and_scoped_to_address() {
        let dir = temp_data_dir();
        let credentials = Arc::new(MemoryCredentials::default());
        let service = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        let first = service
            .save_jev(
                "https://api.typesafe.ai/".into(),
                "jev-1.13.0".into(),
                Some("synthetic-secret-1".into()),
            )
            .await
            .unwrap();
        assert!(first.credential_configured);
        assert_eq!(first.config.base_url, "https://api.typesafe.ai");
        let persisted = fs::read_to_string(dir.join("preferences.json")).unwrap();
        assert!(!persisted.contains("synthetic-secret"));
        drop(service);

        let reopened = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        assert!(reopened.jev_status().await.unwrap().credential_configured);
        assert!(reopened
            .save_jev("https://other.example".into(), "jev-latest".into(), None)
            .await
            .is_err());
        assert_eq!(
            reopened.jev_status().await.unwrap().config.base_url,
            "https://api.typesafe.ai"
        );
        assert!(reopened.jev_status().await.unwrap().credential_configured);
        reopened
            .save_jev(
                "https://other.example".into(),
                "jev-latest".into(),
                Some("synthetic-secret-2".into()),
            )
            .await
            .unwrap();
        assert!(reopened.jev_status().await.unwrap().credential_configured);
        reopened.delete_jev_credential().await.unwrap();
        assert!(!reopened.jev_status().await.unwrap().credential_configured);
        assert!(matches!(
            reopened.test_jev_inference().await.unwrap_err().code,
            ErrorCode::JevNotConfigured
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deleting_credential_cancels_in_flight_http_request() {
        use std::{net::TcpListener, sync::mpsc, thread, time::Duration};
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (seen_tx, seen_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            seen_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(300));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"models\":[]}");
        });
        let dir = temp_data_dir();
        let service = Arc::new(
            SourceService::with_credentials(dir.clone(), Arc::new(MemoryCredentials::default()))
                .unwrap(),
        );
        service
            .save_jev(base, "jev-latest".into(), Some("synthetic-only-key".into()))
            .await
            .unwrap();
        let running = {
            let service = service.clone();
            tokio::spawn(async move { service.check_jev_connection().await })
        };
        tokio::task::spawn_blocking(move || seen_rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .await
            .unwrap();
        service.delete_jev_credential().await.unwrap();
        let outcome = running.await.unwrap().unwrap_err();
        assert!(matches!(outcome.code, ErrorCode::JevCancelled));
        assert!(matches!(
            service.check_jev_connection().await.unwrap_err().code,
            ErrorCode::JevNotConfigured
        ));
        server.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn keychain_failure_never_falls_back_to_plaintext_storage() {
        let dir = temp_data_dir();
        let service =
            SourceService::with_credentials(dir.clone(), Arc::new(UnavailableCredentials)).unwrap();
        let error = service
            .save_jev(
                "https://api.typesafe.ai".into(),
                "jev-latest".into(),
                Some("synthetic-only-key".into()),
            )
            .await
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::JevCredentialFailed));
        assert!(!dir.join("preferences.json").exists());
        let status = service.jev_status().await.unwrap();
        assert_eq!(status.config.base_url, "https://api.typesafe.ai");
        assert!(!status.credential_configured);
        assert!(matches!(
            status.credential_error.unwrap().code,
            ErrorCode::JevCredentialFailed
        ));
    }

    async fn stop_jev_during_refresh(inference: bool, delete: bool) {
        use std::{future::Future, net::TcpListener, task::Poll, time::Duration};
        use tokio::time::{sleep, timeout};

        let dir = temp_data_dir();
        fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("fake-gated-list-rich.py");
        fs::write(
            &binary,
            include_bytes!("../../codex/tests/fixtures/fake_codex.py"),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        // Keep HTTP pending without relying on an external service or response timing.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let credentials = Arc::new(MemoryCredentials::default());
        let service =
            Arc::new(SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap());
        service
            .save_jev(
                format!("http://{}", listener.local_addr().unwrap()),
                "jev-latest".into(),
                Some("synthetic-only-key".into()),
            )
            .await
            .unwrap();
        let source = service
            .connect(Some(binary.to_string_lossy().into_owned()))
            .await
            .unwrap();
        assert!(matches!(source.connection, ConnectionState::Connected));
        assert_eq!(service.refresh_sessions().await.unwrap().threads.len(), 4);
        let pause = dir.join("pause-refresh");
        fs::write(&pause, b"").unwrap();
        let refresh = {
            let service = service.clone();
            tokio::spawn(async move { service.refresh_sessions().await })
        };
        timeout(Duration::from_secs(2), async {
            while !dir.join("refresh-paused").exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("source must acknowledge the paused refresh");
        assert_eq!(service.cached_sessions().unwrap().threads.len(), 4);

        let stopped = {
            let request = async {
                if inference {
                    service.test_jev_inference().await.map(|_| ())
                } else {
                    service.check_jev_connection().await.map(|_| ())
                }
            };
            tokio::pin!(request);
            // Poll the public entry point before stopping it: spawning alone would
            // allow cancellation to win before the request had actually started.
            std::future::poll_fn(|cx| {
                assert!(request.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            timeout(Duration::from_secs(2), async {
                tokio::join!(request, async {
                    if delete {
                        let status = service.delete_jev_credential().await.unwrap();
                        assert!(!status.credential_configured);
                    } else {
                        service.cancel_jev().await;
                    }
                })
            })
            .await
        };
        let credential_remains = credentials.load().unwrap().is_some();
        if stopped.is_ok() && delete {
            let result = timeout(Duration::from_secs(2), async {
                if inference {
                    service.test_jev_inference().await.map(|_| ())
                } else {
                    service.check_jev_connection().await.map(|_| ())
                }
            })
            .await
            .expect("deleted credentials must reject new requests during refresh");
            assert!(matches!(
                result.unwrap_err().code,
                ErrorCode::JevNotConfigured
            ));
        }
        assert!(
            !refresh.is_finished(),
            "stop must finish before refresh resumes"
        );

        // Release and reap the source even when the regression assertion fails.
        fs::remove_file(pause).unwrap();
        let refreshed = timeout(Duration::from_secs(2), refresh)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.threads.len(), 4);
        assert!(refreshed.scopes.iter().all(|scope| scope.complete));
        service.shutdown().await;
        fs::remove_dir_all(dir).unwrap();

        let (result, ()) = stopped.expect("Jev stop must finish while refresh is paused");
        assert!(matches!(result.unwrap_err().code, ErrorCode::JevCancelled));
        assert_eq!(credential_remains, !delete);
    }

    #[tokio::test]
    async fn cancel_connection_during_paused_refresh() {
        stop_jev_during_refresh(false, false).await;
    }

    #[tokio::test]
    async fn cancel_inference_during_paused_refresh() {
        stop_jev_during_refresh(true, false).await;
    }

    #[tokio::test]
    async fn delete_credential_with_connection_during_paused_refresh() {
        stop_jev_during_refresh(false, true).await;
    }

    #[tokio::test]
    async fn delete_credential_with_inference_during_paused_refresh() {
        stop_jev_during_refresh(true, true).await;
    }

    #[tokio::test]
    async fn locked_keychain_keeps_saved_nonsecret_config_visible() {
        let dir = temp_data_dir();
        let credentials = Arc::new(MemoryCredentials::default());
        let service = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        service
            .save_jev(
                "https://old.example".into(),
                "jev-1.13.0".into(),
                Some("synthetic-only-key".into()),
            )
            .await
            .unwrap();
        drop(service);

        let locked =
            SourceService::with_credentials(dir.clone(), Arc::new(UnavailableCredentials)).unwrap();
        let status = locked.jev_status().await.unwrap();
        assert_eq!(status.config.base_url, "https://old.example");
        assert_eq!(status.config.model, "jev-1.13.0");
        assert!(!status.credential_configured);
        assert!(matches!(
            status.credential_error.unwrap().code,
            ErrorCode::JevCredentialFailed
        ));
        drop(locked);

        let unlocked = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        assert!(unlocked
            .save_jev("https://new.example".into(), "jev-latest".into(), None)
            .await
            .is_err());
        assert_eq!(
            unlocked.jev_status().await.unwrap().config.base_url,
            "https://old.example"
        );
        assert_eq!(
            credentials.load().unwrap().unwrap().base_url,
            "https://old.example"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn connection_retries_and_preferences_survive_restart() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codexflow-core-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("fake-ok.py");
        let mut file = fs::File::create(&binary).unwrap();
        file.write_all(include_bytes!("../../codex/tests/fixtures/fake_codex.py"))
            .unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .unwrap();

        let service = SourceService::new(root.join("data")).unwrap();
        let failed = service
            .connect(Some("/no/such/codexflow-test-binary".into()))
            .await
            .unwrap();
        assert!(matches!(failed.connection, ConnectionState::Failed));
        assert!(matches!(
            failed.error.unwrap().code,
            ErrorCode::BinaryUnavailable
        ));

        let connected = service
            .connect(Some(binary.to_string_lossy().into_owned()))
            .await
            .unwrap();
        assert!(matches!(connected.connection, ConnectionState::Connected));
        assert!(matches!(
            connected.capabilities.metadata.state,
            codexflow_domain::CapabilityState::Available
        ));
        service.set_theme(DisplayTheme::Dark).await.unwrap();
        service.shutdown().await;

        let reopened = SourceService::new(root.join("data")).unwrap();
        let (theme, source) = reopened.settings().await;
        assert!(matches!(theme, DisplayTheme::Dark));
        assert_eq!(source.selected_binary.as_deref(), binary.to_str());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn session_cache_and_jev_settings_survive_each_others_lifecycle() {
        let dir = temp_data_dir();
        fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("fake-list-rich.py");
        fs::write(
            &binary,
            include_bytes!("../../codex/tests/fixtures/fake_codex.py"),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let credentials = Arc::new(MemoryCredentials::default());
        let service = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        service
            .save_jev(
                "https://api.typesafe.ai".into(),
                "jev-1.13.0".into(),
                Some("synthetic-only-key".into()),
            )
            .await
            .unwrap();
        service
            .connect(Some(binary.to_string_lossy().into_owned()))
            .await
            .unwrap();
        service.set_theme(DisplayTheme::Dark).await.unwrap();
        let collected = service.refresh_sessions().await.unwrap();
        assert_eq!(collected.threads.len(), 4);
        assert!(collected.scopes.iter().all(|scope| scope.complete));
        service.shutdown().await;
        drop(service);

        let locked =
            SourceService::with_credentials(dir.clone(), Arc::new(UnavailableCredentials)).unwrap();
        assert!(locked
            .jev_status()
            .await
            .unwrap()
            .credential_error
            .is_some());
        assert_eq!(locked.cached_sessions().unwrap().threads.len(), 4);
        drop(locked);

        let reopened = SourceService::with_credentials(dir.clone(), credentials).unwrap();
        let (theme, source) = reopened.settings().await;
        assert!(matches!(theme, DisplayTheme::Dark));
        assert_eq!(source.selected_binary.as_deref(), binary.to_str());
        let jev = reopened.jev_status().await.unwrap();
        assert_eq!(jev.config.model, "jev-1.13.0");
        assert!(jev.credential_configured);
        reopened.delete_jev_credential().await.unwrap();
        assert!(!reopened.jev_status().await.unwrap().credential_configured);
        let cached = reopened.cached_sessions().unwrap();
        assert_eq!(cached.threads.len(), 4);
        assert!(cached.scopes.iter().all(|scope| scope.complete));
        assert!(reopened.refresh_sessions().await.is_err());
        drop(reopened);

        let offline =
            SourceService::with_credentials(dir.clone(), Arc::new(UnavailableCredentials)).unwrap();
        let cached = offline.cached_sessions().unwrap();
        assert_eq!(cached.threads.len(), 4);
        assert!(cached.scopes.iter().all(|scope| !scope.complete));
        drop(offline);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn complete_and_partial_lists_keep_one_cached_thread_per_id() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codexflow-list-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        for mode in ["list-rich", "list-partial", "list-moved"] {
            let binary = root.join(format!("fake-{mode}.py"));
            let mut file = fs::File::create(&binary).unwrap();
            file.write_all(include_bytes!("../../codex/tests/fixtures/fake_codex.py"))
                .unwrap();
            file.set_permissions(fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let service = SourceService::new(root.join("data")).unwrap();
        service
            .connect(Some(
                root.join("fake-list-rich.py")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await
            .unwrap();
        let first = service.refresh_sessions().await.unwrap();
        assert_eq!(first.threads.len(), 4);
        assert!(first.scopes.iter().all(|scope| scope.complete));
        let archived = first
            .threads
            .iter()
            .find(|thread| thread.id == "thread-a")
            .unwrap();
        assert!(archived.archived);
        assert!(first
            .threads
            .iter()
            .find(|thread| thread.id == "thread-b")
            .unwrap()
            .read_error
            .is_some());
        assert_eq!(
            first
                .threads
                .iter()
                .find(|thread| thread.id == "thread-b")
                .unwrap()
                .parent_thread_id
                .as_deref(),
            Some("thread-a")
        );
        assert_eq!(service.refresh_sessions().await.unwrap().threads.len(), 4);
        service.shutdown().await;

        let reopened = SourceService::new(root.join("data")).unwrap();
        assert_eq!(reopened.cached_sessions().unwrap().threads.len(), 4);
        assert!(reopened.refresh_sessions().await.is_err());
        assert_eq!(reopened.cached_sessions().unwrap().threads.len(), 4);
        reopened
            .connect(Some(
                root.join("fake-list-partial.py")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await
            .unwrap();
        let partial = reopened.refresh_sessions().await.unwrap();
        assert_eq!(partial.threads.len(), 4);
        assert!(partial.threads.iter().any(|thread| thread.id == "thread-c"));
        assert!(
            !partial
                .scopes
                .iter()
                .find(|scope| !scope.archived)
                .unwrap()
                .complete
        );
        assert!(
            partial
                .scopes
                .iter()
                .find(|scope| scope.archived)
                .unwrap()
                .complete
        );
        reopened
            .connect(Some(
                root.join("fake-list-moved.py")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await
            .unwrap();
        let moved = reopened.refresh_sessions().await.unwrap();
        assert_eq!(moved.threads.len(), 4);
        assert!(
            moved
                .threads
                .iter()
                .find(|thread| thread.id == "thread-b")
                .unwrap()
                .archived
        );
        let database = rusqlite::Connection::open(root.join("data/sessions.sqlite3")).unwrap();
        database
            .execute_batch(
                "CREATE TRIGGER reject_thread_update BEFORE UPDATE ON threads
             BEGIN SELECT RAISE(FAIL, 'simulated write failure'); END;",
            )
            .unwrap();
        drop(database);
        assert!(reopened.refresh_sessions().await.is_err());
        let failed = reopened.cached_sessions().unwrap();
        assert_eq!(failed.threads.len(), 4);
        assert!(failed.scopes.iter().all(|scope| !scope.complete
            && scope
                .error
                .as_deref()
                .is_some_and(|error| error.contains("保存"))));
        assert!(failed
            .scopes
            .iter()
            .all(|scope| scope.completed_at_unix_ms.is_some()));
        reopened.shutdown().await;
        let after_failure = SourceService::new(root.join("data"))
            .unwrap()
            .cached_sessions()
            .unwrap();
        assert_eq!(after_failure.threads.len(), 4);
        assert!(after_failure.scopes.iter().all(|scope| !scope.complete));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_choice_survives_a_refresh_reconciliation_that_started_first() {
        use std::{sync::mpsc, thread, time::Duration};

        let root = temp_data_dir();
        let project = root.join("notes");
        fs::create_dir_all(&project).unwrap();
        let service = Arc::new(
            SourceService::with_credentials(
                root.join("data"),
                Arc::new(MemoryCredentials::default()),
            )
            .unwrap(),
        );
        service
            .sessions
            .save_collection(
                &[codexflow_domain::ThreadMetadata {
                    id: "new-project-session".into(),
                    session_id: "new-project-session".into(),
                    title: None,
                    preview: String::new(),
                    cwd: project.to_string_lossy().into_owned(),
                    project_id: None,
                    source_kind: "cli".into(),
                    source_detail: None,
                    thread_source: None,
                    parent_thread_id: None,
                    forked_from_id: None,
                    git: None,
                    created_at: 0,
                    updated_at: 1,
                    archived: false,
                    metadata_complete: true,
                    content_complete: false,
                    read_error: None,
                    observed_at_unix_ms: 1,
                }],
                &[],
            )
            .unwrap();

        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let refreshing = Arc::clone(&service);
        let refresh = thread::spawn(move || {
            refreshing.reconcile_projects_with(|| {
                snapshot_tx.send(()).unwrap();
                resume_rx.recv().unwrap();
            })
        });
        snapshot_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("刷新应先取得旧项目快照");

        let (choice_started_tx, choice_started_rx) = mpsc::channel();
        let (choice_done_tx, choice_done_rx) = mpsc::channel();
        let choosing = Arc::clone(&service);
        let project_path = project.to_string_lossy().into_owned();
        let choice = thread::spawn(move || {
            choice_started_tx.send(()).unwrap();
            let result = choosing.choose_project(&project_path);
            choice_done_tx.send(result.is_ok()).unwrap();
            result
        });
        choice_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("项目选择线程应已启动");
        assert!(matches!(
            choice_done_rx.recv_timeout(Duration::from_millis(250)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        resume_tx.send(()).unwrap();
        refresh.join().unwrap().unwrap();
        choice.join().unwrap().unwrap();

        let catalog = service.project_catalog().unwrap();
        let selected_id = catalog.selected_project_id.unwrap();
        assert!(catalog.projects.iter().any(|item| item.id == selected_id));
        let sessions = service.project_sessions(&selected_id).unwrap();
        assert_eq!(sessions.threads.len(), 1);
        assert_eq!(sessions.threads[0].thread.id, "new-project-session");
        assert!(catalog.unassigned.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn selected_project_and_jev_credentials_survive_each_others_lifecycle() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("codexflow-project-service-{nonce}"));
        let project = root.join("notes");
        let other = root.join("notes-other");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&other).unwrap();
        let credentials = Arc::new(MemoryCredentials::default());
        let service =
            SourceService::with_credentials(root.join("data"), credentials.clone()).unwrap();
        service
            .save_jev(
                "https://api.typesafe.ai".into(),
                "jev-1.13.0".into(),
                Some("synthetic-only-key".into()),
            )
            .await
            .unwrap();
        let make_thread = |id: &str, cwd: &std::path::Path| codexflow_domain::ThreadMetadata {
            id: id.into(),
            session_id: id.into(),
            title: None,
            preview: String::new(),
            cwd: cwd.to_string_lossy().into_owned(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 0,
            updated_at: 1,
            archived: false,
            metadata_complete: true,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: 1,
        };
        service
            .sessions
            .save_collection(
                &[
                    make_thread("inside", &project),
                    make_thread("outside", &other),
                ],
                &[],
            )
            .unwrap();
        let catalog = service.choose_project(project.to_str().unwrap()).unwrap();
        let id = catalog.selected_project_id.unwrap();
        assert_eq!(catalog.unassigned.len(), 1);
        let sessions = service.project_sessions(&id).unwrap();
        assert_eq!(sessions.threads.len(), 1);
        assert_eq!(sessions.threads[0].thread.id, "inside");
        assert_eq!(
            sessions.workspaces,
            vec![fs::canonicalize(&project)
                .unwrap()
                .to_string_lossy()
                .into_owned()]
        );
        drop(service);
        let locked =
            SourceService::with_credentials(root.join("data"), Arc::new(UnavailableCredentials))
                .unwrap();
        assert!(locked
            .jev_status()
            .await
            .unwrap()
            .credential_error
            .is_some());
        assert_eq!(
            locked.project_sessions(&id).unwrap().threads[0].thread.id,
            "inside"
        );
        drop(locked);
        let reopened =
            SourceService::with_credentials(root.join("data"), credentials.clone()).unwrap();
        assert_eq!(
            reopened.jev_status().await.unwrap().config.model,
            "jev-1.13.0"
        );
        assert!(reopened.jev_status().await.unwrap().credential_configured);
        reopened
            .save_jev(
                "https://api.typesafe.ai".into(),
                "jev-1.13.0".into(),
                Some("replacement-synthetic-key".into()),
            )
            .await
            .unwrap();
        assert_eq!(
            credentials.load().unwrap().unwrap().key,
            "replacement-synthetic-key"
        );
        reopened.delete_jev_credential().await.unwrap();
        assert!(!reopened.jev_status().await.unwrap().credential_configured);
        assert_eq!(
            reopened.project_catalog().unwrap().selected_project_id,
            Some(id.clone())
        );
        let sessions = reopened.project_sessions(&id).unwrap();
        assert_eq!(sessions.threads.len(), 1);
        assert_eq!(sessions.threads[0].thread.id, "inside");
        assert_eq!(reopened.project_catalog().unwrap().unassigned.len(), 1);
        let _ = fs::remove_dir_all(root);
    }
}
