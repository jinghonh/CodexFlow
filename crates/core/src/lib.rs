mod analysis_batch;
mod candidates;
mod explorer;
mod facts;
mod inferred;
mod projects;
mod relations;
mod summary;
mod text_settings;
mod workstreams;

use codexflow_codex::{diagnose, CollectionUpdate, Session};
use codexflow_domain::{
    build_project_timeline, AppError, CandidatePreview, ConnectionState, DisplayTheme, ErrorCode,
    EvidenceCheck, EvidenceField, EvidencePage, EvidenceState, FactPage, HistoryCoverage,
    HistoryItemLocation, HistoryItemPage, HistoryTurnPage, IndexRun, IndexRunState, JevConfig,
    JevConnectionResult, JevInferenceResult, JevStatus, Preferences, ProjectCatalog, ProjectGraph,
    ProjectSessions, ProjectTimeline, ProjectWorkstreams, RelationReview, ReviewedInferredRelation,
    SessionList, SourceEvidence, SourceStatus, UserRelationDecision,
};
use codexflow_jev::{
    normalize_base_url, system_credentials, system_text_credentials, Credential, CredentialStore,
    JevClient,
};
use codexflow_store::{EvidenceSourceSnapshot, PreferenceStore, SessionStore, WorkstreamChange};
pub use explorer::{ProjectThreadQuery, ProjectThreadQueryResult};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
pub use summary::SummaryAnalyzer;
pub const EVALUATION_CANDIDATE_RULES_VERSION: &str = candidates::CANDIDATE_RULE_VERSION;
pub const EVALUATION_FACT_RULES_VERSION: &str = facts::RULE_VERSION;
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
    text_credentials: Arc<dyn CredentialStore>,
    project_updates: std::sync::Mutex<()>,
    // Invalidates ownership computed from stale source and project snapshots.
    reconciliation_revision: AtomicU64,
    jev_gate: RwLock<()>,
    jev_cancel: Mutex<CancellationToken>,
    text_gate: RwLock<()>,
    text_cancel: Mutex<CancellationToken>,
    text_validation_cancel: Mutex<CancellationToken>,
    refresh_active: std::sync::Mutex<Option<(String, CancellationToken)>>,
    summary_active:
        std::sync::Mutex<std::collections::HashMap<String, (String, CancellationToken)>>,
    analysis_active:
        std::sync::Mutex<std::collections::HashMap<String, analysis_batch::AnalysisControl>>,
    analysis_update_lock: std::sync::Mutex<()>,
    analysis_clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    model_slots: Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    analysis_auth_home: Option<PathBuf>,
}

struct State {
    status: SourceStatus,
    session: Option<Session>,
}

struct EmptyCredentials;
impl CredentialStore for EmptyCredentials {
    fn load(&self) -> Result<Option<Credential>, AppError> {
        Ok(None)
    }
    fn save(&self, _: &Credential) -> Result<(), AppError> {
        Err(AppError::text(
            ErrorCode::TextCredentialFailed,
            "测试凭据存储不可写。",
            false,
        ))
    }
    fn delete(&self) -> Result<(), AppError> {
        Ok(())
    }
}

impl SourceService {
    pub fn new(app_data_dir: PathBuf) -> Result<Self, AppError> {
        Self::with_credential_stores(
            app_data_dir,
            system_credentials(),
            system_text_credentials(),
        )
    }

    pub fn with_credentials(
        app_data_dir: PathBuf,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self, AppError> {
        Self::with_credential_stores(app_data_dir, credentials, Arc::new(EmptyCredentials))
    }

    pub fn with_credential_stores(
        app_data_dir: PathBuf,
        credentials: Arc<dyn CredentialStore>,
        text_credentials: Arc<dyn CredentialStore>,
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
            text_credentials,
            project_updates: std::sync::Mutex::new(()),
            reconciliation_revision: AtomicU64::new(0),
            jev_gate: RwLock::new(()),
            jev_cancel: Mutex::new(CancellationToken::new()),
            text_gate: RwLock::new(()),
            text_cancel: Mutex::new(CancellationToken::new()),
            text_validation_cancel: Mutex::new(CancellationToken::new()),
            refresh_active: std::sync::Mutex::new(None),
            summary_active: std::sync::Mutex::new(std::collections::HashMap::new()),
            analysis_active: std::sync::Mutex::new(std::collections::HashMap::new()),
            analysis_update_lock: std::sync::Mutex::new(()),
            analysis_clock: Arc::new(|| now_ms() as i64),
            model_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            #[cfg(test)]
            analysis_auth_home: None,
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
        let replacing_key = api_key.is_some();
        let previous_credential = if replacing_key {
            self.credentials.load()?
        } else {
            None
        };
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
        next.jev_revision = next.jev_revision.saturating_add(1);
        if let Err(error) = self.store.save(&next) {
            if replacing_key {
                let restored = if let Some(previous) = previous_credential {
                    self.credentials.save(&previous)
                } else {
                    self.credentials.delete()
                };
                if restored.is_err() {
                    return Err(AppError::jev(
                        ErrorCode::JevCredentialFailed,
                        "设置保存失败，且无法恢复之前的钥匙串状态；请重新填写当前服务地址的 API Key。",
                        true,
                    ));
                }
            }
            return Err(error);
        }
        *preferences = next;
        drop(preferences);
        self.jev_status().await
    }

    pub async fn delete_jev_credential(&self) -> Result<JevStatus, AppError> {
        let _gate = self.stop_jev().await;
        self.credentials.delete()?;
        let mut preferences = self.preferences.lock().await;
        let mut next = preferences.clone();
        next.jev_revision = next.jev_revision.saturating_add(1);
        self.store.save(&next)?;
        *preferences = next;
        drop(preferences);
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
        let token = self.jev_cancel.lock().await.clone();
        let _slot = tokio::select! {
            biased;
            _ = token.cancelled() => return Err(jev_cancelled()),
            acquired = self.model_slots.acquire() => acquired.map_err(|_| AppError::jev(
                ErrorCode::AnalysisUnavailable, "模型调用队列不可用。", true))?,
        };
        let _gate = self.jev_gate.read().await;
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
            jev_revision: preferences.jev_revision,
            text: preferences.text.clone(),
            text_revision: preferences.text_revision,
        };
        self.store.save(&next)?;
        *preferences = next;
        Ok(preferences.theme.clone())
    }

    pub async fn connect(&self, selected_binary: Option<String>) -> Result<SourceStatus, AppError> {
        let runs: Vec<_> = self
            .analysis_active
            .lock()
            .unwrap()
            .values()
            .map(|control| control.id.clone())
            .collect();
        for id in runs {
            let _ = self.cancel_analysis_run(&id).await;
        }
        for (_, token) in self.summary_active.lock().unwrap().values() {
            token.cancel();
        }
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
                jev_revision: preferences.jev_revision,
                text: preferences.text.clone(),
                text_revision: preferences.text_revision,
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
        let runs: Vec<_> = self
            .analysis_active
            .lock()
            .unwrap()
            .values()
            .map(|control| control.id.clone())
            .collect();
        for id in runs {
            let _ = self.cancel_analysis_run(&id).await;
        }
        self.cancel_jev().await;
        for (_, token) in self.summary_active.lock().unwrap().values() {
            token.cancel();
        }
        if let Some((_, token)) = self.refresh_active.lock().unwrap().as_ref() {
            token.cancel();
        }
        let mut state = self.state.lock().await;
        if let Some(mut session) = state.session.take() {
            session.close().await;
        }
    }

    pub fn cached_sessions(&self) -> Result<SessionList, AppError> {
        self.sessions.list()
    }

    pub async fn load_thread_history(&self, thread_id: &str) -> Result<HistoryCoverage, AppError> {
        let thread = self.sessions.thread(thread_id)?.ok_or_else(|| {
            AppError::codex(
                ErrorCode::SourceReadFailed,
                "会话未在本地索引中。请先刷新列表。",
                false,
            )
        })?;
        let mut state = self.state.lock().await;
        check_process(&mut state);
        let session = state.session.as_mut().ok_or_else(|| {
            AppError::codex(
                ErrorCode::SourceReadFailed,
                "来源当前不可用；可继续查看已缓存的回合和条目。",
                true,
            )
        })?;
        let snapshot = session
            .collect_history(thread_id, thread.updated_at, now_ms() as i64)
            .await;
        drop(state);
        let coverage = self.sessions.save_history(&snapshot)?;
        self.ensure_facts(thread_id)?;
        Ok(coverage)
    }

    pub fn history_turns(
        &self,
        thread_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<HistoryTurnPage, AppError> {
        self.sessions.history_turns(thread_id, offset, limit)
    }

    pub fn history_items(
        &self,
        thread_id: &str,
        turn_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<HistoryItemPage, AppError> {
        self.sessions
            .history_items(thread_id, turn_id, offset, limit)
    }

    pub fn locate_history_item(
        &self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
    ) -> Result<Option<HistoryItemLocation>, AppError> {
        self.sessions
            .locate_history_item(thread_id, turn_id, item_id)
    }

    fn ensure_facts(&self, thread_id: &str) -> Result<(), AppError> {
        let changed = || {
            AppError::codex(
                ErrorCode::SourceReadFailed,
                "事实提取期间来源版本或完整性已变化；旧事实保留，请重新读取。",
                true,
            )
        };
        let thread = self.sessions.thread(thread_id)?.ok_or_else(|| {
            AppError::codex(ErrorCode::SourceReadFailed, "会话未在本地索引中。", false)
        })?;
        if !self
            .sessions
            .history_coverage(thread_id)?
            .is_some_and(|coverage| {
                coverage.source_updated_at == thread.updated_at
                    && coverage.turns_complete
                    && coverage.items_complete
            })
        {
            // An incomplete refresh may contain old rows. Keep the prior facts until
            // the whole current source is available rather than extracting from a mix.
            return Ok(());
        }
        let generation = self.sessions.history_generation(thread_id)?;
        let index = self.sessions.fact_index(thread_id)?;
        if index.as_ref().is_some_and(|(_, rule, saved_generation)| {
            rule == facts::RULE_VERSION && *saved_generation == generation
        }) {
            return Ok(());
        }
        let items = self.sessions.all_history_items(thread_id)?;
        let digest = facts::content_digest(&items);
        if index
            .as_ref()
            .is_some_and(|(saved, rule, _)| saved == &digest && rule == facts::RULE_VERSION)
        {
            return self
                .sessions
                .mark_fact_index_current(
                    thread_id,
                    thread.updated_at,
                    generation,
                    &digest,
                    facts::RULE_VERSION,
                )?
                .then_some(())
                .ok_or_else(changed);
        }
        let (facts, evidence) = facts::extract(&items);
        self.sessions
            .replace_automatic_facts(
                thread_id,
                thread.updated_at,
                &digest,
                facts::RULE_VERSION,
                generation,
                &facts,
                &evidence,
            )?
            .then_some(())
            .ok_or_else(changed)
    }

    pub fn source_facts(
        &self,
        thread_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<FactPage, AppError> {
        self.ensure_facts(thread_id)?;
        self.sessions.facts(thread_id, offset, limit)
    }

    pub fn source_evidence(
        &self,
        thread_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<EvidencePage, AppError> {
        self.ensure_facts(thread_id)?;
        self.sessions.evidence_page(thread_id, offset, limit)
    }

    pub fn check_evidence(&self, evidence: &SourceEvidence) -> Result<EvidenceCheck, AppError> {
        let snapshot = self.sessions.evidence_snapshot(evidence.clone())?;
        Ok(Self::check_evidence_snapshot(snapshot))
    }

    fn check_evidence_snapshot(snapshot: EvidenceSourceSnapshot) -> EvidenceCheck {
        let EvidenceSourceSnapshot {
            evidence,
            thread,
            turn_exists,
            item,
            item_in_other_turn,
            fact,
            location,
        } = snapshot;
        let result = |state, message: &str| EvidenceCheck {
            state,
            message: message.into(),
            location: None,
        };
        let Some(thread) = thread else {
            return result(EvidenceState::MissingThread, "证据所指会话不存在。");
        };
        if !turn_exists {
            return result(EvidenceState::MissingTurn, "证据所指回合不存在。");
        }
        let Some(item) = item else {
            if item_in_other_turn {
                return result(
                    EvidenceState::WrongHierarchy,
                    "条目标识存在，但不属于证据指定的回合。",
                );
            }
            return result(
                EvidenceState::MissingItem,
                "证据所指条目不存在；历史可能只读取了部分内容。",
            );
        };
        let Some(fact) = fact else {
            return result(EvidenceState::MissingFact, "证据所属事实不存在或已被替换。");
        };
        if fact.thread_id != evidence.thread_id
            || fact.turn_id != evidence.turn_id
            || fact.item_id != evidence.item_id
            || fact.evidence_id != evidence.id
        {
            return result(
                EvidenceState::WrongHierarchy,
                "证据定位与所属事实的会话、回合或条目不匹配。",
            );
        }
        if item.content_version != evidence.content_version
            || fact.content_version != evidence.content_version
            || item.source_updated_at != thread.updated_at
        {
            return result(
                EvidenceState::StaleVersion,
                "证据内容版本已失效；请重新读取来源历史。",
            );
        }
        let source = match evidence.field {
            EvidenceField::Text => item.text.as_deref(),
            EvidenceField::Command => item.command.as_deref(),
            EvidenceField::Output => item.output.as_deref(),
            EvidenceField::ChangePath => evidence
                .change_index
                .and_then(|index| item.changes.get(index as usize))
                .map(|change| change.path.as_str()),
            EvidenceField::ChangeDiff => evidence
                .change_index
                .and_then(|index| item.changes.get(index as usize))
                .map(|change| change.diff.as_str()),
        };
        if evidence.excerpt.is_empty()
            || !source.is_some_and(|text| text.contains(&evidence.excerpt))
        {
            return result(
                EvidenceState::ExcerptMissing,
                "证据摘录无法在指定的来源字段中定位。",
            );
        }
        if location.is_none() {
            return result(EvidenceState::MissingItem, "证据所指条目无法定位。");
        }
        EvidenceCheck {
            state: EvidenceState::Valid,
            message: "证据有效。".into(),
            location,
        }
    }

    pub fn validate_source_evidence(&self, evidence_id: &str) -> Result<EvidenceCheck, AppError> {
        let snapshot = self
            .sessions
            .stored_evidence_snapshot(evidence_id)?
            .ok_or_else(|| {
                AppError::codex(
                    ErrorCode::SourceReadFailed,
                    "证据不存在或已被新的自动事实替换。",
                    false,
                )
            })?;
        Ok(Self::check_evidence_snapshot(snapshot))
    }

    fn reconcile_projects(&self) -> Result<(), AppError> {
        self.reconcile_projects_with(|| {})
    }

    // The hook pauses after the snapshot and before path/Git attribution work,
    // so tests can model a slow reconciliation without holding project_updates.
    fn reconcile_projects_with(&self, during_recompute: impl FnOnce()) -> Result<(), AppError> {
        let mut during_recompute = Some(during_recompute);
        loop {
            let revision = self.reconciliation_revision.load(Ordering::Relaxed);
            let threads = self.sessions.list()?.threads;
            let projects = self.sessions.projects()?;
            let previous = self.sessions.attributions()?;

            // Recheck after reading the independent SQLite views. If source
            // sessions or projects changed during the snapshot, take a fresh one.
            if self.reconciliation_revision.load(Ordering::Relaxed) != revision {
                continue;
            }

            if let Some(hook) = during_recompute.take() {
                hook();
            }
            let (projects, attributions) = projects::reconcile(&threads, projects, &previous);

            let _updates = self.lock_project_updates();
            if self.reconciliation_revision.load(Ordering::Relaxed) != revision {
                continue;
            }
            let relations = relations::observe(&threads, &attributions);
            self.sessions.save_projects_and_attributions(
                &projects,
                &attributions,
                Some(&relations),
            )?;
            self.reconciliation_revision.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
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

    pub fn project_timeline(&self, project_id: &str) -> Result<ProjectTimeline, AppError> {
        let sessions = self.sessions.project_sessions(project_id)?;
        let turns = self.sessions.project_turns(project_id)?;
        Ok(build_project_timeline(sessions, turns))
    }

    pub fn project_graph(&self, project_id: &str) -> Result<ProjectGraph, AppError> {
        let sessions = self.sessions.project_sessions(project_id)?;
        let relations = self.sessions.observed_relations(project_id)?;
        let (preview, derived) = self.automatic_candidates(&sessions)?;
        let jev_config = self.store.load()?.jev;
        let mut graph = relations::project_graph(sessions, relations);
        graph.derived_relations = derived;
        let reviews: std::collections::BTreeMap<_, _> = self
            .sessions
            .relation_reviews()?
            .into_iter()
            .map(|(review, snapshot)| (review.relation_id.clone(), (review, snapshot)))
            .collect();
        let mut reviewed = std::collections::BTreeMap::new();
        for result in self.sessions.inferred_pair_outcomes(project_id)? {
            let candidate = preview
                .candidates
                .iter()
                .find(|candidate| candidate.id == result.candidate_id);
            let source_current = if let Some(candidate) = candidate {
                preview.candidate_versions.get(&candidate.id) == Some(&result.input_version)
            } else {
                false
            };
            let config_current = result.jev_identity.as_ref().is_some_and(|identity| {
                identity.base_url == jev_config.base_url
                    && identity.requested_model == jev_config.model
                    && identity.rules_version == codexflow_jev::RELATION_RULES_VERSION
                    && identity.actual_model.starts_with("jev-")
            });
            let stale_reason = if !source_current {
                Some("候选或来源内容版本已变化，请重新分析关系。".to_owned())
            } else if !config_current {
                Some("Jev 服务、模型或分析规则已变化，请重新分析关系。".to_owned())
            } else {
                None
            };
            for relation in &result.relations {
                if relation.project_id != project_id
                    || relation.source != "jev"
                    || relation.from_thread_id == relation.to_thread_id
                {
                    continue;
                }
                let mut relation = relation.clone();
                relation.id = inferred::relation_id(
                    &relation.from_thread_id,
                    &relation.to_thread_id,
                    relation.kind,
                );
                let evidence_version = inferred::evidence_version(&relation.evidence)?;
                let review = reviews
                    .get(&relation.id)
                    .map(|(review, _)| review.clone())
                    .unwrap_or(RelationReview {
                        relation_id: relation.id.clone(),
                        project_id: project_id.into(),
                        decision: UserRelationDecision::Pending,
                        revision: 0,
                        confirmed_evidence_version: None,
                    });
                let valid = if let Some(candidate) = candidate {
                    source_current
                        && config_current
                        && result.jev_identity.as_ref().is_some_and(|identity| {
                            relation.requested_model == identity.requested_model
                                && relation.actual_model == identity.actual_model
                        })
                        && relation.input_version == result.input_version
                        && relation.from_thread_id != relation.to_thread_id
                        && [
                            relation.from_thread_id.as_str(),
                            relation.to_thread_id.as_str(),
                        ]
                        .iter()
                        .all(|id| {
                            *id == candidate.left_thread_id || *id == candidate.right_thread_id
                        })
                        && candidate.evidence.pairs.contains(&relation.evidence)
                        && (0.0..=1.0).contains(&relation.confidence)
                        && inferred::valid_evidence(self, project_id, &relation.evidence.left)?
                        && inferred::valid_evidence(self, project_id, &relation.evidence.right)?
                } else {
                    false
                };
                let entry = ReviewedInferredRelation {
                    relation,
                    evidence_version,
                    evidence_valid: valid,
                    stale_reason: if valid {
                        None
                    } else {
                        stale_reason
                            .clone()
                            .or_else(|| Some("来源证据已变化或不可定位。".into()))
                    },
                    review,
                };
                if !reviewed
                    .get(&entry.relation.id)
                    .is_some_and(|old: &ReviewedInferredRelation| old.evidence_valid)
                {
                    reviewed.insert(entry.relation.id.clone(), entry);
                }
            }
            if source_current && config_current {
                graph.inference_outcomes.push(result);
            }
        }
        for (relation_id, (review, snapshot)) in reviews {
            if reviewed.contains_key(&relation_id) {
                continue;
            }
            let Some(mut relation) = snapshot else {
                continue;
            };
            if relation.source != "jev"
                || relation.from_thread_id == relation.to_thread_id
                || inferred::relation_id(
                    &relation.from_thread_id,
                    &relation.to_thread_id,
                    relation.kind,
                ) != relation_id
                || self
                    .sessions
                    .thread_project_id(&relation.from_thread_id)?
                    .as_deref()
                    != Some(project_id)
                || self
                    .sessions
                    .thread_project_id(&relation.to_thread_id)?
                    .as_deref()
                    != Some(project_id)
            {
                continue;
            }
            relation.id = relation_id.clone();
            relation.project_id = project_id.into();
            let evidence_version = inferred::evidence_version(&relation.evidence)?;
            reviewed.insert(
                relation_id,
                ReviewedInferredRelation {
                    relation,
                    evidence_version,
                    evidence_valid: false,
                    stale_reason: Some(
                        "旧关系已不在当前候选结果中，请检查旧证据并重新分析。".into(),
                    ),
                    review,
                },
            );
        }
        graph.reviewed_relations = reviewed.into_values().collect();
        for entry in &graph.reviewed_relations {
            if entry.evidence_valid && entry.review.decision != UserRelationDecision::Rejected {
                graph.inferred_relations.push(entry.relation.clone());
            }
        }
        Ok(graph)
    }

    pub fn project_workstreams(&self, project_id: &str) -> Result<ProjectWorkstreams, AppError> {
        let graph = self.project_graph(project_id)?;
        let _guard = self.lock_project_updates();
        for _ in 0..8 {
            let automatic = self.automatic_workstreams_for_graph(&graph)?;
            let corrections = self.sessions.workstream_corrections(project_id)?;
            if automatic.revision == corrections.revision {
                return Ok(workstreams::apply_corrections(
                    &graph,
                    automatic,
                    &corrections,
                ));
            }
        }
        Err(AppError::workstream_conflict())
    }

    fn automatic_workstreams(&self, project_id: &str) -> Result<ProjectWorkstreams, AppError> {
        let graph = self.project_graph(project_id)?;
        let _guard = self.lock_project_updates();
        self.automatic_workstreams_for_graph(&graph)
    }

    fn automatic_workstreams_for_graph(
        &self,
        graph: &ProjectGraph,
    ) -> Result<ProjectWorkstreams, AppError> {
        self.automatic_workstreams_for_graph_with_hook(graph, || {})
    }

    fn automatic_workstreams_for_graph_with_hook<F: FnOnce()>(
        &self,
        graph: &ProjectGraph,
        before_first_replace: F,
    ) -> Result<ProjectWorkstreams, AppError> {
        let project_id = &graph.project.id;
        let mut before_first_replace = Some(before_first_replace);
        for _ in 0..8 {
            let (previous, corrections) = self.sessions.workstream_snapshot(project_id)?;
            let mut result = workstreams::build(graph, &previous);
            for old in &previous {
                if result.workstreams.iter().any(|stream| stream.id == old.id) {
                    continue;
                }
                if corrections.names.contains_key(&old.id)
                    || corrections
                        .members
                        .values()
                        .any(|target| target.as_deref() == Some(&old.id))
                {
                    let mut retained = old.clone();
                    retained.members.clear();
                    retained.relation_ids.clear();
                    result.workstreams.push(retained);
                }
            }
            result.workstreams.sort_by(|a, b| a.id.cmp(&b.id));
            if result.workstreams == previous {
                result.revision = corrections.revision;
                return Ok(result);
            }
            if let Some(hook) = before_first_replace.take() {
                hook();
            }
            if self.sessions.replace_workstreams(
                project_id,
                corrections.revision,
                &result.workstreams,
            )? {
                result.revision = corrections.revision.saturating_add(1);
                return Ok(result);
            }
        }
        Err(AppError::workstream_conflict())
    }

    pub fn rename_workstream(
        &self,
        project_id: &str,
        id: &str,
        name: &str,
        expected_revision: u64,
    ) -> Result<u64, AppError> {
        let name = name.trim();
        if !(2..=80).contains(&name.chars().count()) || name.chars().any(char::is_control) {
            return Err(AppError::project("工作流名称须为 2–80 个可显示字符。"));
        }
        let view = self.project_workstreams(project_id)?;
        if !view.workstreams.iter().any(|stream| stream.id == id) {
            return Err(AppError::workstream_conflict());
        }
        self.sessions.save_workstream_correction(
            project_id,
            expected_revision,
            WorkstreamChange::Rename { id, name },
        )
    }

    pub fn restore_workstream_name(
        &self,
        project_id: &str,
        id: &str,
        expected_revision: u64,
    ) -> Result<u64, AppError> {
        let view = self.project_workstreams(project_id)?;
        if !view.workstreams.iter().any(|stream| stream.id == id) {
            return Err(AppError::workstream_conflict());
        }
        self.sessions.save_workstream_correction(
            project_id,
            expected_revision,
            WorkstreamChange::RestoreName { id },
        )
    }

    pub fn move_thread_to_workstream(
        &self,
        project_id: &str,
        thread_id: &str,
        target_id: Option<&str>,
        expected_revision: u64,
    ) -> Result<u64, AppError> {
        let graph = self.project_graph(project_id)?;
        if !graph
            .nodes
            .iter()
            .any(|node| node.id == thread_id && !node.reference_only)
        {
            return Err(AppError::project("该会话不属于当前项目，请刷新项目。"));
        }
        let view = self.project_workstreams(project_id)?;
        if target_id.is_some_and(|id| !view.workstreams.iter().any(|stream| stream.id == id)) {
            return Err(AppError::workstream_conflict());
        }
        self.sessions.save_workstream_correction(
            project_id,
            expected_revision,
            WorkstreamChange::Move {
                thread_id,
                target_id,
            },
        )
    }

    pub fn restore_thread_workstream(
        &self,
        project_id: &str,
        thread_id: &str,
        expected_revision: u64,
    ) -> Result<u64, AppError> {
        let graph = self.project_graph(project_id)?;
        if !graph
            .nodes
            .iter()
            .any(|node| node.id == thread_id && !node.reference_only)
        {
            return Err(AppError::project("该会话不属于当前项目，请刷新项目。"));
        }
        self.project_workstreams(project_id)?;
        self.sessions.save_workstream_correction(
            project_id,
            expected_revision,
            WorkstreamChange::RestoreMember { thread_id },
        )
    }

    pub fn decide_inferred_relation(
        &self,
        project_id: &str,
        relation_id: &str,
        decision: UserRelationDecision,
        expected_revision: u64,
        expected_evidence_version: Option<&str>,
    ) -> Result<RelationReview, AppError> {
        let graph = self.project_graph(project_id)?;
        let relation = graph
            .reviewed_relations
            .iter()
            .find(|entry| entry.relation.id == relation_id)
            .ok_or_else(|| AppError {
                code: ErrorCode::AnalysisInvalidResult,
                message: "找不到该推断关系，请刷新项目关系图。".into(),
                retryable: true,
                cache_preserved: true,
                backend: "core".into(),
                retry_after_ms: None,
            })?;
        if decision == UserRelationDecision::Confirmed {
            if expected_evidence_version != Some(relation.evidence_version.as_str()) {
                return Err(AppError::conflict());
            }
            if !relation.evidence_valid {
                return Err(AppError {
                    code: ErrorCode::AnalysisInvalidResult,
                    message: "关系证据已过期，请刷新并重新检查证据。".into(),
                    retryable: true,
                    cache_preserved: true,
                    backend: "core".into(),
                    retry_after_ms: None,
                });
            }
        }
        self.sessions.save_relation_review(
            project_id,
            relation_id,
            decision,
            (decision == UserRelationDecision::Confirmed)
                .then_some(relation.evidence_version.as_str()),
            expected_revision,
            Some(&relation.relation),
        )
    }

    fn automatic_candidates(
        &self,
        sessions: &ProjectSessions,
    ) -> Result<(CandidatePreview, Vec<codexflow_domain::DerivedRelation>), AppError> {
        let mut readable = sessions.clone();
        let mut complete_ids = std::collections::HashSet::new();
        for item in &sessions.threads {
            if self
                .sessions
                .history_coverage(&item.thread.id)?
                .is_some_and(|coverage| {
                    coverage.source_updated_at == item.thread.updated_at
                        && coverage.turns_complete
                        && coverage.items_complete
                })
            {
                complete_ids.insert(item.thread.id.clone());
            }
        }
        readable
            .threads
            .retain(|item| complete_ids.contains(&item.thread.id));
        for item in &readable.threads {
            self.ensure_facts(&item.thread.id)?;
        }
        let expected: Vec<_> = readable
            .threads
            .iter()
            .map(|item| {
                Ok((
                    item.thread.id.clone(),
                    item.thread.updated_at,
                    self.sessions.history_generation(&item.thread.id)?,
                ))
            })
            .collect::<Result<_, AppError>>()?;
        let mut hasher = Sha256::new();
        hasher.update(candidates::CANDIDATE_RULE_VERSION);
        hasher.update(facts::RULE_VERSION);
        hasher.update(
            serde_json::to_vec(sessions).map_err(|_| AppError::store("序列化候选输入失败。"))?,
        );
        hasher.update(
            serde_json::to_vec(&expected).map_err(|_| AppError::store("序列化候选版本失败。"))?,
        );
        let input_version = format!("{:x}", hasher.finalize());
        if let Some(saved) = self.sessions.cached_automatic_candidate_view_if_current(
            &sessions.project.id,
            &input_version,
            &expected,
            facts::RULE_VERSION,
        )? {
            return Ok(saved);
        }
        let material = self
            .sessions
            .project_material(&sessions.project.id, &expected, facts::RULE_VERSION)?
            .ok_or_else(|| {
                AppError::codex(
                    ErrorCode::SourceReadFailed,
                    "候选读取期间来源完整性或版本已变化，请刷新后重试。",
                    true,
                )
            })?;
        let mut result = candidates::build(&readable, material);
        result.0.input_version = input_version;
        result.0.unavailable_threads = (sessions.threads.len() - readable.threads.len()) as u64;
        for candidate in &result.0.candidates {
            result.0.candidate_versions.insert(
                candidate.id.clone(),
                inferred::candidate_version_at(candidate, &expected)?,
            );
        }
        let analyzed: std::collections::HashSet<_> = self
            .sessions
            .inferred_pair_outcomes(&sessions.project.id)?
            .into_iter()
            .filter(|outcome| {
                result.0.candidate_versions.get(&outcome.candidate_id)
                    == Some(&outcome.input_version)
            })
            .map(|outcome| outcome.candidate_id)
            .collect();
        if let Some(previous) = self.sessions.automatic_candidates(&sessions.project.id)? {
            let mut stale = std::collections::BTreeMap::new();
            for entry in previous.stale_candidates {
                if !analyzed.contains(&entry.candidate.id) {
                    stale.insert(entry.candidate.id.clone(), entry);
                }
            }
            for candidate in previous.candidates {
                let old_version = previous
                    .candidate_versions
                    .get(&candidate.id)
                    .cloned()
                    .unwrap_or_default();
                if result.0.candidate_versions.get(&candidate.id) == Some(&old_version)
                    || analyzed.contains(&candidate.id)
                {
                    continue;
                }
                let incomplete = !complete_ids.contains(&candidate.left_thread_id)
                    || !complete_ids.contains(&candidate.right_thread_id);
                stale.insert(
                    candidate.id.clone(),
                    codexflow_domain::StaleCandidate {
                        candidate,
                        input_version: old_version,
                        reason: if incomplete {
                            "来源部分可读、读取失败或尚未读取新版内容；旧候选保留供检查。".into()
                        } else {
                            "候选材料、来源内容或提取规则已变化；旧候选待重新分析。".into()
                        },
                    },
                );
            }
            result.0.stale_candidates = stale.into_values().collect();
        }
        if !self.sessions.replace_automatic_candidates_if_current(
            &result.0,
            &result.1,
            &expected,
            facts::RULE_VERSION,
        )? {
            return Err(AppError::codex(
                ErrorCode::SourceReadFailed,
                "候选保存前来源完整性或版本已变化，旧候选保留，请刷新后重试。",
                true,
            ));
        }
        Ok(result)
    }

    pub fn candidate_preview(&self, project_id: &str) -> Result<CandidatePreview, AppError> {
        let sessions = self.project_sessions(project_id)?;
        Ok(self.automatic_candidates(&sessions)?.0)
    }

    pub fn choose_project(&self, path: &str) -> Result<ProjectCatalog, AppError> {
        let project = projects::selected_project(path).map_err(AppError::project)?;
        {
            let _updates = self.lock_project_updates();
            self.sessions
                .save_projects_and_attributions(&[project.clone()], &[], None)?;
            self.reconciliation_revision.fetch_add(1, Ordering::Relaxed);
            self.sessions.select_project(&project.id)?;
        }
        self.reconcile_projects()?;
        self.sessions.catalog()
    }

    pub fn choose_existing_project(&self, project_id: &str) -> Result<ProjectCatalog, AppError> {
        self.sessions.select_project(project_id)?;
        self.sessions.catalog()
    }

    pub async fn refresh_sessions(&self) -> Result<SessionList, AppError> {
        let (mut run, token) = self.reserve_refresh(None)?;
        let result = self.run_refresh(&mut run, &token, &|_| {}).await;
        self.clear_refresh(&run.id);
        result
    }

    pub fn latest_refresh(&self) -> Result<Option<IndexRun>, AppError> {
        self.sessions.latest_index_run()
    }

    pub fn refresh_status(&self, id: &str) -> Result<IndexRun, AppError> {
        self.sessions.index_run(id)?.ok_or_else(|| {
            AppError::codex(ErrorCode::RefreshNotFound, "找不到指定的索引运行。", false)
        })
    }

    pub fn cancel_refresh(&self, id: &str) -> Result<IndexRun, AppError> {
        let active = self.refresh_active.lock().unwrap();
        if let Some((active_id, token)) = active.as_ref() {
            if active_id == id {
                token.cancel();
                return self.refresh_status(id);
            }
        }
        drop(active);
        self.refresh_status(id)
    }

    pub fn start_refresh(
        self: &Arc<Self>,
        project_id: Option<String>,
        notify: impl Fn(IndexRun) + Send + Sync + 'static,
    ) -> Result<IndexRun, AppError> {
        let (run, token) = self.reserve_refresh(project_id)?;
        let queued = run.clone();
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let mut run = run;
            let _ = service.run_refresh(&mut run, &token, &notify).await;
            service.clear_refresh(&run.id);
        });
        Ok(queued)
    }

    fn reserve_refresh(
        &self,
        project_id: Option<String>,
    ) -> Result<(IndexRun, CancellationToken), AppError> {
        static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
        let mut active = self.refresh_active.lock().unwrap();
        if active.is_some() {
            return Err(AppError::codex(
                ErrorCode::RefreshAlreadyRunning,
                "已有索引运行正在执行，请等待或取消后再刷新。",
                true,
            ));
        }
        let started_at_unix_ms = (now_ms() as i64).max(
            self.sessions
                .latest_index_run()?
                .map_or(0, |previous| previous.started_at_unix_ms + 1),
        );
        let run = IndexRun {
            id: format!(
                "index-{started_at_unix_ms}-{}-{}",
                std::process::id(),
                NEXT_RUN.fetch_add(1, Ordering::Relaxed)
            ),
            project_id,
            state: IndexRunState::Queued,
            started_at_unix_ms,
            finished_at_unix_ms: None,
            pages_saved: 0,
            threads_seen: 0,
            error: None,
            interrupted: false,
        };
        self.sessions.save_index_run(&run)?;
        let token = CancellationToken::new();
        *active = Some((run.id.clone(), token.clone()));
        Ok((run, token))
    }

    fn clear_refresh(&self, id: &str) {
        let mut active = self.refresh_active.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|(active_id, _)| active_id == id)
        {
            *active = None;
        }
    }

    async fn run_refresh(
        &self,
        run: &mut IndexRun,
        token: &CancellationToken,
        notify: &(dyn Fn(IndexRun) + Send + Sync),
    ) -> Result<SessionList, AppError> {
        let attempted_at = run.started_at_unix_ms;
        run.state = IndexRunState::Running;
        if let Err(error) = self.sessions.save_index_run(run) {
            run.state = IndexRunState::Failed;
            run.error = Some(error.clone());
            run.finished_at_unix_ms = Some(now_ms() as i64);
            let _ = self.sessions.save_index_run(run);
            notify(run.clone());
            return Err(error);
        }
        notify(run.clone());
        let result = self.collect_refresh(run, token, notify, attempted_at).await;
        run.finished_at_unix_ms = Some(now_ms() as i64);
        match &result {
            Ok((_, cancelled, complete, any_success)) => {
                run.state = if *cancelled {
                    IndexRunState::Cancelled
                } else if *complete {
                    IndexRunState::Complete
                } else if *any_success {
                    IndexRunState::Partial
                } else {
                    IndexRunState::Failed
                };
                if !complete && !cancelled {
                    run.error = Some(AppError::codex(
                        ErrorCode::SourceReadFailed,
                        "来源列表未全部读取成功；已提交的缓存仍可浏览，请重试刷新。",
                        true,
                    ));
                }
            }
            Err(error) => {
                run.state = if token.is_cancelled() {
                    IndexRunState::Cancelled
                } else {
                    IndexRunState::Failed
                };
                run.error = Some(error.clone());
                let _ = self.sessions.fail_refresh(attempted_at, &error.message);
            }
        }
        self.sessions.save_index_run(run)?;
        notify(run.clone());
        result.map(|(list, _, _, _)| list)
    }

    async fn collect_refresh(
        &self,
        run: &mut IndexRun,
        token: &CancellationToken,
        notify: &(dyn Fn(IndexRun) + Send + Sync),
        attempted_at: i64,
    ) -> Result<(SessionList, bool, bool, bool), AppError> {
        self.sessions.begin_refresh(attempted_at)?;
        let mut state = self.state.lock().await;
        check_process(&mut state);
        let session = state.session.as_mut().ok_or_else(|| {
            AppError::codex(
                ErrorCode::SourceReadFailed,
                "Codex 来源当前不可用。请先连接，已有会话缓存仍可浏览。",
                true,
            )
        })?;
        let collection = session
            .collect_threads_with(attempted_at, token, |update| {
                match update {
                    CollectionUpdate::Page(threads) => {
                        {
                            let _updates = self.lock_project_updates();
                            self.sessions.save_collection(threads, &[])?;
                            self.reconciliation_revision.fetch_add(1, Ordering::Relaxed);
                        }
                        run.pages_saved += 1;
                        run.threads_seen += threads.len() as u64;
                        self.reconcile_projects()?;
                    }
                    CollectionUpdate::Scope(scope) => {
                        self.sessions
                            .save_collection(&[], std::slice::from_ref(scope))?;
                    }
                }
                self.sessions.save_index_run(run)?;
                notify(run.clone());
                Ok(())
            })
            .await?;
        check_process(&mut state);
        let complete =
            collection.scopes.len() == 2 && collection.scopes.iter().all(|scope| scope.complete);
        let any_success =
            run.pages_saved > 0 || collection.scopes.iter().any(|scope| scope.complete);
        Ok((
            self.sessions.list()?,
            collection.cancelled,
            complete,
            any_success,
        ))
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
        let persisted: String = rusqlite::Connection::open(dir.join("sessions.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT preferences_json FROM app_preferences WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!persisted.contains("synthetic-secret"));
        assert!(!fs::read(dir.join("sessions.sqlite3"))
            .unwrap()
            .windows(b"synthetic-secret".len())
            .any(|bytes| bytes == b"synthetic-secret"));
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

    #[tokio::test]
    async fn sqlite_write_conflict_does_not_report_settings_saved_or_replace_key() {
        let dir = temp_data_dir();
        let credentials = Arc::new(MemoryCredentials::default());
        let service = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        service
            .save_jev(
                "https://api.typesafe.ai".into(),
                "jev-1.13.0".into(),
                Some("old-synthetic-key".into()),
            )
            .await
            .unwrap();
        service.set_theme(DisplayTheme::Dark).await.unwrap();
        let lock = rusqlite::Connection::open(dir.join("sessions.sqlite3")).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = service
            .save_jev(
                "https://other.example".into(),
                "jev-new".into(),
                Some("new-synthetic-key".into()),
            )
            .await
            .unwrap_err();
        assert!(matches!(error.code, ErrorCode::StorageFailed));
        assert!(error.retryable && error.cache_preserved);
        let status = service.jev_status().await.unwrap();
        assert!(status.credential_configured);
        assert_eq!(status.config.base_url, "https://api.typesafe.ai");
        assert_eq!(
            credentials.load().unwrap().unwrap().key,
            "old-synthetic-key"
        );
        assert!(matches!(service.settings().await.0, DisplayTheme::Dark));
        lock.execute_batch("ROLLBACK").unwrap();
        drop(service);
        let reopened = SourceService::with_credentials(dir.clone(), credentials).unwrap();
        assert!(reopened.jev_status().await.unwrap().credential_configured);
        assert_eq!(
            reopened.jev_status().await.unwrap().config.model,
            "jev-1.13.0"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn read_only_database_does_not_report_settings_saved() {
        let dir = temp_data_dir();
        let credentials = Arc::new(MemoryCredentials::default());
        let service = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        service.set_theme(DisplayTheme::Dark).await.unwrap();
        let database = dir.join("sessions.sqlite3");
        fs::set_permissions(&database, fs::Permissions::from_mode(0o444)).unwrap();
        let error = service.set_theme(DisplayTheme::Light).await.unwrap_err();
        assert!(matches!(error.code, ErrorCode::StorageFailed));
        assert!(error.cache_preserved);
        assert!(matches!(service.settings().await.0, DisplayTheme::Dark));
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
        drop(service);
        let reopened = SourceService::with_credentials(dir.clone(), credentials).unwrap();
        assert!(matches!(reopened.settings().await.0, DisplayTheme::Dark));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn service_upgrades_settings_with_keychain_reference_and_rejects_future_schema() {
        let dir = temp_data_dir();
        let credentials = Arc::new(MemoryCredentials::default());
        let original = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        original
            .save_jev(
                "https://api.typesafe.ai/gateway".into(),
                "jev-1.13.0".into(),
                Some("synthetic-only-key".into()),
            )
            .await
            .unwrap();
        let preferences = original.preferences.lock().await.clone();
        fs::write(
            dir.join("preferences.json"),
            serde_json::to_vec(&preferences).unwrap(),
        )
        .unwrap();
        drop(original);
        let database = dir.join("sessions.sqlite3");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("DROP TABLE app_preferences; PRAGMA user_version = 14;")
            .unwrap();
        drop(connection);
        let upgraded = SourceService::with_credentials(dir.clone(), credentials.clone()).unwrap();
        let status = upgraded.jev_status().await.unwrap();
        assert!(status.credential_configured);
        assert_eq!(status.config.model, "jev-1.13.0");
        assert_eq!(status.config.base_url, "https://api.typesafe.ai/gateway");
        fs::remove_file(dir.join("preferences.json")).unwrap();
        drop(upgraded);
        let reopened = SourceService::with_credentials(dir.clone(), credentials).unwrap();
        assert!(reopened.jev_status().await.unwrap().credential_configured);
        drop(reopened);
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 16;")
            .unwrap();
        drop(connection);
        let error =
            SourceService::with_credentials(dir.clone(), Arc::new(MemoryCredentials::default()))
                .err()
                .expect("future schema must be refused");
        assert!(matches!(error.code, ErrorCode::DatabaseTooNew));
        assert!(!error.retryable && error.cache_preserved);
        let version: i64 = rusqlite::Connection::open(&database)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 16);
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
        let persisted: i64 = rusqlite::Connection::open(dir.join("sessions.sqlite3"))
            .unwrap()
            .query_row("SELECT COUNT(*) FROM app_preferences", [], |row| row.get(0))
            .unwrap();
        assert_eq!(persisted, 0);
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
    fn existing_project_selection_does_not_wait_for_slow_reconciliation() {
        use std::{sync::mpsc, thread, time::Duration};

        let root = temp_data_dir();
        let first_project = root.join("first");
        let second_project = root.join("second");
        fs::create_dir_all(&first_project).unwrap();
        fs::create_dir_all(&second_project).unwrap();
        let service = Arc::new(
            SourceService::with_credentials(
                root.join("data"),
                Arc::new(MemoryCredentials::default()),
            )
            .unwrap(),
        );
        service
            .choose_project(first_project.to_str().unwrap())
            .unwrap();
        let catalog = service
            .choose_project(second_project.to_str().unwrap())
            .unwrap();
        let target_id = catalog
            .projects
            .iter()
            .find(|project| {
                project.root == fs::canonicalize(&first_project).unwrap().to_string_lossy()
            })
            .unwrap()
            .id
            .clone();

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
            .expect("重算应进入模拟慢归属解析阶段");

        let (choice_started_tx, choice_started_rx) = mpsc::channel();
        let (choice_done_tx, choice_done_rx) = mpsc::channel();
        let choosing = Arc::clone(&service);
        let selected_id = target_id.clone();
        let choice = thread::spawn(move || {
            choice_started_tx.send(()).unwrap();
            let result = choosing.choose_existing_project(&selected_id);
            choice_done_tx.send(result.is_ok()).unwrap();
            result
        });
        choice_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("已有项目选择线程应已启动");
        let responded_during_recompute = matches!(
            choice_done_rx.recv_timeout(Duration::from_secs(1)),
            Ok(true)
        );

        resume_tx.send(()).unwrap();
        refresh.join().unwrap().unwrap();
        let selected_catalog = choice.join().unwrap().unwrap();
        assert!(
            responded_during_recompute,
            "已有项目选择应在归属重算期间完成"
        );
        assert_eq!(
            selected_catalog.selected_project_id.as_deref(),
            Some(target_id.as_str())
        );
        let final_catalog = service.project_catalog().unwrap();
        assert_eq!(
            final_catalog.selected_project_id.as_deref(),
            Some(target_id.as_str())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn new_project_added_during_slow_reconciliation_is_attributed_afterward() {
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
                    turns_complete: false,
                    items_complete: false,
                    missing_from_source: false,
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
            .expect("重算应进入模拟慢归属解析阶段");

        let (choice_done_tx, choice_done_rx) = mpsc::channel();
        let choosing = Arc::clone(&service);
        let project_path = project.to_string_lossy().into_owned();
        let choice = thread::spawn(move || {
            let result = choosing.choose_project(&project_path);
            choice_done_tx.send(result.is_ok()).unwrap();
            result
        });
        let responded_during_recompute = matches!(
            choice_done_rx.recv_timeout(Duration::from_secs(1)),
            Ok(true)
        );

        resume_tx.send(()).unwrap();
        refresh.join().unwrap().unwrap();
        choice.join().unwrap().unwrap();
        assert!(
            responded_during_recompute,
            "新项目选择应能在另一轮慢归属重算期间完成"
        );

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
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
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

    #[tokio::test]
    async fn refresh_can_resume_after_cancelling_mid_response_line() {
        use tokio::time::{sleep, timeout, Duration};

        let root = temp_data_dir();
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("fake-cancel-partial.py");
        fs::write(
            &binary,
            include_bytes!("../../codex/tests/fixtures/fake_codex.py"),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();

        let service = Arc::new(SourceService::new(root.join("data")).unwrap());
        let connected = service
            .connect(Some(binary.to_string_lossy().into_owned()))
            .await
            .unwrap();
        assert!(matches!(connected.connection, ConnectionState::Connected));

        let cancelled = service.start_refresh(None, |_| {}).unwrap();
        timeout(Duration::from_secs(3), async {
            while !root.join("partial-response-started").exists() {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        service.cancel_refresh(&cancelled.id).unwrap();
        timeout(Duration::from_secs(3), async {
            while !service
                .refresh_status(&cancelled.id)
                .unwrap()
                .state
                .terminal()
            {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            service.refresh_status(&cancelled.id).unwrap().state,
            IndexRunState::Cancelled
        );

        fs::write(root.join("resume-partial-response"), "").unwrap();
        let resumed = service.start_refresh(None, |_| {}).unwrap();
        timeout(Duration::from_secs(3), async {
            while !service
                .refresh_status(&resumed.id)
                .unwrap()
                .state
                .terminal()
            {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            service.refresh_status(&resumed.id).unwrap().state,
            IndexRunState::Complete
        );
        assert!(matches!(
            service.status().await.connection,
            ConnectionState::Connected
        ));
        assert!(service
            .cached_sessions()
            .unwrap()
            .scopes
            .iter()
            .all(|scope| scope.complete));

        service.shutdown().await;
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn failed_thread_history_does_not_block_another_thread_or_erase_metadata() {
        let root = temp_data_dir();
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("fake-history-partial.py");
        fs::write(
            &binary,
            include_bytes!("../../codex/tests/fixtures/fake_codex.py"),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let service = SourceService::with_credentials(
            root.join("data"),
            Arc::new(MemoryCredentials::default()),
        )
        .unwrap();
        let connected = service
            .connect(Some(binary.to_string_lossy().into_owned()))
            .await
            .unwrap();
        assert!(matches!(connected.connection, ConnectionState::Connected));
        let metadata = |id: &str| codexflow_domain::ThreadMetadata {
            id: id.into(),
            session_id: format!("session-{id}"),
            title: Some(id.into()),
            preview: "测试".into(),
            cwd: "/tmp/example-project".into(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 100,
            updated_at: 200,
            archived: false,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: 1,
        };
        service
            .sessions
            .save_collection(&[metadata("thread-bad"), metadata("thread-h")], &[])
            .unwrap();
        let failed = service.load_thread_history("thread-bad").await.unwrap();
        assert!(failed.incompatible);
        assert!(!failed.items_complete);
        let partial = service.load_thread_history("thread-h").await.unwrap();
        assert!(partial.turns_complete);
        assert!(!partial.items_complete);
        assert_eq!(service.history_turns("thread-h", 0, 20).unwrap().total, 2);
        assert_eq!(
            service
                .history_items("thread-h", "turn-1", 0, 20)
                .unwrap()
                .total,
            1
        );
        assert_eq!(service.cached_sessions().unwrap().threads.len(), 2);
        assert_eq!(
            service
                .locate_history_item("thread-h", "turn-1", "item-1")
                .unwrap()
                .unwrap()
                .turn_id,
            "turn-1"
        );
        service.shutdown().await;
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn repeated_item_id_in_distinct_turns_survives_both_history_read_paths() {
        for (mode, path) in [
            (
                "duplicate-paged",
                codexflow_domain::HistoryReadPath::Paginated,
            ),
            (
                "duplicate-legacy",
                codexflow_domain::HistoryReadPath::FullRead,
            ),
        ] {
            let root = temp_data_dir();
            fs::create_dir_all(&root).unwrap();
            let binary = root.join(format!("fake-history-{mode}.py"));
            fs::write(
                &binary,
                include_bytes!("../../codex/tests/fixtures/fake_codex.py"),
            )
            .unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
            let service = SourceService::with_credentials(
                root.join("data"),
                Arc::new(MemoryCredentials::default()),
            )
            .unwrap();
            service
                .connect(Some(binary.to_string_lossy().into_owned()))
                .await
                .unwrap();
            service
                .sessions
                .save_collection(
                    &[codexflow_domain::ThreadMetadata {
                        id: "thread-h".into(),
                        session_id: "session-thread-h".into(),
                        title: Some("历史会话".into()),
                        preview: "测试".into(),
                        cwd: "/tmp/example-project".into(),
                        project_id: None,
                        source_kind: "cli".into(),
                        source_detail: None,
                        thread_source: None,
                        parent_thread_id: None,
                        forked_from_id: None,
                        git: None,
                        created_at: 100,
                        updated_at: 200,
                        archived: false,
                        metadata_complete: true,
                        turns_complete: false,
                        items_complete: false,
                        missing_from_source: false,
                        content_complete: false,
                        read_error: None,
                        observed_at_unix_ms: 1,
                    }],
                    &[],
                )
                .unwrap();
            let coverage = service.load_thread_history("thread-h").await.unwrap();
            assert!(coverage.items_complete, "{mode}: {:?}", coverage.error);
            assert_eq!(coverage.path, path);
            assert_eq!(coverage.loaded_items, 2);
            for (turn_id, turn_offset) in [("turn-1", 0), ("turn-2", 1)] {
                let page = service.history_items("thread-h", turn_id, 0, 20).unwrap();
                assert_eq!(page.total, 1, "{mode}: {turn_id}");
                assert_eq!(page.items[0].id, "item-1");
                assert_eq!(page.items[0].turn_id, turn_id);
                let location = service
                    .locate_history_item("thread-h", turn_id, "item-1")
                    .unwrap()
                    .unwrap();
                assert_eq!(location.turn_id, turn_id);
                assert_eq!(location.turn_offset, turn_offset);
                assert_eq!(location.offset, 0);
            }
            service.shutdown().await;
            let reopened = SessionStore::new(root.join("data")).unwrap();
            assert_eq!(
                reopened
                    .history_items("thread-h", "turn-1", 0, 20)
                    .unwrap()
                    .total,
                1
            );
            assert_eq!(
                reopened
                    .history_items("thread-h", "turn-2", 0, 20)
                    .unwrap()
                    .total,
                1
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn facts_persist_and_evidence_checks_hierarchy_excerpt_and_version() {
        use codexflow_domain::{
            HistoryItem, HistoryReadPath, HistorySnapshot, HistoryTurn, ThreadMetadata,
        };
        let root = temp_data_dir();
        let service =
            SourceService::with_credentials(root.clone(), Arc::new(MemoryCredentials::default()))
                .unwrap();
        let thread = ThreadMetadata {
            id: "fact-thread".into(),
            session_id: "session".into(),
            title: None,
            preview: String::new(),
            cwd: "/tmp".into(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: None,
            forked_from_id: None,
            git: None,
            created_at: 0,
            updated_at: 100,
            archived: false,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: 1,
        };
        service
            .sessions
            .save_collection(&[thread.clone()], &[])
            .unwrap();
        let make_turn = |id: &str, ordinal| HistoryTurn {
            thread_id: thread.id.clone(),
            id: id.into(),
            ordinal,
            status: "completed".into(),
            started_at_unix_ms: None,
            completed_at_unix_ms: None,
            duration_ms: None,
            time_error: None,
            source_updated_at: 100,
            content_version: "turn-v1".into(),
        };
        let make_item = |turn_id: &str, command: &str| HistoryItem {
            thread_id: thread.id.clone(),
            turn_id: turn_id.into(),
            id: "same-item".into(),
            ordinal: 0,
            source_type: "commandExecution".into(),
            supported: true,
            text: None,
            command: Some(command.into()),
            cwd: None,
            output: None,
            exit_code: Some(0),
            status: Some("completed".into()),
            changes: vec![],
            source_updated_at: 100,
            content_version: format!("version-{turn_id}"),
        };
        let snapshot = HistorySnapshot {
            coverage: HistoryCoverage {
                thread_id: thread.id.clone(),
                source_updated_at: 100,
                attempted_at_unix_ms: 1,
                path: HistoryReadPath::FullRead,
                turns_complete: true,
                items_complete: true,
                turn_pages: 0,
                item_pages: 0,
                loaded_turns: 2,
                loaded_items: 2,
                incompatible: false,
                error: None,
            },
            turns: vec![make_turn("turn-1", 0), make_turn("turn-2", 1)],
            items: vec![
                make_item("turn-1", "cargo test"),
                make_item("turn-2", "cargo build"),
            ],
        };
        service.sessions.save_history(&snapshot).unwrap();
        let facts = service.source_facts(&thread.id, 0, 1).unwrap();
        assert_eq!(facts.total, 2);
        assert_eq!(facts.facts.len(), 1);
        assert_eq!(service.source_evidence(&thread.id, 0, 1).unwrap().total, 2);
        let evidence = service.source_evidence(&thread.id, 0, 10).unwrap().evidence;
        assert_ne!(evidence[0].id, evidence[1].id);
        assert_eq!(
            service
                .validate_source_evidence(&evidence[0].id)
                .unwrap()
                .state,
            EvidenceState::Valid
        );
        assert_eq!(
            service
                .validate_source_evidence(&evidence[0].id)
                .unwrap()
                .location
                .unwrap()
                .turn_offset,
            0
        );
        let mut wrong = evidence[0].clone();
        wrong.turn_id = "turn-2".into();
        assert_eq!(
            service.check_evidence(&wrong).unwrap().state,
            EvidenceState::WrongHierarchy
        );
        wrong.turn_id = "missing-turn".into();
        assert_eq!(
            service.check_evidence(&wrong).unwrap().state,
            EvidenceState::MissingTurn
        );
        wrong = evidence[0].clone();
        wrong.item_id = "missing-item".into();
        assert_eq!(
            service.check_evidence(&wrong).unwrap().state,
            EvidenceState::MissingItem
        );
        wrong = evidence[0].clone();
        wrong.fact_id = "missing-fact".into();
        let missing_fact = service.check_evidence(&wrong).unwrap();
        assert_eq!(missing_fact.state, EvidenceState::MissingFact);
        assert!(missing_fact.message.contains("事实不存在"));
        wrong = evidence[0].clone();
        wrong.excerpt = "does not appear".into();
        assert_eq!(
            service.check_evidence(&wrong).unwrap().state,
            EvidenceState::ExcerptMissing
        );
        wrong = evidence[0].clone();
        wrong.content_version = "old".into();
        assert_eq!(
            service.check_evidence(&wrong).unwrap().state,
            EvidenceState::StaleVersion
        );
        let mut changed_thread = thread.clone();
        changed_thread.updated_at = 101;
        service
            .sessions
            .save_collection(&[changed_thread], &[])
            .unwrap();
        assert_eq!(
            service.check_evidence(&evidence[0]).unwrap().state,
            EvidenceState::StaleVersion
        );
        drop(service);
        let reopened =
            SourceService::with_credentials(root.clone(), Arc::new(MemoryCredentials::default()))
                .unwrap();
        assert_eq!(reopened.source_facts(&thread.id, 0, 10).unwrap().total, 2);
        let mut updated = snapshot.clone();
        updated.coverage.source_updated_at = 101;
        updated.items[0].command = Some("cargo clippy".into());
        updated.items[0].content_version = "new-content-version".into();
        for item in &mut updated.items {
            item.source_updated_at = 101;
        }
        for turn in &mut updated.turns {
            turn.source_updated_at = 101;
        }
        reopened.sessions.save_history(&updated).unwrap();
        let rebuilt = reopened.source_facts(&thread.id, 0, 10).unwrap();
        assert_eq!(rebuilt.total, 2);
        assert!(rebuilt
            .facts
            .iter()
            .any(|fact| fact.subject == "cargo clippy"));
        assert!(!rebuilt
            .facts
            .iter()
            .any(|fact| fact.subject == "cargo test"));
        assert_eq!(
            reopened.source_facts(&thread.id, 0, 10).unwrap().facts[0].id,
            rebuilt.facts[0].id
        );
        let connection = rusqlite::Connection::open(root.join("sessions.sqlite3")).unwrap();
        connection
            .execute(
                "UPDATE fact_index SET rule_version='old-rule' WHERE thread_id=?1",
                [&thread.id],
            )
            .unwrap();
        drop(connection);
        assert_eq!(reopened.source_facts(&thread.id, 0, 10).unwrap().total, 2);
        assert_eq!(
            reopened.sessions.fact_index(&thread.id).unwrap().unwrap().1,
            facts::RULE_VERSION
        );
        let current_evidence = reopened
            .source_evidence(&thread.id, 0, 10)
            .unwrap()
            .evidence;
        let orphan = &current_evidence[0];
        let mut mismatched_fact = rebuilt.facts[0].clone();
        mismatched_fact.content_version = "wrong-fact-version".into();
        let (digest, rule, generation) = reopened.sessions.fact_index(&thread.id).unwrap().unwrap();
        reopened
            .sessions
            .replace_automatic_facts(
                &thread.id,
                reopened
                    .sessions
                    .thread(&thread.id)
                    .unwrap()
                    .unwrap()
                    .updated_at,
                &digest,
                &rule,
                generation,
                &[mismatched_fact],
                &[orphan.clone()],
            )
            .unwrap();
        assert_eq!(
            reopened.validate_source_evidence(&orphan.id).unwrap().state,
            EvidenceState::StaleVersion
        );
        let connection = rusqlite::Connection::open(root.join("sessions.sqlite3")).unwrap();
        connection
            .execute("DELETE FROM source_facts WHERE id=?1", [&orphan.fact_id])
            .unwrap();
        drop(connection);
        let check = reopened.validate_source_evidence(&orphan.id).unwrap();
        assert_eq!(check.state, EvidenceState::MissingFact);
        assert!(check.message.contains("事实不存在"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn paged_refresh_can_be_cancelled_and_resumed_without_losing_cache_or_project() {
        use tokio::time::{sleep, timeout, Duration};
        let root = temp_data_dir();
        fs::create_dir_all(&root).unwrap();
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        for mode in [
            "list-rich",
            "list-rich-second-gate",
            "list-partial",
            "list-rich-exit-second",
        ] {
            let binary = root.join(format!("fake-{mode}.py"));
            fs::write(
                &binary,
                include_bytes!("../../codex/tests/fixtures/fake_codex.py"),
            )
            .unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let service = Arc::new(SourceService::new(root.join("data")).unwrap());
        let project_id = service
            .choose_project(project.to_str().unwrap())
            .unwrap()
            .selected_project_id
            .unwrap();
        service
            .connect(Some(
                root.join("fake-list-rich.py")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await
            .unwrap();
        let first = service
            .start_refresh(Some(project_id.clone()), |_| {})
            .unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if service.refresh_status(&first.id).unwrap().state.terminal() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            service.refresh_status(&first.id).unwrap().state,
            IndexRunState::Complete
        );
        assert_eq!(service.cached_sessions().unwrap().threads.len(), 4);

        service
            .connect(Some(
                root.join("fake-list-rich-second-gate.py")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await
            .unwrap();
        fs::write(root.join("pause-refresh"), "").unwrap();
        let second = service
            .start_refresh(Some(project_id.clone()), |_| {})
            .unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if root.join("refresh-paused").exists()
                    && service.refresh_status(&second.id).unwrap().pages_saved > 0
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            service.refresh_status(&second.id).unwrap().state,
            IndexRunState::Running
        );
        assert!(service
            .cached_sessions()
            .unwrap()
            .threads
            .iter()
            .all(|thread| !thread.missing_from_source));
        service.cancel_refresh(&second.id).unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if service.refresh_status(&second.id).unwrap().state.terminal() {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            service.refresh_status(&second.id).unwrap().state,
            IndexRunState::Cancelled
        );
        assert_eq!(service.cached_sessions().unwrap().threads.len(), 4);
        fs::remove_file(root.join("pause-refresh")).unwrap();
        service.shutdown().await;
        drop(service);

        let reopened = Arc::new(SourceService::new(root.join("data")).unwrap());
        assert_eq!(
            reopened
                .project_catalog()
                .unwrap()
                .selected_project_id
                .as_deref(),
            Some(project_id.as_str())
        );
        assert_eq!(
            reopened.refresh_status(&second.id).unwrap().state,
            IndexRunState::Cancelled
        );
        assert_eq!(reopened.cached_sessions().unwrap().threads.len(), 4);
        reopened
            .connect(Some(
                root.join("fake-list-partial.py")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await
            .unwrap();
        let partial = reopened
            .start_refresh(Some(project_id.clone()), |_| {})
            .unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if reopened
                    .refresh_status(&partial.id)
                    .unwrap()
                    .state
                    .terminal()
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            reopened.refresh_status(&partial.id).unwrap().state,
            IndexRunState::Partial
        );
        assert_eq!(reopened.cached_sessions().unwrap().threads.len(), 4);
        assert!(reopened
            .cached_sessions()
            .unwrap()
            .threads
            .iter()
            .all(|thread| !thread.missing_from_source));
        reopened
            .connect(Some(
                root.join("fake-list-rich-exit-second.py")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await
            .unwrap();
        let exited = reopened
            .start_refresh(Some(project_id.clone()), |_| {})
            .unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if reopened
                    .refresh_status(&exited.id)
                    .unwrap()
                    .state
                    .terminal()
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            reopened.refresh_status(&exited.id).unwrap().state,
            IndexRunState::Partial
        );
        assert_eq!(reopened.cached_sessions().unwrap().threads.len(), 4);
        reopened.shutdown().await;
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_graph_preserves_observed_structure_and_diagnostics_across_reconciliation() {
        use codexflow_domain::{ObservedRelationKind, ParentEndpoint, ThreadMetadata};

        let root = temp_data_dir();
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let service = SourceService::with_credentials(
            root.join("data"),
            Arc::new(MemoryCredentials::default()),
        )
        .unwrap();
        let thread = |id: &str, cwd: &std::path::Path, fork: Option<&str>, parent: Option<&str>| {
            ThreadMetadata {
                id: id.into(),
                session_id: "shared-session-id".into(),
                title: Some(format!("会话 {id}")),
                preview: String::new(),
                cwd: cwd.to_string_lossy().into_owned(),
                project_id: None,
                source_kind: "cli".into(),
                source_detail: None,
                thread_source: None,
                parent_thread_id: parent.map(str::to_owned),
                forked_from_id: fork.map(str::to_owned),
                git: None,
                created_at: 1,
                updated_at: 2,
                archived: false,
                metadata_complete: true,
                turns_complete: false,
                items_complete: false,
                missing_from_source: false,
                content_complete: false,
                read_error: None,
                observed_at_unix_ms: 3,
            }
        };
        let threads = vec![
            thread("root", &first, None, None),
            thread("fork", &first, Some("root"), None),
            thread("subagent", &first, None, Some("root")),
            thread("both", &first, Some("root"), Some("root")),
            thread("missing", &first, Some("not-cached"), None),
            thread("cross", &first, None, Some("external")),
            thread("self", &first, None, Some("self")),
            thread("cycle-a", &first, None, Some("cycle-b")),
            thread("cycle-b", &first, None, Some("cycle-a")),
            thread("unassigned", &first, None, Some("unknown-owner")),
            thread("unknown-owner", &root.join("gone"), None, None),
            thread("external", &second, None, None),
        ];
        service.sessions.save_collection(&threads, &[]).unwrap();
        let first_id = service
            .choose_project(first.to_str().unwrap())
            .unwrap()
            .selected_project_id
            .unwrap();
        service.choose_project(second.to_str().unwrap()).unwrap();

        let graph = service.project_graph(&first_id).unwrap();
        assert_eq!(graph.relations.len(), 9);
        assert!(graph.relations.iter().all(|relation| {
            relation.source == "observed"
                && relation.confidence == 1.0
                && relation.to_thread_id != relation.from_thread_id
        }));
        let edge = |child: &str, kind| {
            graph
                .relations
                .iter()
                .find(|relation| relation.to_thread_id == child && relation.kind == kind)
                .unwrap()
        };
        assert_eq!(
            edge("fork", ObservedRelationKind::ForkedFrom).from_thread_id,
            "root"
        );
        assert_eq!(
            edge("fork", ObservedRelationKind::ForkedFrom).source_field,
            "forkedFromId"
        );
        assert_eq!(
            edge("subagent", ObservedRelationKind::SubagentOf).from_thread_id,
            "root"
        );
        assert_eq!(
            edge("subagent", ObservedRelationKind::SubagentOf).source_field,
            "parentThreadId"
        );
        assert_ne!(
            edge("both", ObservedRelationKind::ForkedFrom).id,
            edge("both", ObservedRelationKind::SubagentOf).id
        );
        assert_eq!(
            edge("missing", ObservedRelationKind::ForkedFrom).parent_endpoint,
            ParentEndpoint::Missing
        );
        assert_eq!(
            edge("cross", ObservedRelationKind::SubagentOf).parent_endpoint,
            ParentEndpoint::OutsideProject
        );
        assert_eq!(
            edge("unassigned", ObservedRelationKind::SubagentOf).parent_endpoint,
            ParentEndpoint::Unassigned
        );
        assert_eq!(
            edge("cycle-a", ObservedRelationKind::SubagentOf).from_thread_id,
            "cycle-b"
        );
        assert_eq!(
            edge("cycle-b", ObservedRelationKind::SubagentOf).from_thread_id,
            "cycle-a"
        );
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.id == "not-cached" && node.reference_only && node.title.is_none()));
        assert!(graph
            .nodes
            .iter()
            .any(|node| node.id == "external" && node.reference_only && node.title.is_none()));
        assert!(graph
            .diagnostics
            .iter()
            .any(|item| item.thread_id == "self" && item.source_field == "parentThreadId"));

        let identities: Vec<_> = graph
            .relations
            .iter()
            .map(|relation| relation.id.clone())
            .collect();
        service.sessions.save_collection(&threads, &[]).unwrap();
        service.reconcile_projects().unwrap();
        let refreshed = service.project_graph(&first_id).unwrap();
        assert_eq!(
            refreshed
                .relations
                .iter()
                .map(|relation| relation.id.clone())
                .collect::<Vec<_>>(),
            identities
        );
        drop(service);
        let reopened = SourceService::with_credentials(
            root.join("data"),
            Arc::new(MemoryCredentials::default()),
        )
        .unwrap();
        assert_eq!(
            reopened.project_graph(&first_id).unwrap().relations.len(),
            9
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workstream_corrections_survive_regrouping_restart_and_restore_independently() {
        use codexflow_domain::ThreadMetadata;
        let root = temp_data_dir();
        let project_path = root.join("project");
        fs::create_dir_all(&project_path).unwrap();
        let data = root.join("data");
        let service =
            SourceService::with_credentials(data.clone(), Arc::new(MemoryCredentials::default()))
                .unwrap();
        let thread = |id: &str, parent: Option<&str>, updated_at| ThreadMetadata {
            id: id.into(),
            session_id: id.into(),
            title: Some(id.into()),
            preview: id.into(),
            cwd: project_path.to_string_lossy().into_owned(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: parent.map(str::to_owned),
            forked_from_id: None,
            git: None,
            created_at: 1,
            updated_at,
            archived: false,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: updated_at,
        };
        service
            .sessions
            .save_collection(
                &[
                    thread("a", None, 2),
                    thread("b", Some("a"), 2),
                    thread("c", None, 2),
                    thread("d", Some("c"), 2),
                    thread("x", None, 2),
                ],
                &[],
            )
            .unwrap();
        let project_id = service
            .choose_project(project_path.to_str().unwrap())
            .unwrap()
            .selected_project_id
            .unwrap();
        let mut view = service.project_workstreams(&project_id).unwrap();
        assert_eq!(view.workstreams.len(), 2);
        let first = view
            .workstreams
            .iter()
            .find(|stream| stream.members == ["a", "b"])
            .unwrap()
            .id
            .clone();
        let second = view
            .workstreams
            .iter()
            .find(|stream| stream.members == ["c", "d"])
            .unwrap()
            .id
            .clone();
        let stale_revision = view.revision;
        service
            .rename_workstream(&project_id, &first, "人工工作", view.revision)
            .unwrap();
        assert!(matches!(
            service.move_thread_to_workstream(&project_id, "a", Some(&second), stale_revision),
            Err(AppError {
                code: ErrorCode::ConcurrentModification,
                ..
            })
        ));
        view = service.project_workstreams(&project_id).unwrap();
        service
            .move_thread_to_workstream(&project_id, "a", Some(&second), view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        service
            .move_thread_to_workstream(&project_id, "b", Some(&second), view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == first)
                .unwrap()
                .members,
            Vec::<String>::new()
        );
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == first)
                .unwrap()
                .name,
            "人工工作"
        );
        service
            .move_thread_to_workstream(&project_id, "x", Some(&first), view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        service
            .move_thread_to_workstream(&project_id, "c", None, view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == first)
                .unwrap()
                .members,
            ["x"]
        );
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == second)
                .unwrap()
                .members,
            ["a", "b", "d"]
        );
        assert_eq!(view.ungrouped_thread_ids, ["c"]);
        assert_eq!(
            view.workstreams
                .iter()
                .flat_map(|stream| stream.members.iter())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            4
        );

        service
            .sessions
            .save_collection(&[thread("c", Some("a"), 3)], &[])
            .unwrap();
        service.reconcile_projects().unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == first)
                .unwrap()
                .name,
            "人工工作"
        );
        assert!(view.workstreams.iter().any(|stream| stream.id == second));
        assert!(view
            .workstreams
            .iter()
            .find(|stream| stream.id == second)
            .unwrap()
            .members
            .contains(&"b".into()));
        assert_eq!(view.ungrouped_thread_ids, ["c"]);

        drop(service);
        let service =
            SourceService::with_credentials(data, Arc::new(MemoryCredentials::default())).unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == first)
                .unwrap()
                .name,
            "人工工作"
        );
        assert!(view.manually_assigned_thread_ids.contains(&"c".into()));
        service
            .sessions
            .save_collection(&[thread("c", None, 4)], &[])
            .unwrap();
        service.reconcile_projects().unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        assert_eq!(view.workstreams.len(), 2);
        service
            .restore_thread_workstream(&project_id, "a", view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        service
            .restore_thread_workstream(&project_id, "b", view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        service
            .restore_thread_workstream(&project_id, "c", view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == first)
                .unwrap()
                .name,
            "人工工作"
        );
        service
            .restore_workstream_name(&project_id, &first, view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        assert_ne!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == first)
                .unwrap()
                .name,
            "人工工作"
        );
        assert!(view.manually_named_workstream_ids.is_empty());
        assert!(view
            .workstreams
            .iter()
            .find(|stream| stream.id == first)
            .unwrap()
            .members
            .contains(&"x".into()));
        service
            .restore_thread_workstream(&project_id, "x", view.revision)
            .unwrap();
        view = service.project_workstreams(&project_id).unwrap();
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == first)
                .unwrap()
                .members,
            ["a", "b"]
        );
        assert_eq!(
            view.workstreams
                .iter()
                .find(|stream| stream.id == second)
                .unwrap()
                .members,
            ["c", "d"]
        );
        assert_eq!(view.ungrouped_thread_ids, ["x"]);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn complete_cached_project_recovers_through_public_service_after_restart() {
        use codexflow_domain::{
            AnalysisRun, AnalysisRunState, AnalysisStage, AnalysisUnit, AnalysisUnitState,
            CandidateEvidence, CausalTimeCheck, EvidenceField, EvidencePair, EvidenceState,
            HistoryItem, HistoryReadPath, HistorySnapshot, HistoryTurn, IndexRun, IndexRunState,
            InferredRelation, InferredRelationKind, SummaryRun, SummaryRunState, ThreadMetadata,
            ThreadSummary, ThreadSummaryContent, UserRelationDecision,
        };

        let root = temp_data_dir();
        let project_path = root.join("project");
        let data = root.join("data");
        fs::create_dir_all(&project_path).unwrap();
        let credentials = Arc::new(MemoryCredentials::default());
        let service = SourceService::with_credentials(data.clone(), credentials.clone()).unwrap();
        service
            .save_jev(
                "https://api.typesafe.ai".into(),
                "jev-1.13.0".into(),
                Some("synthetic-only-key".into()),
            )
            .await
            .unwrap();
        let thread = |id: &str, parent: Option<&str>| ThreadMetadata {
            id: id.into(),
            session_id: id.into(),
            title: Some(id.into()),
            preview: id.into(),
            cwd: project_path.to_string_lossy().into_owned(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: parent.map(str::to_owned),
            forked_from_id: None,
            git: None,
            created_at: 1,
            updated_at: 2,
            archived: false,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: 2,
        };
        service
            .sessions
            .save_collection(
                &[thread("a", None), thread("b", Some("a")), thread("x", None)],
                &[],
            )
            .unwrap();
        let project_id = service
            .choose_project(project_path.to_str().unwrap())
            .unwrap()
            .selected_project_id
            .unwrap();
        let snapshot = HistorySnapshot {
            coverage: codexflow_domain::HistoryCoverage {
                thread_id: "a".into(),
                source_updated_at: 2,
                attempted_at_unix_ms: 3,
                path: HistoryReadPath::FullRead,
                turns_complete: true,
                items_complete: true,
                turn_pages: 0,
                item_pages: 0,
                loaded_turns: 1,
                loaded_items: 1,
                incompatible: false,
                error: None,
            },
            turns: vec![HistoryTurn {
                thread_id: "a".into(),
                id: "turn".into(),
                ordinal: 0,
                status: "completed".into(),
                started_at_unix_ms: Some(10),
                completed_at_unix_ms: Some(20),
                duration_ms: Some(10),
                time_error: None,
                source_updated_at: 2,
                content_version: "turn-v1".into(),
            }],
            items: vec![HistoryItem {
                thread_id: "a".into(),
                turn_id: "turn".into(),
                id: "item".into(),
                ordinal: 0,
                source_type: "commandExecution".into(),
                supported: true,
                text: None,
                command: Some("cargo test".into()),
                cwd: None,
                output: None,
                exit_code: Some(0),
                status: Some("completed".into()),
                changes: vec![],
                source_updated_at: 2,
                content_version: "item-v1".into(),
            }],
        };
        service.sessions.save_history(&snapshot).unwrap();
        let evidence = service
            .source_evidence("a", 0, 10)
            .unwrap()
            .evidence
            .remove(0);
        let summary = ThreadSummary {
            thread_id: "a".into(),
            content: ThreadSummaryContent {
                goal: "验证项目".into(),
                activity: "运行测试".into(),
                outcome: "通过".into(),
                decisions: "保留实现".into(),
                issues: "无".into(),
            },
            evidence_ids: vec![evidence.id.clone()],
            evidence_refs: vec![],
            model: "synthetic-model".into(),
            requested_model: None,
            service_base_url: None,
            binary_path: None,
            binary_fingerprint: None,
            binary_version: None,
            input_digest: "synthetic".into(),
            source_updated_at: 2,
            history_generation: service.sessions.history_generation("a").unwrap(),
            created_at_unix_ms: 4,
        };
        assert!(service
            .sessions
            .save_summary_if_current(&summary, summary.history_generation)
            .unwrap());
        let mut streams = service.project_workstreams(&project_id).unwrap();
        let stream_id = streams
            .workstreams
            .iter()
            .find(|stream| stream.members == ["a", "b"])
            .unwrap()
            .id
            .clone();
        service
            .rename_workstream(&project_id, &stream_id, "人工主线", streams.revision)
            .unwrap();
        streams = service.project_workstreams(&project_id).unwrap();
        service
            .move_thread_to_workstream(&project_id, "x", Some(&stream_id), streams.revision)
            .unwrap();

        let relation_id = inferred::relation_id("a", "b", InferredRelationKind::Related);
        let candidate_evidence = |thread_id: &str| CandidateEvidence {
            id: format!("evidence-{thread_id}"),
            thread_id: thread_id.into(),
            turn_id: "turn".into(),
            item_id: "item".into(),
            field: EvidenceField::Command,
            change_index: None,
            excerpt: "cargo test".into(),
            content_version: "item-v1".into(),
            fact_id: None,
        };
        let relation = InferredRelation {
            id: relation_id.clone(),
            project_id: project_id.clone(),
            candidate_id: "candidate".into(),
            from_thread_id: "a".into(),
            to_thread_id: "b".into(),
            kind: InferredRelationKind::Related,
            source: "jev".into(),
            requested_model: "jev-1.13.0".into(),
            actual_model: "jev-1.13.0".into(),
            confidence: 0.8,
            probabilities: Default::default(),
            evidence_confidence: 0.8,
            evidence_probabilities: Default::default(),
            evidence_options: Vec::new(),
            selected_evidence_option: None,
            evidence: EvidencePair {
                id: "pair".into(),
                left: candidate_evidence("a"),
                right: candidate_evidence("b"),
            },
            time_check: CausalTimeCheck::Unverifiable,
            explanation: "合成关系".into(),
            input_version: "synthetic-v1".into(),
        };
        service
            .sessions
            .save_relation_review(
                &project_id,
                &relation_id,
                UserRelationDecision::Rejected,
                None,
                0,
                Some(&relation),
            )
            .unwrap();
        service
            .sessions
            .save_index_run(&IndexRun {
                id: "index-interrupted".into(),
                project_id: Some(project_id.clone()),
                state: IndexRunState::Running,
                started_at_unix_ms: 5,
                finished_at_unix_ms: None,
                pages_saved: 1,
                threads_seen: 3,
                error: None,
                interrupted: false,
            })
            .unwrap();
        service
            .sessions
            .save_summary_run(&SummaryRun {
                id: "summary-interrupted".into(),
                thread_id: "a".into(),
                state: SummaryRunState::Running,
                model: "synthetic-model".into(),
                started_at_unix_ms: 6,
                finished_at_unix_ms: None,
                temporary_thread_id: None,
                turn_id: None,
                reused_cache: false,
                error: None,
            })
            .unwrap();
        service
            .sessions
            .save_analysis_run(&AnalysisRun {
                id: "analysis-interrupted".into(),
                project_id: project_id.clone(),
                state: AnalysisRunState::Running,
                pause_reason: None,
                input_version: "synthetic-v1".into(),
                codex_binary: None,
                codex_binary_fingerprint: None,
                codex_version: None,
                codex_model: "synthetic-model".into(),
                text_base_url: String::new(),
                text_config_revision: 0,
                jev_base_url: "https://api.typesafe.ai".into(),
                jev_model: "jev-1.13.0".into(),
                jev_rules_version: "synthetic".into(),
                jev_config_revision: 1,
                jev_pinned_model: Some("jev-1.13.0".into()),
                jev_probe_attempts: 0,
                limits: Default::default(),
                stage_selection: codexflow_domain::AnalysisStageSelection::default(),
                batch_number: 1,
                batch_calls: 1,
                total_calls: 1,
                total_questions: 0,
                input_tokens: None,
                output_tokens: None,
                processed: 1,
                succeeded: 1,
                failed: 0,
                pending: 1,
                units: [AnalysisUnitState::Succeeded, AnalysisUnitState::Running]
                    .into_iter()
                    .enumerate()
                    .map(|(index, state)| AnalysisUnit {
                        id: format!("unit-{index}"),
                        stage: AnalysisStage::Summary,
                        input_version: "synthetic-v1".into(),
                        state,
                        attempts: 1,
                        active_summary_run_id: Some("summary-interrupted".into()),
                        requested_model: "synthetic-model".into(),
                        actual_model: None,
                        error: None,
                        relation_classification: None,
                        relation_evidence_selection: None,
                    })
                    .collect(),
                relations_planned: false,
                names_planned: false,
                relation_only: false,
                started_at_unix_ms: 7,
                finished_at_unix_ms: None,
                interrupted: false,
                error: None,
            })
            .unwrap();
        let previous_settings = service.preferences.lock().await.clone();
        fs::write(
            data.join("preferences.json"),
            serde_json::to_vec(&previous_settings).unwrap(),
        )
        .unwrap();
        drop(service);

        let connection = rusqlite::Connection::open(data.join("sessions.sqlite3")).unwrap();
        connection
            .execute_batch("DROP TABLE app_preferences; PRAGMA user_version = 14;")
            .unwrap();
        drop(connection);

        let reopened = SourceService::with_credentials(data, credentials).unwrap();
        assert_eq!(
            reopened
                .project_catalog()
                .unwrap()
                .selected_project_id
                .as_deref(),
            Some(project_id.as_str())
        );
        assert_eq!(
            reopened
                .project_sessions(&project_id)
                .unwrap()
                .threads
                .len(),
            3
        );
        assert_eq!(reopened.source_facts("a", 0, 10).unwrap().total, 1);
        assert_eq!(
            reopened
                .validate_source_evidence(&evidence.id)
                .unwrap()
                .state,
            EvidenceState::Valid
        );
        assert_eq!(
            reopened
                .summary_preview("a")
                .await
                .unwrap()
                .cached_summary
                .unwrap()
                .content
                .goal,
            "验证项目"
        );
        assert!(reopened.jev_status().await.unwrap().credential_configured);
        let reviewed = reopened
            .project_graph(&project_id)
            .unwrap()
            .reviewed_relations;
        assert_eq!(
            reviewed
                .iter()
                .find(|item| item.relation.id == relation_id)
                .unwrap()
                .review
                .decision,
            UserRelationDecision::Rejected
        );
        let streams = reopened.project_workstreams(&project_id).unwrap();
        assert_eq!(
            streams
                .workstreams
                .iter()
                .find(|stream| stream.id == stream_id)
                .unwrap()
                .name,
            "人工主线"
        );
        assert!(streams
            .workstreams
            .iter()
            .find(|stream| stream.id == stream_id)
            .unwrap()
            .members
            .contains(&"x".into()));
        let run = reopened.latest_analysis_run(&project_id).unwrap().unwrap();
        assert_eq!(run.state, AnalysisRunState::Paused);
        assert!(run.interrupted);
        assert_eq!(run.units[0].state, AnalysisUnitState::Succeeded);
        assert_eq!(run.units[1].state, AnalysisUnitState::Pending);
        assert!(reopened.latest_refresh().unwrap().unwrap().interrupted);
        assert_eq!(
            reopened.latest_summary_run("a").unwrap().unwrap().state,
            SummaryRunState::Failed
        );
        reopened
            .sessions
            .save_collection(
                &[thread("a", None), thread("b", Some("a")), thread("x", None)],
                &[],
            )
            .unwrap();
        reopened.reconcile_projects().unwrap();
        assert_eq!(
            reopened
                .project_graph(&project_id)
                .unwrap()
                .reviewed_relations[0]
                .review
                .decision,
            UserRelationDecision::Rejected
        );
        assert_eq!(
            reopened
                .project_workstreams(&project_id)
                .unwrap()
                .workstreams
                .iter()
                .find(|stream| stream.id == stream_id)
                .unwrap()
                .name,
            "人工主线"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn regroup_retries_when_a_correction_commits_after_its_snapshot() {
        use codexflow_domain::ThreadMetadata;
        let root = temp_data_dir();
        let project_path = root.join("project");
        fs::create_dir_all(&project_path).unwrap();
        let service = SourceService::with_credentials(
            root.join("data"),
            Arc::new(MemoryCredentials::default()),
        )
        .unwrap();
        let thread = |id: &str, parent: Option<&str>, updated_at| ThreadMetadata {
            id: id.into(),
            session_id: id.into(),
            title: Some(id.into()),
            preview: id.into(),
            cwd: project_path.to_string_lossy().into_owned(),
            project_id: None,
            source_kind: "cli".into(),
            source_detail: None,
            thread_source: None,
            parent_thread_id: parent.map(str::to_owned),
            forked_from_id: None,
            git: None,
            created_at: 1,
            updated_at,
            archived: false,
            metadata_complete: true,
            turns_complete: false,
            items_complete: false,
            missing_from_source: false,
            content_complete: false,
            read_error: None,
            observed_at_unix_ms: updated_at,
        };
        service
            .sessions
            .save_collection(
                &[
                    thread("a", None, 2),
                    thread("b", Some("a"), 2),
                    thread("x", None, 2),
                ],
                &[],
            )
            .unwrap();
        let project_id = service
            .choose_project(project_path.to_str().unwrap())
            .unwrap()
            .selected_project_id
            .unwrap();
        let first = service.project_workstreams(&project_id).unwrap();
        let target = first.workstreams[0].id.clone();
        assert_eq!(first.workstreams[0].members, ["a", "b"]);

        service
            .sessions
            .save_collection(&[thread("b", None, 3)], &[])
            .unwrap();
        service.reconcile_projects().unwrap();
        let changed_graph = service.project_graph(&project_id).unwrap();
        assert!(changed_graph.relations.is_empty());
        let automatic = service
            .automatic_workstreams_for_graph_with_hook(&changed_graph, || {
                // This lands after the regroup has read its old stream and correction snapshot,
                // before its replacement transaction. The old implementation deleted target.
                let revision = service
                    .sessions
                    .save_workstream_correction(
                        &project_id,
                        first.revision,
                        WorkstreamChange::Rename {
                            id: &target,
                            name: "保留的人工名称",
                        },
                    )
                    .unwrap();
                service
                    .sessions
                    .save_workstream_correction(
                        &project_id,
                        revision,
                        WorkstreamChange::Move {
                            thread_id: "x",
                            target_id: Some(&target),
                        },
                    )
                    .unwrap();
            })
            .unwrap();
        assert_eq!(automatic.workstreams.len(), 1);
        assert_eq!(automatic.workstreams[0].id, target);
        assert!(automatic.workstreams[0].members.is_empty());
        let view = service.project_workstreams(&project_id).unwrap();
        let retained = view
            .workstreams
            .iter()
            .find(|stream| stream.id == target)
            .unwrap();
        assert_eq!(retained.name, "保留的人工名称");
        assert_eq!(retained.members, ["x"]);
        assert_eq!(view.ungrouped_thread_ids, ["a", "b"]);
        assert_eq!(view.revision, first.revision + 3);
        let _ = fs::remove_dir_all(root);
    }
}
