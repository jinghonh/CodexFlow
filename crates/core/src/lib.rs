mod analysis_batch;
mod candidates;
mod facts;
mod inferred;
mod projects;
mod relations;
mod summary;
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
    normalize_base_url, system_credentials, Credential, CredentialStore, JevClient,
};
use codexflow_store::{EvidenceSourceSnapshot, PreferenceStore, SessionStore};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
pub use summary::SummaryAnalyzer;
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
    // Invalidates ownership computed from stale source and project snapshots.
    reconciliation_revision: AtomicU64,
    jev_gate: RwLock<()>,
    jev_cancel: Mutex<CancellationToken>,
    refresh_active: std::sync::Mutex<Option<(String, CancellationToken)>>,
    summary_active:
        std::sync::Mutex<std::collections::HashMap<String, (String, CancellationToken)>>,
    analysis_active:
        std::sync::Mutex<std::collections::HashMap<String, analysis_batch::AnalysisControl>>,
    analysis_update_lock: std::sync::Mutex<()>,
    analysis_clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    model_slots: Arc<tokio::sync::Semaphore>,
    analysis_auth_home: Option<PathBuf>,
    binary_fingerprint_cache: std::sync::Mutex<Option<(String, u64, SystemTime, String)>>,
}

struct State {
    status: SourceStatus,
    session: Option<Session>,
}

impl SourceService {
    fn binary_fingerprint(&self, path: &str) -> Option<String> {
        let before = std::fs::metadata(path).ok()?;
        let modified = before.modified().ok()?;
        let mut cached = self.binary_fingerprint_cache.lock().unwrap();
        if let Some((saved_path, size, saved_modified, digest)) = cached.as_ref() {
            if saved_path == path && *size == before.len() && *saved_modified == modified {
                return Some(digest.clone());
            }
        }
        let mut file = std::fs::File::open(path).ok()?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let after = std::fs::metadata(path).ok()?;
        if after.len() != before.len() || after.modified().ok()? != modified {
            return None;
        }
        let digest = format!("{:x}", hasher.finalize());
        *cached = Some((path.to_owned(), before.len(), modified, digest.clone()));
        Some(digest)
    }

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
            reconciliation_revision: AtomicU64::new(0),
            jev_gate: RwLock::new(()),
            jev_cancel: Mutex::new(CancellationToken::new()),
            refresh_active: std::sync::Mutex::new(None),
            summary_active: std::sync::Mutex::new(std::collections::HashMap::new()),
            analysis_active: std::sync::Mutex::new(std::collections::HashMap::new()),
            analysis_update_lock: std::sync::Mutex::new(()),
            analysis_clock: Arc::new(|| now_ms() as i64),
            model_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            analysis_auth_home: None,
            binary_fingerprint_cache: std::sync::Mutex::new(None),
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
        next.jev_revision = next.jev_revision.saturating_add(1);
        self.store.save(&next)?;
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
        if self.sessions.thread(thread_id)?.is_none() {
            return Err(AppError::codex(
                ErrorCode::SourceReadFailed,
                "会话未在本地索引中。",
                false,
            ));
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
            return self.sessions.mark_fact_index_current(thread_id, generation);
        }
        let (facts, evidence) = facts::extract(&items);
        self.sessions.replace_automatic_facts(
            thread_id,
            &digest,
            facts::RULE_VERSION,
            generation,
            &facts,
            &evidence,
        )
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
            let current = if let Some(candidate) = candidate {
                inferred::candidate_version(self, candidate)? == result.input_version
            } else {
                false
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
                    current
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
                    review,
                };
                if !reviewed
                    .get(&entry.relation.id)
                    .is_some_and(|old: &ReviewedInferredRelation| old.evidence_valid)
                {
                    reviewed.insert(entry.relation.id.clone(), entry);
                }
            }
            if current {
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
        let previous = self.sessions.workstreams(project_id)?;
        let result = workstreams::build(&graph, &previous);
        if result.workstreams != previous {
            self.sessions
                .replace_workstreams(project_id, &result.workstreams)?;
        }
        Ok(result)
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
        for item in &sessions.threads {
            self.ensure_facts(&item.thread.id)?;
        }
        let material = self.sessions.project_material(&sessions.project.id)?;
        let result = candidates::build(sessions, material);
        self.sessions
            .replace_automatic_candidates(&result.0, &result.1)?;
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
}
