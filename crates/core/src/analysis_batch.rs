use super::summary::SummaryBatchSnapshot;
use super::{now_ms, SourceService};
use codexflow_domain::{
    AnalysisLimits, AnalysisPreview, AnalysisRun, AnalysisRunState, AnalysisStage,
    AnalysisStagePlan, AnalysisUnit, AnalysisUnitState, AppError, CapabilityState, ErrorCode,
    SummaryRun, SummaryRunState,
};
use sha2::{Digest, Sha256};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

static NEXT_BATCH: AtomicU64 = AtomicU64::new(0);

type Update = Arc<dyn Fn(AnalysisRun) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct AnalysisControl {
    pub id: String,
    pub project_id: String,
    pub cancel: CancellationToken,
    pub pause: Arc<AtomicBool>,
    queue_pause: CancellationToken,
    dispatch: Arc<tokio::sync::Mutex<()>>,
    update: Update,
}

fn core_error(code: ErrorCode, message: &str, retryable: bool) -> AppError {
    AppError {
        code,
        message: message.into(),
        retryable,
        cache_preserved: true,
        backend: "core".into(),
        retry_after_ms: None,
    }
}

fn validate_limits(limits: &AnalysisLimits) -> Result<(), AppError> {
    if limits.call_limit == 0
        || !(1..=2).contains(&limits.concurrency_limit)
        || limits.timeout_seconds == 0
        || limits.retry_limit > 2
        || limits.input_character_limit < 2_000
    {
        return Err(core_error(ErrorCode::AnalysisBudgetInvalid,
            "调用上限和超时必须为正整数；全部后端并发最多 2，自动重试最多 2 次，输入至少 2000 字符。", false));
    }
    Ok(())
}

fn recalculate(run: &mut AnalysisRun) {
    run.succeeded = run
        .units
        .iter()
        .filter(|unit| unit.state == AnalysisUnitState::Succeeded)
        .count() as u32;
    run.failed = run
        .units
        .iter()
        .filter(|unit| unit.state == AnalysisUnitState::Failed)
        .count() as u32;
    run.pending = run
        .units
        .iter()
        .filter(|unit| {
            matches!(
                unit.state,
                AnalysisUnitState::Pending | AnalysisUnitState::Running
            )
        })
        .count() as u32;
    run.processed = run.succeeded + run.failed;
}

/// One reservation is one Codex turn or one Jev POST, regardless of stage.
/// A Jev POST can carry multiple questions; callers pass that count separately.
pub(crate) fn reserve_attempt(
    run: &mut AnalysisRun,
    index: usize,
    questions: u32,
) -> Result<(), AppError> {
    if run.batch_calls >= run.limits.call_limit {
        return Err(core_error(
            ErrorCode::AnalysisBudgetInvalid,
            "本批调用达到上限。",
            true,
        ));
    }
    run.state = AnalysisRunState::Running;
    run.units[index].state = AnalysisUnitState::Running;
    run.units[index].attempts += 1;
    run.batch_calls += 1;
    run.total_calls += 1;
    run.total_questions = run.total_questions.saturating_add(questions);
    Ok(())
}

impl SourceService {
    fn analysis_now(&self) -> i64 {
        (self.analysis_clock)()
    }
    async fn analysis_material(
        &self,
        project_id: &str,
        limits: AnalysisLimits,
    ) -> Result<(AnalysisPreview, Vec<(String, String)>), AppError> {
        validate_limits(&limits)?;
        let sessions = self.project_sessions(project_id)?;
        let source = self.status().await;
        let jev = self.jev_status().await?;
        let candidate = self.candidate_preview(project_id)?;
        let mut hasher = Sha256::new();
        let mut pending = Vec::new();
        let mut cached = 0;
        let mut unavailable = 0;
        let mut threads = sessions.threads;
        threads.sort_by(|left, right| left.thread.id.cmp(&right.thread.id));
        for item in threads {
            let thread = item.thread;
            let generation = self.sessions.history_generation(&thread.id)?;
            hasher.update(format!(
                "{}:{}:{}\n",
                thread.id, thread.updated_at, generation
            ));
            match self
                .summary_preview_limited(&thread.id, limits.input_character_limit)
                .await
            {
                Ok(preview) if preview.cache_current => cached += 1,
                Ok(preview) if preview.content_available && preview.source_current => {
                    pending.push((thread.id, format!("{}:{}", thread.updated_at, generation)));
                }
                Ok(_) => unavailable += 1,
                Err(_) => unavailable += 1,
            }
        }
        let model = codexflow_codex::configured_summary_model()
            .unwrap_or_else(|| "Codex 默认模型（启动后确认）".into());
        let codex_available =
            matches!(
                source.capabilities.codex_summary.state,
                CapabilityState::Available
            ) && codexflow_codex::analysis_isolation_issue(self.analysis_auth_home.as_deref())
                .is_none();
        let jev_configured = jev.credential_configured && jev.credential_error.is_none();
        let candidate_count = candidate.candidate_count;
        let attempts = u64::from(limits.retry_limit) + 1;
        // Until a pinned-version parser is shared with the Jev adapter, reserve
        // one possible alias probe in the preview's upper bound.
        let alias_probe = u64::from(candidate_count > 0);
        let stages = vec![
            AnalysisStagePlan {
                stage: AnalysisStage::Summary,
                service: "Codex".into(),
                model: model.clone(),
                send_scope: format!(
                    "单条会话的已读取历史、事实与可定位证据；每次最多 {} 字符。",
                    limits.input_character_limit
                ),
                pending_items: pending.len() as u64,
                maximum_calls: (pending.len() as u64).saturating_mul(attempts),
                available: codex_available,
                note: if codex_available {
                    "本批可执行；缓存命中不计调用。"
                } else {
                    "Codex 隔离配置或能力未通过检查，暂不可执行。"
                }
                .into(),
            },
            AnalysisStagePlan {
                stage: AnalysisStage::Relation,
                service: "Jev".into(),
                model: jev.config.model.clone(),
                send_scope: format!(
                    "候选关系材料将发送到配置的 Jev 服务 {}；本阶段尚未接入。",
                    jev.config.base_url
                ),
                pending_items: candidate_count,
                maximum_calls: candidate_count
                    .saturating_mul(attempts)
                    .saturating_add(alias_probe),
                available: false,
                note: if jev_configured {
                    "关系判断尚未接入；别名版本探测及重试也计调用。"
                } else {
                    "Jev 未配置或关系判断尚未接入。"
                }
                .into(),
            },
            AnalysisStagePlan {
                stage: AnalysisStage::EvidenceSelection,
                service: "Jev".into(),
                model: jev.config.model.clone(),
                send_scope: "候选两侧证据将发送到配置的 Jev 服务；本阶段尚未接入。".into(),
                pending_items: candidate_count,
                maximum_calls: candidate_count.saturating_mul(2).saturating_mul(attempts),
                available: false,
                note: "每对候选最多 2 次证据选择 POST；只展示预算上界。".into(),
            },
            AnalysisStagePlan {
                stage: AnalysisStage::Naming,
                service: "Codex".into(),
                model,
                send_scope: "形成工作流后发送分组事实与必要证据；本阶段尚未接入。".into(),
                pending_items: 0,
                maximum_calls: 0,
                available: false,
                note: "尚未形成分组，待命名数量未知；不会在本批调用。".into(),
            },
        ];
        let preview = AnalysisPreview {
            project_id: project_id.into(),
            input_version: format!("{:x}", hasher.finalize()),
            stages,
            cached_summaries: cached,
            unavailable_summaries: unavailable,
            maximum_candidates: candidate_count,
            evidence_selection_call_limit: candidate_count.saturating_mul(2),
            pending_groups: None,
            limits,
            jev_configured,
        };
        Ok((preview, pending))
    }

    pub async fn analysis_preview(
        &self,
        project_id: &str,
        limits: AnalysisLimits,
    ) -> Result<AnalysisPreview, AppError> {
        Ok(self.analysis_material(project_id, limits).await?.0)
    }

    pub fn analysis_run(&self, id: &str) -> Result<Option<AnalysisRun>, AppError> {
        self.sessions.analysis_run(id)
    }

    pub fn latest_analysis_run(&self, project_id: &str) -> Result<Option<AnalysisRun>, AppError> {
        self.sessions.latest_analysis_run(project_id)
    }

    fn save_analysis(&self, run: &mut AnalysisRun, update: &Update) -> Result<(), AppError> {
        let _guard = self.analysis_update_lock.lock().unwrap();
        if self
            .sessions
            .analysis_run(&run.id)?
            .is_some_and(|current| current.state == AnalysisRunState::Cancelling)
            && run.state != AnalysisRunState::Cancelled
        {
            run.state = AnalysisRunState::Cancelling;
        }
        recalculate(run);
        self.sessions.save_analysis_run(run)?;
        update(run.clone());
        Ok(())
    }

    pub async fn start_project_analysis(
        self: &Arc<Self>,
        project_id: String,
        limits: AnalysisLimits,
        on_update: impl Fn(AnalysisRun) + Send + Sync + 'static,
    ) -> Result<AnalysisRun, AppError> {
        let (preview, pending) = self.analysis_material(&project_id, limits.clone()).await?;
        if !preview.stages[0].available {
            return Err(core_error(
                ErrorCode::AnalysisUnavailable,
                "Codex 总结尚不可用；请检查连接能力与隔离配置。",
                false,
            ));
        }
        let source = self.status().await;
        let jev = self.preferences.lock().await.clone();
        for _ in 0..50 {
            if !self
                .analysis_active
                .lock()
                .unwrap()
                .contains_key(&project_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut active = self.analysis_active.lock().unwrap();
        if active.contains_key(&project_id)
            || self
                .sessions
                .latest_analysis_run(&project_id)?
                .is_some_and(|run| run.state == AnalysisRunState::Paused)
        {
            return Err(core_error(
                ErrorCode::AnalysisAlreadyRunning,
                "此项目已有可继续的分析运行，请先继续或取消。",
                true,
            ));
        }
        if self.summary_active.lock().unwrap().keys().any(|thread_id| {
            self.sessions
                .thread_project_id(thread_id)
                .ok()
                .flatten()
                .as_deref()
                == Some(&project_id)
        }) {
            return Err(core_error(
                ErrorCode::AnalysisAlreadyRunning,
                "此项目已有单条会话总结运行，请等待其结束。",
                true,
            ));
        }
        let id = format!(
            "analysis-{}-{}",
            now_ms(),
            NEXT_BATCH.fetch_add(1, Ordering::Relaxed)
        );
        let control = AnalysisControl {
            id: id.clone(),
            project_id: project_id.clone(),
            cancel: CancellationToken::new(),
            pause: Arc::new(AtomicBool::new(false)),
            queue_pause: CancellationToken::new(),
            dispatch: Arc::new(tokio::sync::Mutex::new(())),
            update: Arc::new(on_update),
        };
        let mut run = AnalysisRun {
            id,
            project_id: project_id.clone(),
            state: AnalysisRunState::Queued,
            pause_reason: None,
            input_version: preview.input_version,
            codex_binary: source.resolved_binary,
            codex_version: source.version,
            codex_model: preview.stages[0].model.clone(),
            jev_base_url: jev.jev.base_url,
            jev_model: jev.jev.model,
            jev_config_revision: jev.jev_revision,
            limits,
            batch_number: 1,
            batch_calls: 0,
            total_calls: 0,
            total_questions: 0,
            input_tokens: None,
            output_tokens: None,
            processed: 0,
            succeeded: 0,
            failed: 0,
            pending: pending.len() as u32,
            units: pending
                .into_iter()
                .map(|(id, input_version)| AnalysisUnit {
                    id,
                    stage: AnalysisStage::Summary,
                    input_version,
                    state: AnalysisUnitState::Pending,
                    attempts: 0,
                    active_summary_run_id: None,
                    requested_model: preview.stages[0].model.clone(),
                    actual_model: None,
                    error: None,
                })
                .collect(),
            started_at_unix_ms: self.analysis_now(),
            finished_at_unix_ms: None,
            interrupted: false,
            error: None,
        };
        active.insert(project_id.clone(), control.clone());
        if let Err(error) = self.save_analysis(&mut run, &control.update) {
            active.remove(&project_id);
            return Err(error);
        }
        drop(active);
        let service = Arc::clone(self);
        tokio::spawn(async move {
            service.drive_analysis(control).await;
        });
        Ok(run)
    }

    pub fn pause_analysis_run(&self, id: &str) -> Result<AnalysisRun, AppError> {
        let mut run = self
            .sessions
            .analysis_run(id)?
            .ok_or_else(|| core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false))?;
        let active = self.analysis_active.lock().unwrap();
        if let Some(control) = active
            .get(&run.project_id)
            .filter(|control| control.id == id)
        {
            let _update = self.analysis_update_lock.lock().unwrap();
            run = self.sessions.analysis_run(id)?.ok_or_else(|| {
                core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false)
            })?;
            if matches!(
                run.state,
                AnalysisRunState::Running | AnalysisRunState::Queued
            ) {
                control.pause.store(true, Ordering::SeqCst);
                control.queue_pause.cancel();
                run.pause_reason = Some("正在暂停；等待当前模型调用结束。".into());
                recalculate(&mut run);
                self.sessions.save_analysis_run(&run)?;
                (control.update)(run.clone());
            }
        }
        Ok(run)
    }

    pub async fn cancel_analysis_run(&self, id: &str) -> Result<AnalysisRun, AppError> {
        let mut run = self
            .sessions
            .analysis_run(id)?
            .ok_or_else(|| core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false))?;
        let control = self
            .analysis_active
            .lock()
            .unwrap()
            .get(&run.project_id)
            .filter(|control| control.id == id)
            .cloned();
        if let Some(control) = control {
            let _dispatch = control.dispatch.lock().await;
            run = self.sessions.analysis_run(id)?.ok_or_else(|| {
                core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false)
            })?;
            if matches!(
                run.state,
                AnalysisRunState::Complete
                    | AnalysisRunState::Partial
                    | AnalysisRunState::Failed
                    | AnalysisRunState::Cancelled
            ) {
                return Ok(run);
            }
            if run.state == AnalysisRunState::Paused {
                control.cancel.cancel();
                run.state = AnalysisRunState::Cancelled;
                run.finished_at_unix_ms = Some(self.analysis_now());
                self.save_analysis(&mut run, &control.update)?;
                return Ok(run);
            }
            if let Some(summary_id) = run
                .units
                .iter()
                .find_map(|unit| unit.active_summary_run_id.as_deref())
            {
                self.cancel_summary_run(summary_id)?;
            }
            control.cancel.cancel();
            run.state = AnalysisRunState::Cancelling;
            run.pause_reason = None;
            self.save_analysis(&mut run, &control.update)?;
        } else if run.state == AnalysisRunState::Paused {
            run.state = AnalysisRunState::Cancelled;
            run.finished_at_unix_ms = Some(self.analysis_now());
            self.sessions.save_analysis_run(&run)?;
        }
        Ok(run)
    }

    pub async fn continue_analysis_run(
        self: &Arc<Self>,
        id: &str,
        call_limit: u32,
        on_update: impl Fn(AnalysisRun) + Send + Sync + 'static,
    ) -> Result<AnalysisRun, AppError> {
        let mut run = self
            .sessions
            .analysis_run(id)?
            .ok_or_else(|| core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false))?;
        if !matches!(
            run.state,
            AnalysisRunState::Paused
                | AnalysisRunState::Cancelled
                | AnalysisRunState::Partial
                | AnalysisRunState::Failed
        ) {
            return Err(core_error(
                ErrorCode::AnalysisUnavailable,
                "此运行当前不能继续。",
                false,
            ));
        }
        let mut limits = run.limits.clone();
        limits.call_limit = call_limit;
        validate_limits(&limits)?;
        let (preview, _) = self
            .analysis_material(&run.project_id, limits.clone())
            .await?;
        let source = self.status().await;
        let jev_revision = self.preferences.lock().await.jev_revision;
        if preview.input_version != run.input_version
            || source.resolved_binary != run.codex_binary
            || source.version != run.codex_version
            || preview.stages[0].model != run.codex_model
            || jev_revision != run.jev_config_revision
        {
            return Err(core_error(
                ErrorCode::AnalysisConfigChanged,
                "来源或 Codex 配置已变化；请取消旧运行并启动新批次。",
                false,
            ));
        }
        if !preview.stages[0].available {
            return Err(core_error(
                ErrorCode::AnalysisUnavailable,
                "Codex 总结当前不可用。",
                false,
            ));
        }
        for _ in 0..50 {
            if !self
                .analysis_active
                .lock()
                .unwrap()
                .contains_key(&run.project_id)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut active = self.analysis_active.lock().unwrap();
        if active.contains_key(&run.project_id) {
            return Err(core_error(
                ErrorCode::AnalysisAlreadyRunning,
                "此项目已有执行中的分析。",
                true,
            ));
        }
        let reset_pending_attempts = run.state == AnalysisRunState::Cancelled || run.interrupted;
        for unit in &mut run.units {
            if unit.state == AnalysisUnitState::Failed || unit.state == AnalysisUnitState::Running {
                unit.state = AnalysisUnitState::Pending;
                unit.attempts = 0;
                unit.active_summary_run_id = None;
            } else if reset_pending_attempts && unit.state == AnalysisUnitState::Pending {
                unit.attempts = 0;
            }
        }
        run.state = AnalysisRunState::Queued;
        run.pause_reason = None;
        run.error = None;
        run.interrupted = false;
        run.finished_at_unix_ms = None;
        run.limits = limits;
        run.batch_number += 1;
        run.batch_calls = 0;
        let control = AnalysisControl {
            id: run.id.clone(),
            project_id: run.project_id.clone(),
            cancel: CancellationToken::new(),
            pause: Arc::new(AtomicBool::new(false)),
            queue_pause: CancellationToken::new(),
            dispatch: Arc::new(tokio::sync::Mutex::new(())),
            update: Arc::new(on_update),
        };
        active.insert(run.project_id.clone(), control.clone());
        if let Err(error) = self.save_analysis(&mut run, &control.update) {
            active.remove(&run.project_id);
            return Err(error);
        }
        drop(active);
        let service = Arc::clone(self);
        tokio::spawn(async move {
            service.drive_analysis(control).await;
        });
        Ok(run)
    }

    async fn drive_analysis(self: Arc<Self>, control: AnalysisControl) {
        let result = self.drive_analysis_inner(&control).await;
        if let Err(error) = result {
            control.cancel.cancel();
            if let Ok(Some(mut run)) = self.sessions.analysis_run(&control.id) {
                if let Some(summary_id) = run
                    .units
                    .iter()
                    .find_map(|unit| unit.active_summary_run_id.clone())
                {
                    let _ = self.cancel_summary_run(&summary_id);
                    let _ = self
                        .wait_summary(&summary_id, run.limits.timeout_seconds, &control.cancel)
                        .await;
                    run.units
                        .iter_mut()
                        .filter(|unit| {
                            unit.active_summary_run_id.as_deref() == Some(summary_id.as_str())
                        })
                        .for_each(|unit| {
                            unit.active_summary_run_id = None;
                            unit.state = AnalysisUnitState::Pending;
                        });
                }
                run.state = AnalysisRunState::Failed;
                run.error = Some(error);
                run.finished_at_unix_ms = Some(self.analysis_now());
                let _ = self.save_analysis(&mut run, &control.update);
            }
        }
        if let Ok(Some(mut run)) = self.sessions.analysis_run(&control.id) {
            if run.state == AnalysisRunState::Cancelling {
                if let Some(summary_id) = run
                    .units
                    .iter()
                    .find_map(|unit| unit.active_summary_run_id.clone())
                {
                    let _ = self.cancel_summary_run(&summary_id);
                    let _ = self
                        .wait_summary(&summary_id, run.limits.timeout_seconds, &control.cancel)
                        .await;
                }
                for unit in &mut run.units {
                    if unit.state == AnalysisUnitState::Running {
                        unit.state = AnalysisUnitState::Pending;
                        unit.active_summary_run_id = None;
                    }
                }
                run.state = AnalysisRunState::Cancelled;
                run.finished_at_unix_ms = Some(self.analysis_now());
                let _ = self.save_analysis(&mut run, &control.update);
            }
        }
        self.analysis_active
            .lock()
            .unwrap()
            .remove(&control.project_id);
    }

    async fn drive_analysis_inner(
        self: &Arc<Self>,
        control: &AnalysisControl,
    ) -> Result<(), AppError> {
        loop {
            let mut run = self.sessions.analysis_run(&control.id)?.ok_or_else(|| {
                core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false)
            })?;
            if control.cancel.is_cancelled() || run.state == AnalysisRunState::Cancelling {
                run.state = AnalysisRunState::Cancelled;
                run.finished_at_unix_ms = Some(self.analysis_now());
                for unit in &mut run.units {
                    if unit.state == AnalysisUnitState::Running {
                        unit.state = AnalysisUnitState::Pending;
                        unit.active_summary_run_id = None;
                    }
                }
                self.save_analysis(&mut run, &control.update)?;
                return Ok(());
            }
            if control.pause.load(Ordering::SeqCst) {
                run.state = AnalysisRunState::Paused;
                run.pause_reason = Some("用户暂停；可继续未完成单元。".into());
                run.finished_at_unix_ms = Some(self.analysis_now());
                self.save_analysis(&mut run, &control.update)?;
                return Ok(());
            }
            let Some(index) = run
                .units
                .iter()
                .position(|unit| unit.state == AnalysisUnitState::Pending)
            else {
                run.state = if run.failed == 0 {
                    AnalysisRunState::Complete
                } else if run.succeeded > 0 {
                    AnalysisRunState::Partial
                } else {
                    AnalysisRunState::Failed
                };
                run.finished_at_unix_ms = Some(self.analysis_now());
                self.save_analysis(&mut run, &control.update)?;
                return Ok(());
            };
            if run.batch_calls >= run.limits.call_limit {
                run.state = AnalysisRunState::Paused;
                run.pause_reason = Some("本批调用达到上限；继续将开启新批次。".into());
                run.finished_at_unix_ms = Some(self.analysis_now());
                self.save_analysis(&mut run, &control.update)?;
                return Ok(());
            }
            if run.units[index].stage != AnalysisStage::Summary {
                run.state = AnalysisRunState::Paused;
                run.pause_reason = Some("此阶段尚未接入执行器。".into());
                self.save_analysis(&mut run, &control.update)?;
                return Ok(());
            }
            let source = self.status().await;
            let jev_revision = self.preferences.lock().await.jev_revision;
            let model = codexflow_codex::configured_summary_model()
                .unwrap_or_else(|| "Codex 默认模型（启动后确认）".into());
            if source.resolved_binary != run.codex_binary
                || source.version != run.codex_version
                || model != run.codex_model
                || jev_revision != run.jev_config_revision
            {
                run.state = AnalysisRunState::Paused;
                run.pause_reason = Some("分析配置已变化；旧运行不会混入新配置。".into());
                run.error = Some(core_error(
                    ErrorCode::AnalysisConfigChanged,
                    "请取消旧运行并启动新批次。",
                    false,
                ));
                self.save_analysis(&mut run, &control.update)?;
                return Ok(());
            }
            let thread_id = run.units[index].id.clone();
            let current_generation = self.sessions.history_generation(&thread_id)?;
            let current_thread = self.sessions.thread(&thread_id)?;
            if current_thread
                .as_ref()
                .map(|thread| format!("{}:{}", thread.updated_at, current_generation))
                != Some(run.units[index].input_version.clone())
            {
                run.units[index].state = AnalysisUnitState::Failed;
                run.units[index].error = Some(core_error(
                    ErrorCode::SourceReadFailed,
                    "来源版本已变化；旧批次不分析新内容。",
                    true,
                ));
                self.save_analysis(&mut run, &control.update)?;
                continue;
            }
            let preview = self
                .summary_preview_limited(&thread_id, run.limits.input_character_limit)
                .await;
            match preview {
                Ok(preview) if preview.cache_current => {
                    run.units[index].state = AnalysisUnitState::Succeeded;
                    run.units[index].actual_model =
                        preview.cached_summary.map(|summary| summary.model);
                    self.save_analysis(&mut run, &control.update)?;
                    continue;
                }
                Ok(preview) if preview.content_available && preview.source_current => {}
                Ok(_) | Err(_) => {
                    run.units[index].state = AnalysisUnitState::Failed;
                    run.units[index].error = Some(core_error(
                        ErrorCode::SourceReadFailed,
                        "来源内容不可用或已变化；请重新读取后启动新运行。",
                        true,
                    ));
                    self.save_analysis(&mut run, &control.update)?;
                    continue;
                }
            }
            let dispatch = Arc::clone(&control.dispatch).lock_owned().await;
            if control.cancel.is_cancelled() || control.pause.load(Ordering::SeqCst) {
                continue;
            }
            reserve_attempt(&mut run, index, 0)?;
            self.save_analysis(&mut run, &control.update)?;
            if control.cancel.is_cancelled() || control.pause.load(Ordering::SeqCst) {
                let mut current = self.sessions.analysis_run(&control.id)?.unwrap();
                current.batch_calls -= 1;
                current.total_calls -= 1;
                current.units[index].attempts -= 1;
                current.units[index].state = AnalysisUnitState::Pending;
                self.save_analysis(&mut current, &control.update)?;
                continue;
            }
            let started = self
                .start_summary_for_batch(
                    thread_id,
                    run.limits.input_character_limit,
                    SummaryBatchSnapshot {
                        input_version: &run.units[index].input_version,
                        model: &run.codex_model,
                        binary: run.codex_binary.as_deref(),
                        binary_version: run.codex_version.as_deref(),
                    },
                    run.limits.concurrency_limit,
                    control.queue_pause.clone(),
                )
                .await;
            let summary = match started {
                Ok(summary) => summary,
                Err(error) => {
                    let mut current = self.sessions.analysis_run(&control.id)?.unwrap();
                    current.batch_calls -= 1;
                    current.total_calls -= 1;
                    current.units[index].attempts -= 1;
                    current.units[index].state = if matches!(
                        error.code,
                        ErrorCode::AnalysisConfigChanged
                            | ErrorCode::AnalysisAuthenticationFailed
                            | ErrorCode::AnalysisQuotaExceeded
                    ) {
                        AnalysisUnitState::Pending
                    } else {
                        AnalysisUnitState::Failed
                    };
                    current.units[index].error = Some(error.clone());
                    if !error.retryable {
                        current.state = AnalysisRunState::Paused;
                        current.pause_reason = Some(error.message);
                    }
                    self.save_analysis(&mut current, &control.update)?;
                    if current.state == AnalysisRunState::Paused {
                        return Ok(());
                    }
                    continue;
                }
            };
            if summary.reused_cache {
                let mut current = self.sessions.analysis_run(&control.id)?.unwrap();
                current.batch_calls -= 1;
                current.total_calls -= 1;
                current.units[index].attempts -= 1;
                current.units[index].state = AnalysisUnitState::Succeeded;
                current.units[index].actual_model = Some(summary.model);
                self.save_analysis(&mut current, &control.update)?;
                continue;
            }
            let mut current = self.sessions.analysis_run(&control.id)?.unwrap();
            current.units[index].active_summary_run_id = Some(summary.id.clone());
            self.save_analysis(&mut current, &control.update)?;
            drop(dispatch);
            if control.cancel.is_cancelled() {
                let _ = self.cancel_summary_run(&summary.id);
            }
            let (completed, timed_out) = self
                .wait_summary(&summary.id, run.limits.timeout_seconds, &control.cancel)
                .await?;
            let mut current = self.sessions.analysis_run(&control.id)?.unwrap();
            current.units[index].active_summary_run_id = None;
            if completed.temporary_thread_id.is_none() {
                // The isolated analyzer ended before a model turn could be sent.
                current.batch_calls = current.batch_calls.saturating_sub(1);
                current.total_calls = current.total_calls.saturating_sub(1);
                if control.pause.load(Ordering::SeqCst) || control.cancel.is_cancelled() {
                    // A user-stopped queued call is not an automatic retry attempt.
                    current.units[index].attempts = current.units[index].attempts.saturating_sub(1);
                }
            }
            if completed.state == SummaryRunState::Complete {
                current.units[index].state = AnalysisUnitState::Succeeded;
                current.units[index].actual_model = Some(completed.model);
                current.units[index].error = None;
            } else if control.cancel.is_cancelled()
                || (control.pause.load(Ordering::SeqCst) && completed.temporary_thread_id.is_none())
            {
                current.units[index].state = AnalysisUnitState::Pending;
            } else {
                let error = if timed_out {
                    core_error(
                        ErrorCode::AnalysisTimeout,
                        "模型调用超过本次超时，已等待本地取消终态；可重试。",
                        true,
                    )
                } else {
                    completed.error.unwrap_or_else(|| {
                        core_error(ErrorCode::AnalysisUnavailable, "模型调用未成功。", true)
                    })
                };
                let temporary = matches!(
                    error.code,
                    ErrorCode::ProcessExited
                        | ErrorCode::AnalysisOverloaded
                        | ErrorCode::JevRateLimited
                        | ErrorCode::JevOverloaded
                        | ErrorCode::JevTimeout
                ) || (matches!(
                    error.code,
                    ErrorCode::AnalysisTimeout
                        | ErrorCode::AnalysisUnavailable
                        | ErrorCode::JevConnectionFailed
                ) && error.retryable);
                current.units[index].error = Some(error.clone());
                if matches!(
                    error.code,
                    ErrorCode::JevQuotaExceeded
                        | ErrorCode::JevAuthenticationFailed
                        | ErrorCode::JevNotConfigured
                        | ErrorCode::AnalysisModelUnsupported
                        | ErrorCode::AnalysisAuthenticationFailed
                        | ErrorCode::AnalysisQuotaExceeded
                ) {
                    current.units[index].state = AnalysisUnitState::Pending;
                    current.state = AnalysisRunState::Paused;
                    current.pause_reason = Some(error.message);
                } else if temporary
                    && current.units[index].attempts <= u32::from(run.limits.retry_limit)
                {
                    current.units[index].state = AnalysisUnitState::Pending;
                } else {
                    current.units[index].state = AnalysisUnitState::Failed;
                }
            }
            self.save_analysis(&mut current, &control.update)?;
            if current.state == AnalysisRunState::Paused {
                return Ok(());
            }
            if current.units[index].state == AnalysisUnitState::Pending
                && !control.cancel.is_cancelled()
                && !control.pause.load(Ordering::SeqCst)
            {
                let retry_after_ms = current.units[index]
                    .error
                    .as_ref()
                    .and_then(|error| error.retry_after_ms)
                    .unwrap_or(0);
                let delay_ms =
                    (250 * current.units[index].attempts.min(2) as u64).max(retry_after_ms);
                tokio::select! {
                    _ = control.cancel.cancelled() => {},
                    _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {},
                }
            }
        }
    }

    async fn wait_summary(
        &self,
        id: &str,
        timeout_seconds: u64,
        cancel: &CancellationToken,
    ) -> Result<(SummaryRun, bool), AppError> {
        let start = Instant::now();
        let mut stop_sent = false;
        let mut timed_out = false;
        loop {
            let run = self
                .sessions
                .summary_run(id)?
                .ok_or_else(|| core_error(ErrorCode::AnalysisNotFound, "总结运行丢失。", false))?;
            if !matches!(
                run.state,
                SummaryRunState::Running | SummaryRunState::Cancelling
            ) {
                return Ok((run, timed_out));
            }
            if !stop_sent
                && (cancel.is_cancelled()
                    || start.elapsed() >= Duration::from_secs(timeout_seconds))
            {
                timed_out = !cancel.is_cancelled();
                self.cancel_summary_run(id)?;
                stop_sent = true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::{
        HistoryCoverage, HistoryItem, HistoryReadPath, HistorySnapshot, HistoryTurn, LocalProject,
        ThreadAttribution, ThreadMetadata,
    };
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

    fn root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "codexflow-batch-{name}-{}-{}-{}",
            std::process::id(),
            now_ms(),
            NEXT_BATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn thread(id: &str) -> ThreadMetadata {
        ThreadMetadata {
            id: id.into(),
            session_id: format!("session-{id}"),
            title: Some(id.into()),
            preview: "合成目标".into(),
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
            turns_complete: true,
            items_complete: true,
            missing_from_source: false,
            content_complete: true,
            read_error: None,
            observed_at_unix_ms: 1,
        }
    }

    async fn service(root: &PathBuf, mode: &str, count: usize) -> Arc<SourceService> {
        let binary = root.join(format!("fake-analysis-{mode}.py"));
        fs::write(
            &binary,
            include_bytes!("../../codex/tests/fixtures/fake_codex.py"),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.join("safe-auth-home");
        fs::create_dir_all(&home).unwrap();
        let mut service = SourceService::new(root.join("data")).unwrap();
        service.analysis_auth_home = Some(home);
        service.analysis_clock = Arc::new(|| 12345);
        let service = Arc::new(service);
        service
            .connect(Some(binary.to_string_lossy().into_owned()))
            .await
            .unwrap();
        let threads: Vec<_> = (0..count).map(|i| thread(&format!("thread-{i}"))).collect();
        service.sessions.save_collection(&threads, &[]).unwrap();
        for item in &threads {
            service
                .sessions
                .save_history(&HistorySnapshot {
                    coverage: HistoryCoverage {
                        thread_id: item.id.clone(),
                        source_updated_at: 200,
                        attempted_at_unix_ms: 1,
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
                        thread_id: item.id.clone(),
                        id: "turn-1".into(),
                        ordinal: 0,
                        status: "completed".into(),
                        started_at_unix_ms: Some(100),
                        completed_at_unix_ms: Some(101),
                        duration_ms: Some(1000),
                        time_error: None,
                        source_updated_at: 200,
                        content_version: "turn-v1".into(),
                    }],
                    items: vec![HistoryItem {
                        thread_id: item.id.clone(),
                        turn_id: "turn-1".into(),
                        id: "item-1".into(),
                        ordinal: 0,
                        source_type: "userMessage".into(),
                        supported: true,
                        text: Some("请实现合成目标".into()),
                        command: None,
                        cwd: None,
                        output: None,
                        exit_code: None,
                        status: None,
                        changes: vec![],
                        source_updated_at: 200,
                        content_version: "item-v1".into(),
                    }],
                })
                .unwrap();
        }
        service
            .sessions
            .save_projects_and_attributions(
                &[LocalProject {
                    id: "project-test".into(),
                    name: "合成项目".into(),
                    root: root.to_string_lossy().into_owned(),
                    git_common_dir: None,
                }],
                &threads
                    .iter()
                    .map(|item| ThreadAttribution {
                        thread_id: item.id.clone(),
                        project_id: Some("project-test".into()),
                        workspace_root: None,
                        basis: "test".into(),
                        detail: "测试归属".into(),
                        diagnostic: None,
                        source_project_id: None,
                    })
                    .collect::<Vec<_>>(),
                None,
            )
            .unwrap();
        service
    }

    async fn wait_state(service: &SourceService, id: &str, state: AnalysisRunState) -> AnalysisRun {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let run = service.analysis_run(id).unwrap().unwrap();
                if run.state == state {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn call_limit_pauses_and_continuation_only_handles_remaining_units() {
        let root = root("budget");
        let service = service(&root, "ok", 3).await;
        let preview = service
            .analysis_preview("project-test", AnalysisLimits::default())
            .await
            .unwrap();
        assert_eq!(preview.stages[0].pending_items, 3);
        assert_eq!(preview.stages[1].available, false);
        let limits = AnalysisLimits {
            call_limit: 1,
            ..AnalysisLimits::default()
        };
        let started = service
            .start_project_analysis("project-test".into(), limits, |_| {})
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(
            (paused.started_at_unix_ms, paused.finished_at_unix_ms),
            (12345, Some(12345))
        );
        assert_eq!(
            (
                paused.succeeded,
                paused.pending,
                paused.batch_calls,
                paused.total_calls
            ),
            (1, 2, 1, 1)
        );
        let continued = service
            .continue_analysis_run(&started.id, 2, |_| {})
            .await
            .unwrap();
        assert_eq!(continued.batch_number, 2);
        let complete = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!(
            (
                complete.succeeded,
                complete.pending,
                complete.batch_calls,
                complete.total_calls
            ),
            (3, 0, 2, 3)
        );
        assert!(complete
            .units
            .iter()
            .all(|unit| unit.actual_model.as_deref() == Some("test-model")));
        let cached = service
            .analysis_preview("project-test", AnalysisLimits::default())
            .await
            .unwrap();
        assert_eq!(
            (cached.cached_summaries, cached.stages[0].pending_items),
            (3, 0)
        );
        let reopened = SourceService::new(root.join("data")).unwrap();
        assert_eq!(
            reopened
                .latest_analysis_run("project-test")
                .unwrap()
                .unwrap()
                .state,
            AnalysisRunState::Complete
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn interrupted_run_recovers_and_continues_without_redoing_saved_summary() {
        let root = root("restart");
        let service = service(&root, "ok", 3).await;
        let started = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    call_limit: 1,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let mut paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let active = service
                    .analysis_active
                    .lock()
                    .unwrap()
                    .contains_key("project-test");
                if !active {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        paused.state = AnalysisRunState::Running;
        paused.units[1].state = AnalysisUnitState::Running;
        paused.units[1].active_summary_run_id = Some("orphaned-summary".into());
        service.sessions.save_analysis_run(&paused).unwrap();
        drop(service);
        let mut reopened = SourceService::new(root.join("data")).unwrap();
        reopened.analysis_auth_home = Some(root.join("safe-auth-home"));
        let reopened = Arc::new(reopened);
        let recovered = reopened.analysis_run(&started.id).unwrap().unwrap();
        assert_eq!(recovered.state, AnalysisRunState::Paused);
        assert!(recovered.interrupted);
        assert_eq!((recovered.succeeded, recovered.pending), (1, 2));
        assert_eq!(recovered.units[1].state, AnalysisUnitState::Pending);
        let binary = root.join("fake-analysis-ok.py");
        reopened
            .connect(Some(binary.to_string_lossy().into_owned()))
            .await
            .unwrap();
        reopened
            .continue_analysis_run(&started.id, 2, |_| {})
            .await
            .unwrap();
        let complete = wait_state(&reopened, &started.id, AnalysisRunState::Complete).await;
        assert_eq!((complete.succeeded, complete.total_calls), (3, 3));
        assert_eq!(complete.batch_number, 2);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cancellation_waits_for_codex_terminal_and_can_continue() {
        let root = root("late");
        let service = service(&root, "once-late", 1).await;
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run = service.analysis_run(&started.id).unwrap().unwrap();
                if run.units[0]
                    .active_summary_run_id
                    .as_ref()
                    .is_some_and(|id| service.summary_run(id).unwrap().unwrap().turn_id.is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            service
                .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
                .await,
            Err(AppError {
                code: ErrorCode::AnalysisAlreadyRunning,
                ..
            })
        ));
        assert!(matches!(
            service.start_thread_summary("thread-0".into()).await,
            Err(AppError {
                code: ErrorCode::AnalysisAlreadyRunning,
                ..
            })
        ));
        assert_eq!(
            service
                .project_sessions("project-test")
                .unwrap()
                .threads
                .len(),
            1
        );
        assert_eq!(
            service
                .cancel_analysis_run(&started.id)
                .await
                .unwrap()
                .state,
            AnalysisRunState::Cancelling
        );
        let cancelled = wait_state(&service, &started.id, AnalysisRunState::Cancelled).await;
        assert_eq!(cancelled.pending, 1);
        assert!(service.sessions.summary("thread-0").unwrap().is_none());
        assert!(
            service
                .project_sessions("project-test")
                .unwrap()
                .threads
                .len()
                == 1
        );
        let resumed = service
            .continue_analysis_run(&started.id, 1, |_| {})
            .await
            .unwrap();
        assert_eq!(resumed.batch_number, 2);
        let complete = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!((complete.succeeded, complete.total_calls), (1, 2));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cancelling_before_a_model_turn_refunds_the_reserved_ticket() {
        let root = root("queued-cancel");
        let service = service(&root, "ok", 1).await;
        let held = Arc::clone(&service.model_slots)
            .acquire_many_owned(2)
            .await
            .unwrap();
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run = service.analysis_run(&started.id).unwrap().unwrap();
                if run.units[0].active_summary_run_id.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        service.cancel_analysis_run(&started.id).await.unwrap();
        let cancelled = wait_state(&service, &started.id, AnalysisRunState::Cancelled).await;
        assert_eq!(
            (
                cancelled.total_calls,
                cancelled.batch_calls,
                cancelled.pending
            ),
            (0, 0, 1)
        );
        assert_eq!(cancelled.units[0].attempts, 0);
        assert!(service.sessions.summary("thread-0").unwrap().is_none());
        drop(held);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn single_call_batches_in_two_projects_do_not_deadlock_the_shared_gate() {
        let root = root("two-projects");
        let service = service(&root, "slow-success", 2).await;
        service
            .sessions
            .save_projects_and_attributions(
                &[LocalProject {
                    id: "project-other".into(),
                    name: "另一合成项目".into(),
                    root: root.to_string_lossy().into_owned(),
                    git_common_dir: None,
                }],
                &[ThreadAttribution {
                    thread_id: "thread-1".into(),
                    project_id: Some("project-other".into()),
                    workspace_root: None,
                    basis: "test".into(),
                    detail: "测试归属".into(),
                    diagnostic: None,
                    source_project_id: None,
                }],
                None,
            )
            .unwrap();
        let limits = AnalysisLimits {
            call_limit: 1,
            concurrency_limit: 1,
            ..AnalysisLimits::default()
        };
        let held = Arc::clone(&service.model_slots)
            .acquire_many_owned(2)
            .await
            .unwrap();
        let first = service
            .start_project_analysis("project-test".into(), limits.clone(), |_| {})
            .await
            .unwrap();
        let second = service
            .start_project_analysis("project-other".into(), limits, |_| {})
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        drop(held);
        let first = wait_state(&service, &first.id, AnalysisRunState::Complete).await;
        let second = wait_state(&service, &second.id, AnalysisRunState::Complete).await;
        assert_eq!((first.total_calls, second.total_calls), (1, 1));
        assert_eq!((first.succeeded, second.succeeded), (1, 1));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn pause_drops_a_queued_summary_without_spending_a_ticket() {
        let root = root("queued-pause");
        let service = service(&root, "ok", 1).await;
        let held = Arc::clone(&service.model_slots)
            .acquire_many_owned(2)
            .await
            .unwrap();
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run = service.analysis_run(&started.id).unwrap().unwrap();
                if run.units[0].active_summary_run_id.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        service.pause_analysis_run(&started.id).unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(
            (paused.total_calls, paused.batch_calls, paused.pending),
            (0, 0, 1)
        );
        assert_eq!(paused.units[0].attempts, 0);
        drop(held);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(service.sessions.summary("thread-0").unwrap().is_none());
        service
            .continue_analysis_run(&started.id, 1, |_| {})
            .await
            .unwrap();
        let complete = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!((complete.total_calls, complete.succeeded), (1, 1));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn queued_pause_does_not_consume_retries_in_the_next_batch() {
        let root = root("pause-retry");
        let service = service(&root, "twice-flaky", 1).await;
        let held = Arc::clone(&service.model_slots)
            .acquire_many_owned(2)
            .await
            .unwrap();
        let limits = AnalysisLimits {
            call_limit: 3,
            retry_limit: 2,
            ..AnalysisLimits::default()
        };
        let started = service
            .start_project_analysis("project-test".into(), limits, |_| {})
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run = service.analysis_run(&started.id).unwrap().unwrap();
                if run.units[0].active_summary_run_id.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        service.pause_analysis_run(&started.id).unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!((paused.total_calls, paused.units[0].attempts), (0, 0));
        drop(held);
        service
            .continue_analysis_run(&started.id, 3, |_| {})
            .await
            .unwrap();
        let complete = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!(
            (
                complete.total_calls,
                complete.units[0].attempts,
                complete.succeeded
            ),
            (3, 3, 1)
        );
        assert_eq!(
            std::fs::read_to_string(root.join("failure-count")).unwrap(),
            "2"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn transient_failure_retries_with_a_new_ticket() {
        let root = root("retry");
        let service = service(&root, "flaky", 1).await;
        let started = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    call_limit: 3,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let complete = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!(
            (
                complete.total_calls,
                complete.units[0].attempts,
                complete.succeeded
            ),
            (2, 2, 1)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn manual_pause_waits_for_current_unit_and_keeps_the_queue() {
        let root = root("manual-pause");
        let service = service(&root, "slow-success", 2).await;
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run = service.analysis_run(&started.id).unwrap().unwrap();
                if run
                    .units
                    .iter()
                    .any(|unit| unit.active_summary_run_id.is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pausing = service.pause_analysis_run(&started.id).unwrap();
        assert!(pausing
            .pause_reason
            .as_deref()
            .unwrap()
            .contains("正在暂停"));
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(
            (paused.succeeded, paused.pending, paused.total_calls),
            (1, 1, 1)
        );
        service
            .continue_analysis_run(&started.id, 2, |_| {})
            .await
            .unwrap();
        let complete = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!((complete.succeeded, complete.total_calls), (2, 2));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn changed_jev_settings_cannot_be_mixed_into_a_frozen_run() {
        let root = root("settings");
        let service = service(&root, "ok", 2).await;
        let started = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    call_limit: 1,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(paused.succeeded, 1);
        service
            .save_jev(
                "https://api.typesafe.ai".into(),
                "jev-fixed-test".into(),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            service.continue_analysis_run(&started.id, 1, |_| {}).await,
            Err(AppError {
                code: ErrorCode::AnalysisConfigChanged,
                ..
            })
        ));
        assert!(service
            .sessions
            .summary(&paused.units[0].id)
            .unwrap()
            .is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn explicit_codex_quota_pauses_without_blind_retry() {
        let root = root("quota");
        let service = service(&root, "quota", 1).await;
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!((paused.total_calls, paused.pending), (1, 1));
        assert!(paused.pause_reason.as_deref().unwrap().contains("额度"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn internal_codex_retry_is_interrupted_before_an_uncounted_result() {
        let root = root("internal-retry");
        let service = service(&root, "internal-retry", 1).await;
        let started = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    call_limit: 1,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!((paused.total_calls, paused.pending), (1, 1));
        assert!(root.join("retry-interrupted").exists());
        assert!(service.sessions.summary("thread-0").unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn limits_reject_zero_and_more_than_two_shared_calls() {
        assert!(validate_limits(&AnalysisLimits {
            call_limit: 0,
            ..AnalysisLimits::default()
        })
        .is_err());
        assert!(validate_limits(&AnalysisLimits {
            concurrency_limit: 3,
            ..AnalysisLimits::default()
        })
        .is_err());
        assert!(validate_limits(&AnalysisLimits {
            retry_limit: 3,
            ..AnalysisLimits::default()
        })
        .is_err());
    }
}
