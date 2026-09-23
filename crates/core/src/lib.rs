use codexflow_codex::{diagnose, Session};
use codexflow_domain::{
    AppError, ConnectionState, DisplayTheme, ErrorCode, JevConfig, JevConnectionResult,
    JevInferenceResult, JevStatus, Preferences, SourceStatus,
};
use codexflow_jev::{
    normalize_base_url, system_credentials, Credential, CredentialStore, JevClient,
};
use codexflow_store::PreferenceStore;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, RwLock, RwLockWriteGuard};
use tokio_util::sync::CancellationToken;

pub struct SourceService {
    store: PreferenceStore,
    state: Mutex<State>,
    credentials: Arc<dyn CredentialStore>,
    jev_gate: RwLock<()>,
    jev_cancel: Mutex<CancellationToken>,
}

struct State {
    preferences: Preferences,
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
        let store = PreferenceStore::new(app_data_dir);
        let preferences = store.load()?;
        let status = SourceStatus::new(preferences.selected_binary.clone());
        Ok(Self {
            store,
            state: Mutex::new(State {
                preferences,
                status,
                session: None,
            }),
            credentials,
            jev_gate: RwLock::new(()),
            jev_cancel: Mutex::new(CancellationToken::new()),
        })
    }

    pub async fn jev_status(&self) -> Result<JevStatus, AppError> {
        let config = self.state.lock().await.preferences.jev.clone();
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
        let mut state = self.state.lock().await;
        if state.preferences.jev.base_url != base_url && api_key.is_none() {
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
        let mut next = state.preferences.clone();
        next.jev = JevConfig {
            base_url: base_url.clone(),
            model,
        };
        self.store.save(&next)?;
        state.preferences = next;
        drop(state);
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
        let (credential, model) = self.jev_request_settings().await?;
        let token = self.jev_cancel.lock().await.clone();
        let client = JevClient::new()?;
        tokio::select! {
            biased;
            _ = token.cancelled() => Err(jev_cancelled()),
            result = client.check_connection(&credential, &model) => result,
        }
    }

    pub async fn test_jev_inference(&self) -> Result<JevInferenceResult, AppError> {
        let _gate = self.jev_gate.read().await;
        let (credential, model) = self.jev_request_settings().await?;
        let token = self.jev_cancel.lock().await.clone();
        let client = JevClient::new()?;
        tokio::select! {
            biased;
            _ = token.cancelled() => Err(jev_cancelled()),
            result = client.test_inference(&credential, &model) => result,
        }
    }

    async fn jev_request_settings(&self) -> Result<(Credential, String), AppError> {
        let config = self.state.lock().await.preferences.jev.clone();
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
        (state.preferences.theme.clone(), state.status.clone())
    }

    pub async fn status(&self) -> SourceStatus {
        let mut state = self.state.lock().await;
        check_process(&mut state);
        state.status.clone()
    }

    pub async fn set_theme(&self, theme: DisplayTheme) -> Result<DisplayTheme, AppError> {
        let mut state = self.state.lock().await;
        let next = Preferences {
            selected_binary: state.preferences.selected_binary.clone(),
            theme,
            jev: state.preferences.jev.clone(),
        };
        self.store.save(&next)?;
        state.preferences = next;
        Ok(state.preferences.theme.clone())
    }

    pub async fn connect(&self, selected_binary: Option<String>) -> Result<SourceStatus, AppError> {
        let mut state = self.state.lock().await;
        let choice = selected_binary
            .map(|path| path.trim().to_owned())
            .filter(|path| !path.is_empty());
        let next = Preferences {
            selected_binary: choice.clone(),
            theme: state.preferences.theme.clone(),
            jev: state.preferences.jev.clone(),
        };
        self.store.save(&next)?;
        state.preferences = next;
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
        std::env::temp_dir().join(format!("codexflow-jev-test-{}-{nonce}", std::process::id()))
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
}
