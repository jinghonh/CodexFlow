use codexflow_codex::{diagnose, Session};
use codexflow_domain::{
    AppError, ConnectionState, DisplayTheme, ErrorCode, Preferences, SessionList, SourceStatus,
};
use codexflow_store::{PreferenceStore, SessionStore};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

pub struct SourceService {
    store: PreferenceStore,
    sessions: SessionStore,
    state: Mutex<State>,
}

struct State {
    preferences: Preferences,
    status: SourceStatus,
    session: Option<Session>,
}

impl SourceService {
    pub fn new(app_data_dir: PathBuf) -> Result<Self, AppError> {
        let store = PreferenceStore::new(app_data_dir.clone());
        let sessions = SessionStore::new(app_data_dir)?;
        let preferences = store.load()?;
        let status = SourceStatus::new(preferences.selected_binary.clone());
        Ok(Self {
            store,
            sessions,
            state: Mutex::new(State {
                preferences,
                status,
                session: None,
            }),
        })
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
        let mut state = self.state.lock().await;
        if let Some(mut session) = state.session.take() {
            session.close().await;
        }
    }

    pub fn cached_sessions(&self) -> Result<SessionList, AppError> {
        self.sessions.list()
    }

    pub async fn refresh_sessions(&self) -> Result<SessionList, AppError> {
        let mut state = self.state.lock().await;
        check_process(&mut state);
        let session = state.session.as_mut().ok_or_else(|| {
            AppError::codex(
                ErrorCode::SourceReadFailed,
                "Codex 来源当前不可用。请先连接，已有会话缓存仍可浏览。",
                true,
            )
        })?;
        let collection = session.collect_threads(now_ms() as i64).await;
        self.sessions
            .save_collection(&collection.threads, &collection.scopes)?;
        self.sessions.list()
    }
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
        reopened.shutdown().await;
        let _ = fs::remove_dir_all(root);
    }
}
