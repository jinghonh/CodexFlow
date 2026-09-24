use super::summary::SummaryBatchSnapshot;
use super::{inferred, now_ms, SourceService};
use codexflow_codex::{analyze_workstream_name, AnalysisEvent};
use codexflow_domain::{
    AnalysisLimits, AnalysisPreview, AnalysisRun, AnalysisRunState, AnalysisStage,
    AnalysisStagePlan, AnalysisUnit, AnalysisUnitState, AppError, CapabilityState, ErrorCode,
    InferredPairOutcome, JevDecisionIdentity, RelationJudgment, SummaryRun, SummaryRunState,
    Workstream,
};
use codexflow_jev::{JevClient, JevRelationAnalyzer, RELATION_RULES_VERSION};
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

enum JevUnitResult {
    Classification(codexflow_domain::JevRelationClassification),
    Evidence(codexflow_domain::JevEvidenceSelection),
}

fn pinned_jev_model(model: &str) -> bool {
    model.strip_prefix("jev-").is_some_and(|version| {
        let (base, suffix) = version
            .split_once('-')
            .map_or((version, None), |(base, suffix)| (base, Some(suffix)));
        let parts: Vec<_> = base.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            && suffix.is_none_or(|suffix| {
                !suffix.is_empty()
                    && suffix.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    })
            })
    })
}

fn reusable_jev_outcome(
    result: &InferredPairOutcome,
    candidate_id: &str,
    input_version: &str,
    base_url: &str,
    requested_model: &str,
    input_character_limit: usize,
) -> bool {
    result.candidate_id == candidate_id
        && result.input_version == input_version
        && pinned_jev_model(requested_model)
        && result.jev_identity.as_ref().is_some_and(|identity| {
            identity.base_url == base_url
                && identity.requested_model == requested_model
                && identity.actual_model == requested_model
                && identity.rules_version == RELATION_RULES_VERSION
                && identity.input_character_limit == input_character_limit
        })
}

fn stamp_jev_outcome(result: &mut InferredPairOutcome, run: &AnalysisRun, actual_model: &str) {
    for relation in &mut result.relations {
        relation.requested_model = run.jev_model.clone();
    }
    result.jev_identity = Some(JevDecisionIdentity {
        base_url: run.jev_base_url.clone(),
        requested_model: run.jev_model.clone(),
        actual_model: actual_model.into(),
        rules_version: RELATION_RULES_VERSION.into(),
        input_character_limit: run.limits.input_character_limit,
    });
}

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

fn reserve_jev_probe(run: &mut AnalysisRun) -> Result<(), AppError> {
    if run.batch_calls >= run.limits.call_limit {
        return Err(core_error(
            ErrorCode::AnalysisBudgetInvalid,
            "本批调用达到上限。",
            true,
        ));
    }
    run.state = AnalysisRunState::Running;
    run.jev_probe_attempts += 1;
    run.batch_calls += 1;
    run.total_calls += 1;
    run.total_questions += 1;
    Ok(())
}

impl SourceService {
    fn analysis_now(&self) -> i64 {
        (self.analysis_clock)()
    }
    fn naming_material(
        &self,
        graph: &codexflow_domain::ProjectGraph,
        stream: &Workstream,
        codex_model: &str,
        codex_version: Option<&str>,
        limit: usize,
    ) -> Result<(String, String), AppError> {
        let mut prompt = String::from("请只依据下列已确定的工作流成员与来源事实，给这条工作流起一个简短、具体的中文名称。不得改变成员或推断没有证据的目标。只返回 name 字段。\n");
        prompt.push_str(&format!(
            "成员数：{}；算法：{}。\n",
            stream.members.len(),
            stream.algorithm_version
        ));
        let mut fingerprint = Sha256::new();
        fingerprint.update(stream.algorithm_version.as_bytes());
        fingerprint.update(b"workstream-name-v2");
        fingerprint.update((codex_model.len() as u64).to_be_bytes());
        fingerprint.update(codex_model.as_bytes());
        fingerprint.update([u8::from(codex_version.is_some())]);
        if let Some(version) = codex_version {
            fingerprint.update((version.len() as u64).to_be_bytes());
            fingerprint.update(version.as_bytes());
        }
        fingerprint.update((limit as u64).to_be_bytes());
        for id in &stream.members {
            fingerprint.update(id.as_bytes());
            let title = graph
                .nodes
                .iter()
                .find(|node| node.id == *id)
                .and_then(|node| node.title.as_deref())
                .unwrap_or("无标题");
            let summary = self.sessions.summary(id)?;
            let current = self.sessions.thread(id)?;
            let generation = self.sessions.history_generation(id)?;
            let summary = summary.filter(|summary| {
                current.as_ref().is_some_and(|thread| {
                    summary.source_updated_at == thread.updated_at
                        && summary.history_generation == generation
                })
            });
            let line = serde_json::json!({"threadId":id,"title":title,
                "goal":summary.as_ref().map(|item| item.content.goal.as_str()),
                "activity":summary.as_ref().map(|item| item.content.activity.as_str()),
                "outcome":summary.as_ref().map(|item| item.content.outcome.as_str())})
            .to_string();
            fingerprint.update(line.as_bytes());
            if prompt.chars().count() + line.chars().count() + 2 < limit.saturating_sub(500) {
                prompt.push_str(&line);
                prompt.push('\n');
            }
        }
        for relation in graph
            .relations
            .iter()
            .filter(|item| stream.relation_ids.contains(&item.id))
        {
            let line = format!(
                "结构关系 {} {} {}\n",
                relation.from_thread_id,
                relation.kind.as_str(),
                relation.to_thread_id
            );
            fingerprint.update(line.as_bytes());
            if prompt.chars().count() + line.chars().count() < limit {
                prompt.push_str(&line);
            }
        }
        for relation in graph
            .derived_relations
            .iter()
            .filter(|item| stream.relation_ids.contains(&item.id))
        {
            let line = format!(
                "规则关系 {} {:?} {}：{}\n",
                relation.from_thread_id, relation.kind, relation.to_thread_id, relation.basis
            );
            fingerprint.update(line.as_bytes());
            if prompt.chars().count() + line.chars().count() < limit {
                prompt.push_str(&line);
            }
        }
        for relation in graph
            .reviewed_relations
            .iter()
            .filter(|item| stream.relation_ids.contains(&item.relation.id))
        {
            let line = format!(
                "推断关系 {} {} {}；证据：{} / {}\n",
                relation.relation.from_thread_id,
                relation.relation.kind.as_str(),
                relation.relation.to_thread_id,
                relation.relation.evidence.left.excerpt,
                relation.relation.evidence.right.excerpt
            );
            fingerprint.update(line.as_bytes());
            if prompt.chars().count() + line.chars().count() < limit {
                prompt.push_str(&line);
            }
        }
        Ok((format!("{:x}", fingerprint.finalize()), prompt))
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
        let max_pairs = candidate
            .thread_count
            .saturating_mul(candidate.thread_count.saturating_sub(1))
            / 2;
        let candidate_upper_bound = max_pairs.min(
            candidate
                .thread_count
                .saturating_mul(u64::from(candidate.neighbor_limit))
                / 2,
        );
        let existing = self.sessions.inferred_pair_outcomes(project_id)?;
        let mut pending_candidates = 0;
        for pair in &candidate.candidates {
            let version = inferred::candidate_version(self, pair)?;
            if !existing.iter().any(|result| {
                reusable_jev_outcome(
                    result,
                    &pair.id,
                    &version,
                    &jev.config.base_url,
                    &jev.config.model,
                    limits.input_character_limit,
                )
            }) {
                pending_candidates += 1;
            }
        }
        let attempts = u64::from(limits.retry_limit) + 1;
        let streams = self.project_workstreams(project_id)?;
        let naming_graph = self.project_graph(project_id)?;
        let mut pending_groups = 0;
        for stream in &streams.workstreams {
            let (version, _) = self.naming_material(
                &naming_graph,
                stream,
                &model,
                source.version.as_deref(),
                limits.input_character_limit,
            )?;
            if stream.name_input_version.as_deref() != Some(&version) {
                pending_groups += 1;
            }
        }
        let alias_probe_calls = u64::from(
            jev_configured && candidate_upper_bound > 0 && !pinned_jev_model(&jev.config.model),
        )
        .saturating_mul(attempts);
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
                    "候选关系材料将发送到配置的 Jev 服务 {}。",
                    jev.config.base_url
                ),
                pending_items: pending_candidates,
                maximum_calls: candidate_upper_bound.saturating_mul(attempts).saturating_add(alias_probe_calls),
                available: jev_configured,
                note: if jev_configured {
                    if alias_probe_calls > 0 {
                        "别名先用一次合成 POST 探测并固定本批实际版本；探测和重试计入调用上限。每对候选另有一次分类 POST。"
                    } else {
                        "每对候选一次分类 POST；重试另计。只复用服务、规则和实际版本一致的固定模型结果。"
                    }
                } else {
                    "Jev 未配置，关系阶段暂不可执行。"
                }
                .into(),
            },
            AnalysisStagePlan {
                stage: AnalysisStage::EvidenceSelection,
                service: "Jev".into(),
                model: jev.config.model.clone(),
                send_scope: "最多 20 组候选两侧证据发送到配置的 Jev 服务。".into(),
                pending_items: pending_candidates,
                maximum_calls: candidate_upper_bound.saturating_mul(attempts),
                available: jev_configured,
                note: "每对有支持关系的候选最多一次证据选择 POST；两阶段合计最多两次推理请求。"
                    .into(),
            },
            AnalysisStagePlan {
                stage: AnalysisStage::Naming,
                service: "Codex".into(),
                model,
                send_scope: "发送已确定分组的成员标题、可用总结与内部来源关系；每次受输入字符上限约束。".into(),
                pending_items: pending_groups,
                maximum_calls: (if pending_candidates > 0 { candidate.thread_count / 2 }
                    else { pending_groups }).saturating_mul(attempts),
                available: codex_available,
                note: if codex_available && pending_candidates > 0 {
                    "当前数量只含已有分组；Jev 关系完成后可能增加。每组一次临时 Codex 回合，重试计入共享预算。"
                } else if codex_available { "分组先由关系图确定；每组一次临时 Codex 回合，重试计入共享预算。" }
                    else { "Codex 隔离配置或能力未通过检查，分组仍可浏览。" }.into(),
            },
        ];
        let preview = AnalysisPreview {
            project_id: project_id.into(),
            input_version: format!("{:x}", hasher.finalize()),
            stages,
            cached_summaries: cached,
            unavailable_summaries: unavailable,
            maximum_candidates: candidate_upper_bound,
            evidence_selection_call_limit: candidate_upper_bound,
            pending_groups: (pending_candidates == 0).then_some(pending_groups),
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

    fn save_analysis_outcome(
        &self,
        run: &mut AnalysisRun,
        result: &codexflow_domain::InferredPairOutcome,
        update: &Update,
    ) -> Result<(), AppError> {
        let _guard = self.analysis_update_lock.lock().unwrap();
        if self
            .sessions
            .analysis_run(&run.id)?
            .is_some_and(|current| current.state == AnalysisRunState::Cancelling)
        {
            return Err(core_error(
                ErrorCode::AnalysisCancelled,
                "分析运行已取消，结果已丢弃。",
                false,
            ));
        }
        recalculate(run);
        self.sessions.save_analysis_with_outcome(run, result)?;
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
        if !preview.stages[0].available && !pending.is_empty() {
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
        let units: Vec<AnalysisUnit> = pending
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
                relation_classification: None,
            })
            .collect();
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
            jev_model: jev.jev.model.clone(),
            jev_rules_version: RELATION_RULES_VERSION.into(),
            jev_config_revision: jev.jev_revision,
            jev_pinned_model: if pinned_jev_model(&jev.jev.model) {
                Some(jev.jev.model.clone())
            } else {
                None
            },
            jev_probe_attempts: 0,
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
            pending: units.len() as u32,
            units,
            relations_planned: false,
            names_planned: false,
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
            || run.jev_rules_version != RELATION_RULES_VERSION
        {
            return Err(core_error(
                ErrorCode::AnalysisConfigChanged,
                "来源或 Codex 配置已变化；请取消旧运行并启动新批次。",
                false,
            ));
        }
        if !preview.stages[0].available
            && run.units.iter().any(|unit| {
                unit.stage == AnalysisStage::Summary && unit.state != AnalysisUnitState::Succeeded
            })
        {
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
        if run.units.iter().any(|unit| {
            unit.stage != AnalysisStage::Naming && unit.state != AnalysisUnitState::Succeeded
        }) {
            // Retried summaries and relations can change both the communities and their
            // naming input. Keep already saved names only when the replanned version matches.
            run.units.retain(|unit| unit.stage != AnalysisStage::Naming);
            run.names_planned = false;
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
        run.jev_probe_attempts = 0;
        run.jev_pinned_model = if pinned_jev_model(&run.jev_model) {
            Some(run.jev_model.clone())
        } else {
            None
        };
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
            if !run.relations_planned
                && !run.units.iter().any(|unit| {
                    unit.stage == AnalysisStage::Summary && unit.state == AnalysisUnitState::Pending
                })
            {
                if self.jev_status().await?.credential_configured
                    && self.preferences.lock().await.jev_revision == run.jev_config_revision
                {
                    let preview = self.candidate_preview(&run.project_id)?;
                    let existing = self.sessions.inferred_pair_outcomes(&run.project_id)?;
                    for pair in &preview.candidates {
                        let version = inferred::candidate_version(self, pair)?;
                        if existing.iter().any(|result| {
                            reusable_jev_outcome(
                                result,
                                &pair.id,
                                &version,
                                &run.jev_base_url,
                                &run.jev_model,
                                run.limits.input_character_limit,
                            )
                        }) {
                            continue;
                        }
                        run.units.push(AnalysisUnit {
                            id: pair.id.clone(),
                            stage: AnalysisStage::Relation,
                            input_version: version,
                            state: AnalysisUnitState::Pending,
                            attempts: 0,
                            active_summary_run_id: None,
                            requested_model: run.jev_model.clone(),
                            actual_model: None,
                            error: None,
                            relation_classification: None,
                        });
                    }
                }
                run.relations_planned = true;
                self.save_analysis(&mut run, &control.update)?;
            }
            if run.relations_planned
                && !run.names_planned
                && !run.units.iter().any(|unit| {
                    unit.state == AnalysisUnitState::Pending && unit.stage != AnalysisStage::Naming
                })
            {
                let groups = self.project_workstreams(&run.project_id)?;
                let naming_graph = self.project_graph(&run.project_id)?;
                for stream in groups.workstreams {
                    let (version, _) = self.naming_material(
                        &naming_graph,
                        &stream,
                        &run.codex_model,
                        run.codex_version.as_deref(),
                        run.limits.input_character_limit,
                    )?;
                    if stream.name_input_version.as_deref() == Some(&version) {
                        continue;
                    }
                    run.units.push(AnalysisUnit {
                        id: stream.id,
                        stage: AnalysisStage::Naming,
                        input_version: version,
                        state: AnalysisUnitState::Pending,
                        attempts: 0,
                        active_summary_run_id: None,
                        requested_model: run.codex_model.clone(),
                        actual_model: None,
                        error: None,
                        relation_classification: None,
                    });
                }
                run.names_planned = true;
                self.save_analysis(&mut run, &control.update)?;
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
            if matches!(
                run.units[index].stage,
                AnalysisStage::Relation | AnalysisStage::EvidenceSelection
            ) {
                if run.jev_pinned_model.is_none() {
                    self.probe_jev_alias(control, run).await?;
                    if self
                        .sessions
                        .analysis_run(&control.id)?
                        .is_some_and(|run| run.state == AnalysisRunState::Paused)
                    {
                        return Ok(());
                    }
                    continue;
                }
                self.run_jev_unit(control, index, run).await?;
                if self
                    .sessions
                    .analysis_run(&control.id)?
                    .is_some_and(|run| run.state == AnalysisRunState::Paused)
                {
                    return Ok(());
                }
                continue;
            }
            let source = self.status().await;
            let jev_revision = self.preferences.lock().await.jev_revision;
            let model = codexflow_codex::configured_summary_model()
                .unwrap_or_else(|| "Codex 默认模型（启动后确认）".into());
            if source.resolved_binary != run.codex_binary
                || source.version != run.codex_version
                || model != run.codex_model
                || jev_revision != run.jev_config_revision
                || run.jev_rules_version != RELATION_RULES_VERSION
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
            if run.units[index].stage == AnalysisStage::Naming {
                self.run_naming_unit(control, index, run).await?;
                if self
                    .sessions
                    .analysis_run(&control.id)?
                    .is_some_and(|run| run.state == AnalysisRunState::Paused)
                {
                    return Ok(());
                }
                continue;
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

    async fn run_naming_unit(
        &self,
        control: &AnalysisControl,
        index: usize,
        mut run: AnalysisRun,
    ) -> Result<(), AppError> {
        let stream_id = run.units[index].id.clone();
        let groups = self.project_workstreams(&run.project_id)?;
        let naming_graph = self.project_graph(&run.project_id)?;
        let Some(stream) = groups
            .workstreams
            .into_iter()
            .find(|item| item.id == stream_id)
        else {
            run.units[index].state = AnalysisUnitState::Failed;
            run.units[index].error = Some(core_error(
                ErrorCode::SourceReadFailed,
                "工作流分组已变化，请启动新分析。",
                true,
            ));
            return self.save_analysis(&mut run, &control.update);
        };
        let (version, prompt) = self.naming_material(
            &naming_graph,
            &stream,
            &run.codex_model,
            run.codex_version.as_deref(),
            run.limits.input_character_limit,
        )?;
        if version != run.units[index].input_version {
            run.units[index].state = AnalysisUnitState::Failed;
            run.units[index].error = Some(core_error(
                ErrorCode::SourceReadFailed,
                "命名材料已变化，请启动新分析。",
                true,
            ));
            return self.save_analysis(&mut run, &control.update);
        }
        if stream.name_input_version.as_deref() == Some(&version) {
            run.units[index].state = AnalysisUnitState::Succeeded;
            run.units[index].error = None;
            return self.save_analysis(&mut run, &control.update);
        }
        let permit = tokio::select! {
            _ = control.cancel.cancelled() => return Ok(()),
            _ = control.queue_pause.cancelled() => return Ok(()),
            acquired = self.model_slots.acquire() => acquired.map_err(|_| core_error(
                ErrorCode::AnalysisUnavailable, "模型并发队列已关闭。", true))?,
        };
        let dispatch = control.dispatch.lock().await;
        if control.cancel.is_cancelled() || control.pause.load(Ordering::SeqCst) {
            return Ok(());
        }
        reserve_attempt(&mut run, index, 0)?;
        self.save_analysis(&mut run, &control.update)?;
        drop(dispatch);
        let turn_started = std::sync::atomic::AtomicBool::new(false);
        let call_cancel = control.cancel.child_token();
        let timeout_cancel = call_cancel.clone();
        let timed_out = Arc::new(AtomicBool::new(false));
        let timeout_flag = Arc::clone(&timed_out);
        let timeout_seconds = run.limits.timeout_seconds;
        let timeout = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(timeout_seconds)).await;
            timeout_flag.store(true, Ordering::SeqCst);
            timeout_cancel.cancel();
        });
        let result = analyze_workstream_name(
            run.codex_binary.as_deref(),
            Some(&run.codex_model),
            self.analysis_auth_home.as_deref(),
            prompt,
            call_cancel,
            |event| {
                if matches!(event, AnalysisEvent::Turn(_)) {
                    turn_started.store(true, Ordering::SeqCst);
                }
                Ok(())
            },
        )
        .await;
        timeout.abort();
        let result = if timed_out.load(Ordering::SeqCst) && !control.cancel.is_cancelled() {
            Err(core_error(
                ErrorCode::AnalysisTimeout,
                "工作流命名超过本次超时，已取消临时回合；可重试。",
                true,
            ))
        } else {
            result
        };
        drop(permit);
        let mut current = self
            .sessions
            .analysis_run(&control.id)?
            .ok_or_else(|| core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false))?;
        if !turn_started.load(Ordering::SeqCst) {
            current.batch_calls = current.batch_calls.saturating_sub(1);
            current.total_calls = current.total_calls.saturating_sub(1);
            current.units[index].attempts = current.units[index].attempts.saturating_sub(1);
        }
        if control.cancel.is_cancelled() || current.state == AnalysisRunState::Cancelling {
            current.units[index].state = AnalysisUnitState::Pending;
            return self.save_analysis(&mut current, &control.update);
        }
        let parsed = result.and_then(|output| {
            let value: serde_json::Value = serde_json::from_str(&output.text).map_err(|_| {
                core_error(
                    ErrorCode::AnalysisInvalidResult,
                    "工作流名称格式无效。",
                    true,
                )
            })?;
            let name = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| {
                    (2..=80).contains(&name.chars().count()) && !name.chars().any(char::is_control)
                })
                .ok_or_else(|| {
                    core_error(
                        ErrorCode::AnalysisInvalidResult,
                        "工作流名称必须为 2–80 个可显示字符。",
                        true,
                    )
                })?;
            Ok((name.to_owned(), output.model))
        });
        match parsed {
            Ok((name, model)) => {
                let latest = self.project_workstreams(&run.project_id)?;
                let valid = latest
                    .workstreams
                    .iter()
                    .find(|item| item.id == stream_id)
                    .is_some_and(|item| item.members == stream.members);
                let latest_graph = self.project_graph(&run.project_id)?;
                if valid
                    && self
                        .naming_material(
                            &latest_graph,
                            &stream,
                            &run.codex_model,
                            run.codex_version.as_deref(),
                            run.limits.input_character_limit,
                        )?
                        .0
                        == version
                {
                    current.units[index].state = AnalysisUnitState::Succeeded;
                    current.units[index].actual_model = Some(model);
                    current.units[index].error = None;
                    let saved = {
                        let _guard = self.analysis_update_lock.lock().unwrap();
                        recalculate(&mut current);
                        let saved = !control.cancel.is_cancelled()
                            && self.sessions.save_analysis_with_workstream_name(
                                &current,
                                &stream_id,
                                &stream.members,
                                &name,
                                &version,
                            )?;
                        if saved {
                            (control.update)(current.clone());
                        }
                        saved
                    };
                    if saved {
                        return Ok(());
                    }
                    current.units[index].state = if control.cancel.is_cancelled() {
                        AnalysisUnitState::Pending
                    } else {
                        AnalysisUnitState::Failed
                    };
                    current.units[index].error = Some(core_error(
                        ErrorCode::SourceReadFailed,
                        "命名提交时分组已变化，旧名称未保存。",
                        true,
                    ));
                } else {
                    current.units[index].state = AnalysisUnitState::Failed;
                    current.units[index].error = Some(core_error(
                        ErrorCode::SourceReadFailed,
                        "分组或命名材料已变化，旧名称未保存。",
                        true,
                    ));
                }
            }
            Err(error) => {
                current.units[index].error = Some(error.clone());
                if matches!(
                    error.code,
                    ErrorCode::AnalysisQuotaExceeded | ErrorCode::AnalysisAuthenticationFailed
                ) {
                    current.units[index].state = AnalysisUnitState::Pending;
                    current.state = AnalysisRunState::Paused;
                    current.pause_reason = Some(error.message);
                } else if error.retryable
                    && current.units[index].attempts <= u32::from(run.limits.retry_limit)
                    && turn_started.load(Ordering::SeqCst)
                {
                    current.units[index].state = AnalysisUnitState::Pending;
                } else {
                    current.units[index].state = AnalysisUnitState::Failed;
                    self.sessions.save_workstream_name_error(
                        &run.project_id,
                        &stream_id,
                        &stream.members,
                        &error.message,
                    )?;
                }
            }
        }
        self.save_analysis(&mut current, &control.update)
    }

    async fn probe_jev_alias(
        &self,
        control: &AnalysisControl,
        mut run: AnalysisRun,
    ) -> Result<(), AppError> {
        let credential_cancel = self.jev_cancel.lock().await.clone();
        let permit = tokio::select! {
            _ = control.cancel.cancelled() => return Ok(()),
            _ = control.queue_pause.cancelled() => return Ok(()),
            _ = credential_cancel.cancelled() => return Ok(()),
            value = self.model_slots.acquire() => value.map_err(|_| core_error(ErrorCode::AnalysisUnavailable, "模型并发队列已关闭。", true))?,
        };
        let gate = tokio::select! {
            _ = control.cancel.cancelled() => return Ok(()),
            _ = credential_cancel.cancelled() => return Ok(()),
            value = self.jev_gate.read() => value,
        };
        let (credential, configured_model) = match self.jev_request_settings().await {
            Ok(settings) => settings,
            Err(error) => {
                run.state = AnalysisRunState::Paused;
                run.pause_reason = Some(error.message.clone());
                run.error = Some(error);
                self.save_analysis(&mut run, &control.update)?;
                return Ok(());
            }
        };
        if self.preferences.lock().await.jev_revision != run.jev_config_revision
            || credential.base_url != run.jev_base_url
            || configured_model != run.jev_model
            || run.jev_rules_version != RELATION_RULES_VERSION
        {
            run.state = AnalysisRunState::Paused;
            run.pause_reason = Some("Jev 配置或判断规则已变化；请启动新运行。".into());
            self.save_analysis(&mut run, &control.update)?;
            return Ok(());
        }
        let dispatch = Arc::clone(&control.dispatch).lock_owned().await;
        if control.cancel.is_cancelled() || control.pause.load(Ordering::SeqCst) {
            return Ok(());
        }
        if JevClient::synthetic_request_characters(&configured_model)
            > run.limits.input_character_limit
        {
            run.state = AnalysisRunState::Paused;
            run.pause_reason = Some("Jev 合成探测请求超过本次输入字符上限。".into());
            run.error = Some(AppError::jev(
                ErrorCode::JevInvalidRequest,
                "请提高输入字符上限或缩短模型 ID。",
                false,
            ));
            self.save_analysis(&mut run, &control.update)?;
            return Ok(());
        }
        let client = JevClient::with_timeout(Duration::from_secs(run.limits.timeout_seconds))?;
        reserve_jev_probe(&mut run)?;
        self.save_analysis(&mut run, &control.update)?;
        drop(dispatch);
        let response = tokio::select! {
            _ = control.cancel.cancelled() => Err(core_error(ErrorCode::JevCancelled, "Jev 本地探测已取消。", false)),
            _ = credential_cancel.cancelled() => Err(core_error(ErrorCode::JevCancelled, "Jev 配置变化，探测已取消。", false)),
            value = client.test_inference(&credential, &configured_model) => value,
        };
        drop(permit);
        drop(gate);
        let _dispatch = Arc::clone(&control.dispatch).lock_owned().await;
        let mut current = self
            .sessions
            .analysis_run(&control.id)?
            .ok_or_else(|| core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false))?;
        if control.cancel.is_cancelled() || current.state == AnalysisRunState::Cancelling {
            return Ok(());
        }
        if self.preferences.lock().await.jev_revision != current.jev_config_revision
            || current.jev_rules_version != RELATION_RULES_VERSION
        {
            current.state = AnalysisRunState::Paused;
            current.pause_reason = Some("Jev 配置或判断规则已变化；迟到探测结果已丢弃。".into());
            current.error = Some(core_error(
                ErrorCode::AnalysisConfigChanged,
                "请启动新运行。",
                false,
            ));
            self.save_analysis(&mut current, &control.update)?;
            return Ok(());
        }
        let mut retry_after = None;
        match response {
            Ok(inference) => {
                current.input_tokens = Some(
                    current
                        .input_tokens
                        .unwrap_or(0)
                        .saturating_add(inference.input_tokens),
                );
                current.output_tokens = Some(
                    current
                        .output_tokens
                        .unwrap_or(0)
                        .saturating_add(inference.output_tokens),
                );
                if !pinned_jev_model(&inference.actual_model) {
                    current.state = AnalysisRunState::Paused;
                    current.pause_reason =
                        Some("无法确认 Jev 别名对应的版本；请在设置中填写版本化模型 ID。".into());
                    current.error = Some(core_error(
                        ErrorCode::JevProtocolInvalid,
                        "Jev 合成探测未返回可固定的实际模型版本。",
                        false,
                    ));
                } else {
                    for unit in &mut current.units {
                        if unit.stage == AnalysisStage::EvidenceSelection
                            && unit
                                .relation_classification
                                .as_ref()
                                .is_some_and(|classification| {
                                    classification.actual_model != inference.actual_model
                                })
                        {
                            unit.stage = AnalysisStage::Relation;
                            unit.state = AnalysisUnitState::Pending;
                            unit.attempts = 0;
                            unit.relation_classification = None;
                            unit.actual_model = None;
                            unit.error = None;
                        }
                    }
                    current.jev_pinned_model = Some(inference.actual_model);
                    current.error = None;
                }
            }
            Err(error) => {
                let temporary = matches!(
                    error.code,
                    ErrorCode::JevRateLimited
                        | ErrorCode::JevOverloaded
                        | ErrorCode::JevTimeout
                        | ErrorCode::JevConnectionFailed
                ) && error.retryable;
                if temporary && current.jev_probe_attempts <= u32::from(current.limits.retry_limit)
                {
                    retry_after = Some(error.retry_after_ms.unwrap_or(0));
                    current.error = Some(error);
                } else {
                    current.state = AnalysisRunState::Paused;
                    current.pause_reason =
                        Some(format!("无法确认 Jev 实际模型版本：{}", error.message));
                    current.error = Some(error);
                }
            }
        }
        self.save_analysis(&mut current, &control.update)?;
        if let Some(retry_after) = retry_after {
            let delay = (250 * current.jev_probe_attempts.min(2) as u64).max(retry_after);
            tokio::select! {
                _ = control.cancel.cancelled() => {},
                _ = control.queue_pause.cancelled() => {},
                _ = tokio::time::sleep(Duration::from_millis(delay)) => {},
            }
        }
        Ok(())
    }

    async fn run_jev_unit(
        &self,
        control: &AnalysisControl,
        index: usize,
        mut run: AnalysisRun,
    ) -> Result<(), AppError> {
        let preview = self.candidate_preview(&run.project_id)?;
        let Some(candidate) = preview
            .candidates
            .iter()
            .find(|pair| pair.id == run.units[index].id)
        else {
            run.units[index].state = AnalysisUnitState::Failed;
            run.units[index].error = Some(core_error(
                ErrorCode::SourceReadFailed,
                "候选关系已变化，请启动新运行。",
                true,
            ));
            self.save_analysis(&mut run, &control.update)?;
            return Ok(());
        };
        if inferred::candidate_version(self, candidate)? != run.units[index].input_version {
            run.units[index].state = AnalysisUnitState::Failed;
            run.units[index].error = Some(core_error(
                ErrorCode::SourceReadFailed,
                "候选材料版本已变化，请启动新运行。",
                true,
            ));
            self.save_analysis(&mut run, &control.update)?;
            return Ok(());
        }
        let phase = run.units[index].stage;
        let supported = run.units[index]
            .relation_classification
            .as_ref()
            .map(|classification| {
                classification
                    .choices
                    .iter()
                    .filter(|choice| choice.judgment == RelationJudgment::Supported)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let questions = if phase == AnalysisStage::Relation {
            16
        } else {
            supported.len() as u32
        };
        let credential_cancel = self.jev_cancel.lock().await.clone();
        let permit = tokio::select! {
            _ = control.cancel.cancelled() => return Ok(()),
            _ = control.queue_pause.cancelled() => return Ok(()),
            _ = credential_cancel.cancelled() => return Ok(()),
            value = self.model_slots.acquire() => value.map_err(|_| core_error(ErrorCode::AnalysisUnavailable, "模型并发队列已关闭。", true))?,
        };
        let gate = tokio::select! {
            _ = control.cancel.cancelled() => return Ok(()),
            _ = credential_cancel.cancelled() => return Ok(()),
            value = self.jev_gate.read() => value,
        };
        let (credential, model) = match self.jev_request_settings().await {
            Ok(settings) => settings,
            Err(error) => {
                run.state = AnalysisRunState::Paused;
                run.pause_reason = Some(error.message.clone());
                run.units[index].error = Some(error);
                self.save_analysis(&mut run, &control.update)?;
                return Ok(());
            }
        };
        let revision = self.preferences.lock().await.jev_revision;
        if revision != run.jev_config_revision
            || credential.base_url != run.jev_base_url
            || model != run.jev_model
        {
            run.state = AnalysisRunState::Paused;
            run.pause_reason = Some("Jev 配置已变化；旧运行不会混入新配置。".into());
            run.units[index].error = Some(core_error(
                ErrorCode::AnalysisConfigChanged,
                "请取消旧运行并启动新批次。",
                false,
            ));
            self.save_analysis(&mut run, &control.update)?;
            return Ok(());
        }
        let dispatch = Arc::clone(&control.dispatch).lock_owned().await;
        if control.cancel.is_cancelled() || control.pause.load(Ordering::SeqCst) {
            return Ok(());
        }
        let model = run.jev_pinned_model.clone().ok_or_else(|| {
            core_error(
                ErrorCode::AnalysisUnavailable,
                "Jev 本批实际模型尚未固定。",
                false,
            )
        })?;
        let analyzer = JevRelationAnalyzer::with_timeout_and_material_limit(
            Duration::from_secs(run.limits.timeout_seconds),
            run.limits.input_character_limit,
        )?;
        let preflight = if phase == AnalysisStage::Relation {
            analyzer.validate_classification(candidate, &model)
        } else {
            analyzer.validate_evidence_selection(candidate, &supported, &model)
        };
        if let Err(error) = preflight {
            run.units[index].state = AnalysisUnitState::Failed;
            run.units[index].error = Some(error);
            self.save_analysis(&mut run, &control.update)?;
            return Ok(());
        }
        run.units[index].requested_model = model.clone();
        reserve_attempt(&mut run, index, questions)?;
        self.save_analysis(&mut run, &control.update)?;
        drop(dispatch);
        let response = if phase == AnalysisStage::Relation {
            tokio::select! {
                _ = control.cancel.cancelled() => Err(core_error(ErrorCode::JevCancelled, "Jev 本地请求已取消。", false)),
                _ = credential_cancel.cancelled() => Err(core_error(ErrorCode::JevCancelled, "Jev 配置变化，请启动新运行。", false)),
                value = analyzer.classify(&credential, &model, candidate) => value.map(JevUnitResult::Classification),
            }
        } else {
            tokio::select! {
                _ = control.cancel.cancelled() => Err(core_error(ErrorCode::JevCancelled, "Jev 本地请求已取消。", false)),
                _ = credential_cancel.cancelled() => Err(core_error(ErrorCode::JevCancelled, "Jev 配置变化，请启动新运行。", false)),
                value = analyzer.select_evidence(&credential, &model, candidate, &supported) => value.map(JevUnitResult::Evidence),
            }
        };
        drop(permit);
        drop(gate);
        let _dispatch = Arc::clone(&control.dispatch).lock_owned().await;
        let mut current = self
            .sessions
            .analysis_run(&control.id)?
            .ok_or_else(|| core_error(ErrorCode::AnalysisNotFound, "找不到分析运行。", false))?;
        if control.cancel.is_cancelled() || current.state == AnalysisRunState::Cancelling {
            current.units[index].state = AnalysisUnitState::Pending;
            self.save_analysis(&mut current, &control.update)?;
            return Ok(());
        }
        let latest = self
            .candidate_preview(&current.project_id)?
            .candidates
            .into_iter()
            .find(|pair| pair.id == candidate.id);
        let latest_version = latest
            .as_ref()
            .map(|pair| inferred::candidate_version(self, pair))
            .transpose()?;
        if self.preferences.lock().await.jev_revision != current.jev_config_revision
            || latest_version.as_deref() != Some(current.units[index].input_version.as_str())
            || current.jev_pinned_model.as_deref() != Some(model.as_str())
            || current.jev_rules_version != RELATION_RULES_VERSION
        {
            current.units[index].state = AnalysisUnitState::Pending;
            current.state = AnalysisRunState::Paused;
            current.pause_reason = Some("Jev 配置或候选来源已变化；迟到结果已丢弃。".into());
            self.save_analysis(&mut current, &control.update)?;
            return Ok(());
        }
        let returned = match &response {
            Ok(JevUnitResult::Classification(value)) => {
                Some((&value.actual_model, value.input_tokens, value.output_tokens))
            }
            Ok(JevUnitResult::Evidence(value)) => {
                Some((&value.actual_model, value.input_tokens, value.output_tokens))
            }
            Err(_) => None,
        };
        if let Some((actual, input_tokens, output_tokens)) = returned {
            if actual != &model {
                current.input_tokens = Some(
                    current
                        .input_tokens
                        .unwrap_or(0)
                        .saturating_add(input_tokens),
                );
                current.output_tokens = Some(
                    current
                        .output_tokens
                        .unwrap_or(0)
                        .saturating_add(output_tokens),
                );
                current.units[index].state = AnalysisUnitState::Pending;
                current.units[index].attempts = 0;
                current.units[index].error = Some(core_error(
                    ErrorCode::AnalysisConfigChanged,
                    "Jev 返回的实际版本与本批固定版本不同；已拒绝该结果。",
                    false,
                ));
                current.state = AnalysisRunState::Paused;
                current.pause_reason = Some("Jev 实际模型版本变化；请启动新批次。".into());
                self.save_analysis(&mut current, &control.update)?;
                return Ok(());
            }
        }
        let mut outcome = None;
        match response {
            Ok(JevUnitResult::Classification(classification)) => {
                current.input_tokens = Some(
                    current
                        .input_tokens
                        .unwrap_or(0)
                        .saturating_add(classification.input_tokens),
                );
                current.output_tokens = Some(
                    current
                        .output_tokens
                        .unwrap_or(0)
                        .saturating_add(classification.output_tokens),
                );
                current.units[index].actual_model = Some(classification.actual_model.clone());
                current.units[index].error = None;
                if classification
                    .choices
                    .iter()
                    .any(|choice| choice.judgment == RelationJudgment::Supported)
                {
                    current.units[index].relation_classification = Some(classification);
                    current.units[index].stage = AnalysisStage::EvidenceSelection;
                    current.units[index].state = AnalysisUnitState::Pending;
                    current.units[index].attempts = 0;
                } else {
                    let mut result = inferred::outcome(
                        self,
                        &current.project_id,
                        candidate,
                        &current.units[index].input_version,
                        &classification,
                        None,
                    )?;
                    stamp_jev_outcome(&mut result, &current, &classification.actual_model);
                    outcome = Some(result);
                    current.units[index].state = AnalysisUnitState::Succeeded;
                }
            }
            Ok(JevUnitResult::Evidence(selection)) => {
                current.input_tokens = Some(
                    current
                        .input_tokens
                        .unwrap_or(0)
                        .saturating_add(selection.input_tokens),
                );
                current.output_tokens = Some(
                    current
                        .output_tokens
                        .unwrap_or(0)
                        .saturating_add(selection.output_tokens),
                );
                if current.units[index]
                    .relation_classification
                    .as_ref()
                    .is_some_and(|classification| {
                        classification.actual_model != selection.actual_model
                    })
                {
                    current.units[index].state = AnalysisUnitState::Pending;
                    current.units[index].error = Some(core_error(
                        ErrorCode::AnalysisConfigChanged,
                        "两次 Jev 请求的实际模型不同；请启动新运行。",
                        false,
                    ));
                    current.state = AnalysisRunState::Paused;
                    current.pause_reason = Some("Jev 模型别名在运行中发生变化。".into());
                    self.save_analysis(&mut current, &control.update)?;
                    return Ok(());
                }
                let classification = current.units[index]
                    .relation_classification
                    .as_ref()
                    .ok_or_else(|| {
                        core_error(ErrorCode::AnalysisInvalidResult, "分类结果丢失。", false)
                    })?;
                let mut result = inferred::outcome(
                    self,
                    &current.project_id,
                    candidate,
                    &current.units[index].input_version,
                    classification,
                    Some(&selection),
                )?;
                stamp_jev_outcome(&mut result, &current, &selection.actual_model);
                outcome = Some(result);
                current.units[index].state = AnalysisUnitState::Succeeded;
                current.units[index].actual_model = Some(selection.actual_model);
                current.units[index].error = None;
            }
            Err(error) => {
                let alias_pin_failure = !pinned_jev_model(&current.jev_model)
                    && matches!(
                        error.code,
                        ErrorCode::JevInvalidRequest | ErrorCode::JevProtocolInvalid
                    );
                let temporary = matches!(
                    error.code,
                    ErrorCode::JevRateLimited
                        | ErrorCode::JevOverloaded
                        | ErrorCode::JevTimeout
                        | ErrorCode::JevConnectionFailed
                ) && error.retryable;
                let pause = matches!(
                    error.code,
                    ErrorCode::JevQuotaExceeded
                        | ErrorCode::JevAuthenticationFailed
                        | ErrorCode::JevNotConfigured
                        | ErrorCode::JevModelUnsupported
                        | ErrorCode::JevCancelled
                        | ErrorCode::AnalysisConfigChanged
                );
                current.units[index].error = Some(error.clone());
                if pause || alias_pin_failure {
                    current.state = AnalysisRunState::Paused;
                    current.pause_reason = Some(if alias_pin_failure {
                        "Jev 固定版本请求被拒绝或响应无效；请检查网关是否支持该版本 ID。".into()
                    } else {
                        error.message
                    });
                    current.units[index].state = AnalysisUnitState::Pending;
                } else if temporary
                    && current.units[index].attempts <= u32::from(current.limits.retry_limit)
                {
                    current.units[index].state = AnalysisUnitState::Pending;
                } else {
                    current.units[index].state = AnalysisUnitState::Failed;
                }
            }
        }
        if let Some(result) = outcome {
            self.save_analysis_outcome(&mut current, &result, &control.update)?;
        } else {
            self.save_analysis(&mut current, &control.update)?;
        }
        if current.units[index].state == AnalysisUnitState::Pending
            && current.state != AnalysisRunState::Paused
            && current.units[index].error.is_some()
            && !control.pause.load(Ordering::SeqCst)
        {
            let delay = (250 * current.units[index].attempts.min(2) as u64).max(
                current.units[index]
                    .error
                    .as_ref()
                    .and_then(|error| error.retry_after_ms)
                    .unwrap_or(0),
            );
            tokio::select! { _ = control.cancel.cancelled() => {}, _ = tokio::time::sleep(Duration::from_millis(delay)) => {} }
        }
        Ok(())
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
    use codexflow_jev::{Credential, CredentialStore};
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::Mutex,
    };

    #[derive(Default)]
    struct TestCredentials(Mutex<Option<Credential>>);
    impl CredentialStore for TestCredentials {
        fn load(&self) -> Result<Option<Credential>, AppError> {
            Ok(self.0.lock().unwrap().as_ref().map(|value| Credential {
                base_url: value.base_url.clone(),
                key: value.key.clone(),
            }))
        }
        fn save(&self, value: &Credential) -> Result<(), AppError> {
            *self.0.lock().unwrap() = Some(Credential {
                base_url: value.base_url.clone(),
                key: value.key.clone(),
            });
            Ok(())
        }
        fn delete(&self) -> Result<(), AppError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    fn read_jev_post(stream: &mut std::net::TcpStream) -> serde_json::Value {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            bytes.extend_from_slice(&buffer[..count]);
            let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let header = String::from_utf8_lossy(&bytes[..split]);
            assert!(header.starts_with("POST /v1/systemone HTTP/1.1"));
            assert!(header
                .to_ascii_lowercase()
                .contains("authorization: bearer synthetic-key"));
            let length: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|value| value.parse().ok())
                })
                .unwrap();
            if bytes.len() >= split + 4 + length {
                return serde_json::from_slice(&bytes[split + 4..split + 4 + length]).unwrap();
            }
        }
    }

    fn write_jev_json(stream: &mut std::net::TcpStream, status: u16, body: serde_json::Value) {
        let body = body.to_string();
        write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    }

    fn synthetic_probe_response(model: &str) -> serde_json::Value {
        serde_json::json!({"model":model,
            "answers":{"classification":{"type":"choice","choice":"resolved","confidence":0.9,
                "probabilities":{"resolved":0.9,"unresolved":0.1}}},
            "usage":{"input_tokens":12,"output_tokens":3}})
    }

    fn rejected_relation_response(request: &serde_json::Value, model: &str) -> serde_json::Value {
        let answers: serde_json::Map<String, serde_json::Value> = request["questions"]
            .as_object()
            .unwrap()
            .keys()
            .map(|key| {
                (
                    key.clone(),
                    serde_json::json!({"type":"choice","choice":"REJECTS",
                "confidence":0.8,"probabilities":{"SUPPORTS":0.1,"REJECTS":0.8,"UNKNOWN":0.1}}),
                )
            })
            .collect();
        serde_json::json!({"model":model,"answers":answers,"usage":{"input_tokens":20,"output_tokens":5}})
    }

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
        let mut service = SourceService::with_credentials(
            root.join("data"),
            Arc::new(TestCredentials::default()),
        )
        .unwrap();
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

    fn save_observed_pair(service: &SourceService) {
        let sessions = service.project_sessions("project-test").unwrap();
        service
            .sessions
            .save_projects_and_attributions(
                &[sessions.project],
                &sessions
                    .threads
                    .into_iter()
                    .map(|item| item.attribution)
                    .collect::<Vec<_>>(),
                Some(&[codexflow_domain::ObservedRelation {
                    id: "observed-pair".into(),
                    project_id: "project-test".into(),
                    from_thread_id: "thread-0".into(),
                    to_thread_id: "thread-1".into(),
                    kind: codexflow_domain::ObservedRelationKind::ForkedFrom,
                    source: "observed".into(),
                    source_field: "forkedFromId".into(),
                    confidence: 1.0,
                    parent_endpoint: codexflow_domain::ParentEndpoint::InProject,
                }]),
            )
            .unwrap();
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
    fn versioned_jev_model_is_distinct_from_a_moving_alias() {
        assert!(pinned_jev_model("jev-1.13.0"));
        assert!(pinned_jev_model("jev-1.14.0-preview.1"));
        for alias in ["jev-latest", "jev-preview", "jev-1.13", "jev-1.13.0-"] {
            assert!(!pinned_jev_model(alias));
        }
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

    #[tokio::test]
    async fn naming_runs_without_jev_when_rule_group_already_exists() {
        let root = root("naming-without-jev");
        let service = service(&root, "ok", 2).await;
        let sessions = service.project_sessions("project-test").unwrap();
        service
            .sessions
            .save_projects_and_attributions(
                &[sessions.project],
                &sessions
                    .threads
                    .into_iter()
                    .map(|item| item.attribution)
                    .collect::<Vec<_>>(),
                Some(&[codexflow_domain::ObservedRelation {
                    id: "observed-pair".into(),
                    project_id: "project-test".into(),
                    from_thread_id: "thread-0".into(),
                    to_thread_id: "thread-1".into(),
                    kind: codexflow_domain::ObservedRelationKind::ForkedFrom,
                    source: "observed".into(),
                    source_field: "forkedFromId".into(),
                    confidence: 1.0,
                    parent_endpoint: codexflow_domain::ParentEndpoint::InProject,
                }]),
            )
            .unwrap();
        let preview = service
            .analysis_preview("project-test", AnalysisLimits::default())
            .await
            .unwrap();
        assert!(!preview.jev_configured);
        assert_eq!(preview.stages[3].pending_items, 1);
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        let completed = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!(completed.total_calls, 3);
        assert_eq!(
            completed
                .units
                .iter()
                .filter(|unit| unit.stage == AnalysisStage::Naming
                    && unit.state == AnalysisUnitState::Succeeded)
                .count(),
            1
        );
        assert_eq!(
            service
                .project_workstreams("project-test")
                .unwrap()
                .workstreams[0]
                .name,
            "测试工作流"
        );
        let changed_limit = service
            .analysis_preview(
                "project-test",
                AnalysisLimits {
                    input_character_limit: 20_000,
                    ..AnalysisLimits::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(changed_limit.stages[3].pending_items, 1);
        let stream = service
            .project_workstreams("project-test")
            .unwrap()
            .workstreams
            .remove(0);
        let graph = service.project_graph("project-test").unwrap();
        let version = |model, binary_version, limit| {
            service
                .naming_material(&graph, &stream, model, Some(binary_version), limit)
                .unwrap()
                .0
        };
        assert_ne!(
            version("model-a", "codex-v1", 40_000),
            version("model-b", "codex-v1", 40_000)
        );
        assert_ne!(
            version("model-a", "codex-v1", 40_000),
            version("model-a", "codex-v2", 40_000)
        );
        assert_ne!(
            version("model-a", "codex-v1", 40_000),
            version("model-a", "codex-v1", 20_000)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn invalid_name_keeps_deterministic_group_and_failure_reason() {
        let root = root("invalid-name");
        let service = service(&root, "invalid", 2).await;
        let sessions = service.project_sessions("project-test").unwrap();
        service
            .sessions
            .save_projects_and_attributions(
                &[sessions.project],
                &sessions
                    .threads
                    .into_iter()
                    .map(|item| item.attribution)
                    .collect::<Vec<_>>(),
                Some(&[codexflow_domain::ObservedRelation {
                    id: "observed-pair".into(),
                    project_id: "project-test".into(),
                    from_thread_id: "thread-0".into(),
                    to_thread_id: "thread-1".into(),
                    kind: codexflow_domain::ObservedRelationKind::ForkedFrom,
                    source: "observed".into(),
                    source_field: "forkedFromId".into(),
                    confidence: 1.0,
                    parent_endpoint: codexflow_domain::ParentEndpoint::InProject,
                }]),
            )
            .unwrap();
        let before = service.project_workstreams("project-test").unwrap();
        assert_eq!(before.workstreams.len(), 1);
        let started = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    retry_limit: 0,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let _ = wait_state(&service, &started.id, AnalysisRunState::Failed).await;
        let after = service.project_workstreams("project-test").unwrap();
        assert_eq!(after.workstreams[0].id, before.workstreams[0].id);
        assert_eq!(after.workstreams[0].name, before.workstreams[0].name);
        assert!(after.workstreams[0].name_error.is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn continuing_failed_summary_replans_name_from_new_material() {
        let root = root("name-replan-on-continue");
        let service = service(&root, "flaky", 2).await;
        save_observed_pair(&service);
        let started = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    retry_limit: 0,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let partial = wait_state(&service, &started.id, AnalysisRunState::Partial).await;
        assert!(partial.names_planned);
        assert_eq!(
            partial
                .units
                .iter()
                .filter(|unit| unit.stage == AnalysisStage::Naming)
                .count(),
            1
        );
        let first_version = service
            .project_workstreams("project-test")
            .unwrap()
            .workstreams[0]
            .name_input_version
            .clone()
            .unwrap();
        let resumed = service
            .continue_analysis_run(&started.id, 100, |_| {})
            .await
            .unwrap();
        assert!(!resumed.names_planned);
        assert!(resumed
            .units
            .iter()
            .all(|unit| unit.stage != AnalysisStage::Naming));
        let complete = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!(
            complete
                .units
                .iter()
                .filter(|unit| unit.stage == AnalysisStage::Naming
                    && unit.state == AnalysisUnitState::Succeeded)
                .count(),
            1
        );
        let final_version = service
            .project_workstreams("project-test")
            .unwrap()
            .workstreams[0]
            .name_input_version
            .clone()
            .unwrap();
        assert_ne!(final_version, first_version);
        assert_eq!(complete.total_calls, partial.total_calls + 2);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn native_jev_two_post_path_persists_a_locatable_inferred_edge() {
        let root = root("jev-two-post");
        let service = service(&root, "ok", 2).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for phase in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 8192];
                let request: serde_json::Value = loop {
                    let count = stream.read(&mut buffer).unwrap();
                    bytes.extend_from_slice(&buffer[..count]);
                    let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                        continue;
                    };
                    let header = String::from_utf8_lossy(&bytes[..split]);
                    assert!(header.starts_with("POST /v1/systemone HTTP/1.1"));
                    assert!(header
                        .to_ascii_lowercase()
                        .contains("authorization: bearer synthetic-key"));
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.parse().ok())
                        })
                        .unwrap();
                    if bytes.len() >= split + 4 + length {
                        break serde_json::from_slice(&bytes[split + 4..split + 4 + length])
                            .unwrap();
                    }
                };
                let answers: serde_json::Map<String, serde_json::Value> = request["questions"].as_object().unwrap().iter().map(|(key, question)| {
                    let options: Vec<&str> = question["criteria"].as_object().unwrap().keys().map(String::as_str).collect();
                    let chosen = if phase == 0 { if key == "fixes_ab" { "SUPPORTS" } else { "REJECTS" } } else { "p0" };
                    let probabilities: serde_json::Map<String, serde_json::Value> = options.iter().map(|option| {
                        ((*option).into(), serde_json::json!(if *option == chosen { 0.8 } else { 0.2 / (options.len()-1) as f64 }))
                    }).collect();
                    (key.clone(), serde_json::json!({"type":"choice","choice":chosen,"confidence":0.76,"probabilities":probabilities}))
                }).collect();
                let body = serde_json::json!({"model":"jev-1.13.0","answers":answers,"usage":{"input_tokens":25,"output_tokens":7}}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                seen.push(request);
            }
            seen
        });
        service
            .save_jev(base_url, "jev-1.13.0".into(), Some("synthetic-key".into()))
            .await
            .unwrap();
        let preview = service
            .analysis_preview("project-test", AnalysisLimits::default())
            .await
            .unwrap();
        assert_eq!(preview.maximum_candidates, 1);
        assert!(preview.stages[1].available);
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
        assert_eq!(started.jev_pinned_model.as_deref(), Some("jev-1.13.0"));
        assert_eq!(started.jev_probe_attempts, 0);
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(paused.total_calls, 3);
        assert!(paused
            .units
            .iter()
            .any(|unit| unit.stage == AnalysisStage::EvidenceSelection
                && unit.relation_classification.is_some()));
        service
            .continue_analysis_run(&started.id, 2, |_| {})
            .await
            .unwrap();
        let completed = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!(completed.total_calls, 5); // two summaries, two Jev POSTs and one name
        let streams = service.project_workstreams("project-test").unwrap();
        assert_eq!(streams.workstreams.len(), 1);
        assert_eq!(streams.workstreams[0].name, "测试工作流");
        assert!(streams.workstreams[0].name_input_version.is_some());
        assert_eq!(completed.total_questions, 17);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["model"], "jev-1.13.0");
        assert_eq!(requests[0]["questions"].as_object().unwrap().len(), 16);
        assert_eq!(requests[1]["questions"].as_object().unwrap().len(), 1);
        let graph = service.project_graph("project-test").unwrap();
        assert_eq!(graph.inferred_relations.len(), 1);
        let relation = &graph.inferred_relations[0];
        assert_eq!(
            (&relation.from_thread_id[..], &relation.to_thread_id[..]),
            ("thread-0", "thread-1")
        );
        assert_eq!(relation.kind.as_str(), "FIXES");
        assert_eq!(relation.actual_model, "jev-1.13.0");
        assert_eq!(relation.confidence, 0.76);
        assert_eq!(relation.evidence.left.thread_id, "thread-0");
        assert_eq!(relation.evidence.right.thread_id, "thread-1");
        assert_eq!(graph.inference_outcomes[0].status, "valid");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn inferred_outcomes_exclude_none_unknown_bad_evidence_and_conflicting_time() {
        use codexflow_domain::{
            InferredRelationKind, JevChoiceAnswer, JevEvidenceChoice, JevEvidenceSelection,
            JevRelationClassification, RelationChoice,
        };
        let root = root("inferred-validation");
        let service = service(&root, "ok", 2).await;
        let candidate = service
            .candidate_preview("project-test")
            .unwrap()
            .candidates
            .remove(0);
        let version = inferred::candidate_version(&service, &candidate).unwrap();
        let answer = |choice: &str, confidence| JevChoiceAnswer {
            choice: choice.into(),
            confidence,
            probabilities: std::collections::BTreeMap::from([
                (
                    "SUPPORTS".into(),
                    if choice == "SUPPORTS" { 0.8 } else { 0.1 },
                ),
                (
                    "REJECTS".into(),
                    if choice == "REJECTS" { 0.8 } else { 0.1 },
                ),
                (
                    "UNKNOWN".into(),
                    if choice == "UNKNOWN" { 0.8 } else { 0.1 },
                ),
            ]),
        };
        let mut classification = JevRelationClassification {
            requested_model: "jev-latest".into(),
            actual_model: "jev-1.13.0".into(),
            choices: vec![RelationChoice {
                key: "fixes_ab".into(),
                kind: InferredRelationKind::Fixes,
                from_thread_id: candidate.left_thread_id.clone(),
                to_thread_id: candidate.right_thread_id.clone(),
                judgment: RelationJudgment::Rejected,
                answer: answer("REJECTS", 0.8),
            }],
            input_tokens: 10,
            output_tokens: 2,
        };
        let none = inferred::outcome(
            &service,
            "project-test",
            &candidate,
            &version,
            &classification,
            None,
        )
        .unwrap();
        assert_eq!(none.status, "none");
        assert!(none.relations.is_empty());
        service.sessions.save_inferred_pair_outcome(&none).unwrap();
        assert!(service
            .project_graph("project-test")
            .unwrap()
            .inferred_relations
            .is_empty());
        classification.choices[0].judgment = RelationJudgment::Unknown;
        classification.choices[0].answer = answer("UNKNOWN", 0.8);
        assert_eq!(
            inferred::outcome(
                &service,
                "project-test",
                &candidate,
                &version,
                &classification,
                None
            )
            .unwrap()
            .status,
            "undetermined"
        );
        classification.choices[0].judgment = RelationJudgment::Supported;
        classification.choices[0].answer = answer("SUPPORTS", 0.55);
        let selection = |pair_id: Option<String>| JevEvidenceSelection {
            actual_model: "jev-1.13.0".into(),
            choices: vec![JevEvidenceChoice {
                relation_key: "fixes_ab".into(),
                pair_id,
                answer: JevChoiceAnswer {
                    choice: "p0".into(),
                    confidence: 0.91,
                    probabilities: std::collections::BTreeMap::from([
                        ("p0".into(), 0.9),
                        ("INSUFFICIENT".into(), 0.1),
                    ]),
                },
            }],
            input_tokens: 10,
            output_tokens: 2,
        };
        assert_eq!(
            inferred::outcome(
                &service,
                "project-test",
                &candidate,
                &version,
                &classification,
                Some(&selection(None))
            )
            .unwrap()
            .status,
            "insufficientEvidence"
        );
        assert_eq!(
            inferred::outcome(
                &service,
                "project-test",
                &candidate,
                &version,
                &classification,
                Some(&selection(Some("outside-candidate".into())))
            )
            .unwrap()
            .status,
            "insufficientEvidence"
        );
        let mut wrong_excerpt = candidate.clone();
        wrong_excerpt.evidence.pairs[0].left.excerpt = "不存在的摘录".into();
        assert_eq!(
            inferred::outcome(
                &service,
                "project-test",
                &wrong_excerpt,
                &version,
                &classification,
                Some(&selection(Some(wrong_excerpt.evidence.pairs[0].id.clone())))
            )
            .unwrap()
            .status,
            "insufficientEvidence"
        );
        let mut wrong_endpoint = classification.clone();
        wrong_endpoint.choices[0].from_thread_id = "outside-project".into();
        assert_eq!(
            inferred::outcome(
                &service,
                "project-test",
                &candidate,
                &version,
                &wrong_endpoint,
                Some(&selection(Some(candidate.evidence.pairs[0].id.clone())))
            )
            .unwrap()
            .status,
            "insufficientEvidence"
        );
        let chosen = selection(Some(candidate.evidence.pairs[0].id.clone()));
        let valid = inferred::outcome(
            &service,
            "project-test",
            &candidate,
            &version,
            &classification,
            Some(&chosen),
        )
        .unwrap();
        assert_eq!(valid.status, "valid");
        assert_eq!(valid.relations[0].confidence, 0.55);
        let mut multiple = classification.clone();
        let mut multiple_selection = chosen.clone();
        for (key, kind) in [
            ("validates_ba", InferredRelationKind::Validates),
            ("alternative_to_ab", InferredRelationKind::AlternativeTo),
        ] {
            multiple.choices.push(RelationChoice {
                key: key.into(),
                kind,
                from_thread_id: candidate.right_thread_id.clone(),
                to_thread_id: candidate.left_thread_id.clone(),
                judgment: RelationJudgment::Supported,
                answer: answer("SUPPORTS", 0.8),
            });
            multiple_selection.choices.push(JevEvidenceChoice {
                relation_key: key.into(),
                pair_id: Some(candidate.evidence.pairs[0].id.clone()),
                answer: chosen.choices[0].answer.clone(),
            });
        }
        let multi = inferred::outcome(
            &service,
            "project-test",
            &candidate,
            &version,
            &multiple,
            Some(&multiple_selection),
        )
        .unwrap();
        assert_eq!(multi.relations.len(), 3);
        assert!(multi
            .relations
            .iter()
            .any(|item| item.kind == InferredRelationKind::Validates
                && item.from_thread_id == candidate.right_thread_id));
        assert!(multi
            .relations
            .iter()
            .any(|item| item.kind == InferredRelationKind::AlternativeTo
                && item.from_thread_id == candidate.left_thread_id));
        assert_eq!(valid.relations[0].evidence_confidence, 0.91);
        service.sessions.save_inferred_pair_outcome(&valid).unwrap();
        let initial = service.project_graph("project-test").unwrap();
        let reviewed = &initial.reviewed_relations[0];
        assert_eq!(reviewed.review.revision, 0);
        let relation_id = reviewed.relation.id.clone();
        let evidence_version = reviewed.evidence_version.clone();
        assert!(matches!(
            service.decide_inferred_relation(
                "project-test",
                "observed-or-manual-id",
                codexflow_domain::UserRelationDecision::Confirmed,
                0,
                Some(&evidence_version)
            ),
            Err(AppError {
                code: ErrorCode::AnalysisInvalidResult,
                ..
            })
        ));
        assert!(matches!(
            service.decide_inferred_relation(
                "project-test",
                &relation_id,
                codexflow_domain::UserRelationDecision::Confirmed,
                0,
                Some("older-evidence")
            ),
            Err(AppError {
                code: ErrorCode::ConcurrentModification,
                ..
            })
        ));
        let confirmed = service
            .decide_inferred_relation(
                "project-test",
                &relation_id,
                codexflow_domain::UserRelationDecision::Confirmed,
                0,
                Some(&evidence_version),
            )
            .unwrap();
        assert_eq!(confirmed.revision, 1);
        assert_eq!(
            confirmed.confirmed_evidence_version.as_deref(),
            Some(evidence_version.as_str())
        );
        assert_eq!(
            service
                .project_graph("project-test")
                .unwrap()
                .inferred_relations
                .len(),
            1
        );
        assert!(matches!(
            service.decide_inferred_relation(
                "project-test",
                &relation_id,
                codexflow_domain::UserRelationDecision::Rejected,
                0,
                None
            ),
            Err(AppError {
                code: ErrorCode::ConcurrentModification,
                ..
            })
        ));
        let mut rerun = valid.clone();
        rerun.relations[0].actual_model = "jev-2.0.0".into();
        rerun.relations[0].requested_model = "jev-other".into();
        service.sessions.save_inferred_pair_outcome(&rerun).unwrap();
        assert_eq!(
            service
                .project_graph("project-test")
                .unwrap()
                .reviewed_relations[0]
                .relation
                .id,
            relation_id
        );
        let rejected = service
            .decide_inferred_relation(
                "project-test",
                &relation_id,
                codexflow_domain::UserRelationDecision::Rejected,
                1,
                None,
            )
            .unwrap();
        assert_eq!(rejected.revision, 2);
        service.sessions.save_inferred_pair_outcome(&valid).unwrap();
        let rejected_graph = service.project_graph("project-test").unwrap();
        assert!(rejected_graph.inferred_relations.is_empty());
        assert_eq!(
            rejected_graph.reviewed_relations[0].review.decision,
            codexflow_domain::UserRelationDecision::Rejected
        );
        service.sessions.save_inferred_pair_outcome(&none).unwrap();
        let absent = service.project_graph("project-test").unwrap();
        assert!(absent.inferred_relations.is_empty());
        assert!(!absent.reviewed_relations[0].evidence_valid);
        assert_eq!(absent.reviewed_relations[0].relation.id, relation_id);
        let reopened_review = SourceService::with_credentials(
            root.join("data"),
            Arc::new(TestCredentials::default()),
        )
        .unwrap();
        assert!(reopened_review
            .project_graph("project-test")
            .unwrap()
            .inferred_relations
            .is_empty());
        drop(reopened_review);
        let restored = service
            .decide_inferred_relation(
                "project-test",
                &relation_id,
                codexflow_domain::UserRelationDecision::Pending,
                2,
                None,
            )
            .unwrap();
        assert_eq!(restored.revision, 3);
        service.sessions.save_inferred_pair_outcome(&valid).unwrap();
        service
            .decide_inferred_relation(
                "project-test",
                &relation_id,
                codexflow_domain::UserRelationDecision::Confirmed,
                3,
                Some(&evidence_version),
            )
            .unwrap();
        assert_eq!(
            service
                .project_graph("project-test")
                .unwrap()
                .inferred_relations
                .len(),
            1
        );
        let reopened = SourceService::with_credentials(
            root.join("data"),
            Arc::new(TestCredentials::default()),
        )
        .unwrap();
        assert_eq!(
            reopened
                .project_graph("project-test")
                .unwrap()
                .inferred_relations
                .len(),
            1
        );
        drop(reopened);
        let connection = rusqlite::Connection::open(root.join("data/sessions.sqlite3")).unwrap();
        let mut later = service
            .sessions
            .history_turn(&candidate.left_thread_id, "turn-1")
            .unwrap()
            .unwrap();
        later.started_at_unix_ms = Some(300);
        later.completed_at_unix_ms = Some(301);
        connection
            .execute(
                "UPDATE history_turns SET turn_json=?1 WHERE thread_id=?2 AND id='turn-1'",
                rusqlite::params![
                    serde_json::to_string(&later).unwrap(),
                    candidate.left_thread_id
                ],
            )
            .unwrap();
        assert_eq!(
            inferred::outcome(
                &service,
                "project-test",
                &candidate,
                &version,
                &classification,
                Some(&chosen)
            )
            .unwrap()
            .status,
            "insufficientEvidence"
        );
        let mut changed = service
            .sessions
            .thread(&candidate.left_thread_id)
            .unwrap()
            .unwrap();
        changed.updated_at += 1;
        service.sessions.save_collection(&[changed], &[]).unwrap();
        assert!(service
            .project_graph("project-test")
            .unwrap()
            .inferred_relations
            .is_empty());
        let stale = service.project_graph("project-test").unwrap();
        assert!(!stale.reviewed_relations[0].evidence_valid);
        assert_eq!(
            stale.reviewed_relations[0].review.decision,
            codexflow_domain::UserRelationDecision::Confirmed
        );
        assert!(matches!(
            service.decide_inferred_relation(
                "project-test",
                &relation_id,
                codexflow_domain::UserRelationDecision::Confirmed,
                4,
                Some(&evidence_version)
            ),
            Err(AppError {
                code: ErrorCode::AnalysisInvalidResult,
                ..
            })
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn in_flight_jev_result_is_rejected_after_cancel_or_config_change() {
        for config_change in [false, true] {
            let root = root(if config_change {
                "jev-config-change"
            } else {
                "jev-cancel"
            });
            let service = service(&root, "ok", 2).await;
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    bytes.extend_from_slice(&buffer[..count]);
                    let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                        continue;
                    };
                    let header = String::from_utf8_lossy(&bytes[..split]);
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|value| value.parse().ok())
                        })
                        .unwrap();
                    if bytes.len() >= split + 4 + length {
                        break;
                    }
                }
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
            });
            service
                .save_jev(
                    base_url.clone(),
                    "jev-latest".into(),
                    Some("synthetic-key".into()),
                )
                .await
                .unwrap();
            let run = service
                .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
                .await
                .unwrap();
            tokio::time::timeout(
                Duration::from_secs(5),
                tokio::task::spawn_blocking(move || started_rx.recv().unwrap()),
            )
            .await
            .unwrap()
            .unwrap();
            if config_change {
                service
                    .save_jev(base_url, "jev-new".into(), None)
                    .await
                    .unwrap();
                let paused = wait_state(&service, &run.id, AnalysisRunState::Paused).await;
                assert!(paused.jev_pinned_model.is_none());
                assert_eq!(paused.jev_probe_attempts, 1);
            } else {
                service.cancel_analysis_run(&run.id).await.unwrap();
                let cancelled = wait_state(&service, &run.id, AnalysisRunState::Cancelled).await;
                assert!(cancelled.jev_pinned_model.is_none());
                assert_eq!(cancelled.jev_probe_attempts, 1);
            }
            release_tx.send(()).unwrap();
            server.join().unwrap();
            assert!(service
                .project_graph("project-test")
                .unwrap()
                .inferred_relations
                .is_empty());
            assert!(service
                .sessions
                .inferred_pair_outcomes("project-test")
                .unwrap()
                .is_empty());
            let _ = fs::remove_dir_all(root);
        }
    }

    #[tokio::test]
    async fn jev_authentication_pauses_and_rate_limit_retry_is_budgeted() {
        for authentication in [true, false] {
            let root = root(if authentication { "jev-401" } else { "jev-429" });
            let service = service(&root, "ok", 2).await;
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let count = if authentication { 1 } else { 2 };
                for attempt in 0..count {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buffer = [0u8; 8192];
                    let request: serde_json::Value = loop {
                        let read = stream.read(&mut buffer).unwrap();
                        bytes.extend_from_slice(&buffer[..read]);
                        let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n")
                        else {
                            continue;
                        };
                        let header = String::from_utf8_lossy(&bytes[..split]);
                        let length: usize = header
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|value| value.parse().ok())
                            })
                            .unwrap();
                        if bytes.len() >= split + 4 + length {
                            break serde_json::from_slice(&bytes[split + 4..split + 4 + length])
                                .unwrap();
                        }
                    };
                    let (status, body) = if authentication {
                        (401, "{}".to_string())
                    } else if attempt == 0 {
                        (429, "{}".to_string())
                    } else {
                        let answers: serde_json::Map<String, serde_json::Value> = request["questions"].as_object().unwrap().keys().map(|key| {
                                (key.clone(), serde_json::json!({"type":"choice","choice":"REJECTS","confidence":0.8,
                                    "probabilities":{"SUPPORTS":0.1,"REJECTS":0.8,"UNKNOWN":0.1}}))
                            }).collect();
                        (
                            200,
                            serde_json::json!({"model":"jev-1.13.0","answers":answers,
                                "usage":{"input_tokens":20,"output_tokens":5}})
                            .to_string(),
                        )
                    };
                    write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                }
            });
            service
                .save_jev(base_url, "jev-1.13.0".into(), Some("synthetic-key".into()))
                .await
                .unwrap();
            let run = service
                .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
                .await
                .unwrap();
            if authentication {
                let paused = wait_state(&service, &run.id, AnalysisRunState::Paused).await;
                assert_eq!(paused.total_calls, 3);
                assert_eq!(
                    paused
                        .units
                        .iter()
                        .find(|unit| unit.stage == AnalysisStage::Relation)
                        .unwrap()
                        .attempts,
                    1
                );
                assert!(paused.pause_reason.unwrap().contains("认证"));
                assert!(service
                    .sessions
                    .inferred_pair_outcomes("project-test")
                    .unwrap()
                    .is_empty());
            } else {
                let complete = wait_state(&service, &run.id, AnalysisRunState::Complete).await;
                assert_eq!(complete.total_calls, 4);
                assert_eq!(complete.total_questions, 32);
                assert_eq!(
                    service
                        .project_graph("project-test")
                        .unwrap()
                        .inference_outcomes[0]
                        .status,
                    "none"
                );
            }
            server.join().unwrap();
            let _ = fs::remove_dir_all(root);
        }
    }

    #[tokio::test]
    async fn jev_cache_requires_matching_nonsecret_config_rules_and_pinned_actual_model() {
        let root = root("jev-cache-identity");
        let service = service(&root, "ok", 2).await;
        let base = "http://127.0.0.1:4242";
        service
            .save_jev(
                base.into(),
                "jev-1.13.0".into(),
                Some("synthetic-key-a".into()),
            )
            .await
            .unwrap();
        let candidate = service
            .candidate_preview("project-test")
            .unwrap()
            .candidates
            .remove(0);
        let input_version = inferred::candidate_version(&service, &candidate).unwrap();
        let mut saved = InferredPairOutcome {
            candidate_id: candidate.id.clone(),
            project_id: "project-test".into(),
            input_version: input_version.clone(),
            status: "none".into(),
            unknown_count: 0,
            decisions: Vec::new(),
            relations: Vec::new(),
            jev_identity: Some(JevDecisionIdentity {
                base_url: base.into(),
                requested_model: "jev-1.13.0".into(),
                actual_model: "jev-1.13.0".into(),
                rules_version: RELATION_RULES_VERSION.into(),
                input_character_limit: AnalysisLimits::default().input_character_limit,
            }),
        };
        async fn pending(service: &SourceService) -> u64 {
            service
                .analysis_preview("project-test", AnalysisLimits::default())
                .await
                .unwrap()
                .stages[1]
                .pending_items
        }
        service.sessions.save_inferred_pair_outcome(&saved).unwrap();
        assert_eq!(pending(&service).await, 0);
        assert_eq!(
            service
                .analysis_preview(
                    "project-test",
                    AnalysisLimits {
                        input_character_limit: 8_000,
                        ..AnalysisLimits::default()
                    }
                )
                .await
                .unwrap()
                .stages[1]
                .pending_items,
            1
        );
        service
            .save_jev(
                base.into(),
                "jev-1.13.0".into(),
                Some("synthetic-key-b".into()),
            )
            .await
            .unwrap();
        assert_eq!(pending(&service).await, 0, "API Key 轮换不改变判断身份");

        saved.jev_identity.as_mut().unwrap().actual_model = "jev-1.14.0".into();
        service.sessions.save_inferred_pair_outcome(&saved).unwrap();
        assert_eq!(pending(&service).await, 1);
        saved.jev_identity.as_mut().unwrap().actual_model = "jev-1.13.0".into();
        saved.jev_identity.as_mut().unwrap().rules_version = "older-choice-rules".into();
        service.sessions.save_inferred_pair_outcome(&saved).unwrap();
        assert_eq!(pending(&service).await, 1);
        saved.jev_identity.as_mut().unwrap().rules_version = RELATION_RULES_VERSION.into();
        service.sessions.save_inferred_pair_outcome(&saved).unwrap();
        service
            .save_jev(base.into(), "jev-1.14.0".into(), None)
            .await
            .unwrap();
        assert_eq!(pending(&service).await, 1);
        service
            .save_jev(
                "http://127.0.0.1:4343".into(),
                "jev-1.13.0".into(),
                Some("synthetic-key-c".into()),
            )
            .await
            .unwrap();
        assert_eq!(pending(&service).await, 1);
        service
            .save_jev(
                base.into(),
                "jev-latest".into(),
                Some("synthetic-key-d".into()),
            )
            .await
            .unwrap();
        saved.jev_identity.as_mut().unwrap().requested_model = "jev-latest".into();
        saved.jev_identity.as_mut().unwrap().actual_model = "jev-1.13.0".into();
        service.sessions.save_inferred_pair_outcome(&saved).unwrap();
        assert_eq!(pending(&service).await, 1, "别名目标未经探测时不复用旧判断");
        saved.jev_identity.as_mut().unwrap().actual_model = "jev-1.14.0".into();
        service.sessions.save_inferred_pair_outcome(&saved).unwrap();
        assert_eq!(pending(&service).await, 1, "别名目标变更仍需重新判断");
        saved.jev_identity = None;
        service.sessions.save_inferred_pair_outcome(&saved).unwrap();
        assert_eq!(pending(&service).await, 1, "旧缓存缺少配置身份时不可复用");

        service
            .save_jev(base.into(), "jev-1.14.0".into(), None)
            .await
            .unwrap();
        let run = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    call_limit: 2,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let paused = wait_state(&service, &run.id, AnalysisRunState::Paused).await;
        assert!(paused
            .units
            .iter()
            .any(|unit| unit.stage == AnalysisStage::Relation && unit.id == candidate.id));
        let mut old_rules = paused.clone();
        old_rules.jev_rules_version = "older-choice-rules".into();
        service.sessions.save_analysis_run(&old_rules).unwrap();
        assert!(matches!(
            service.continue_analysis_run(&run.id, 2, |_| {}).await,
            Err(AppError {
                code: ErrorCode::AnalysisConfigChanged,
                ..
            })
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn alias_probe_pins_one_actual_version_across_multiple_candidates() {
        let root = root("jev-alias-pinned-batch");
        let service = service(&root, "ok", 3).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for index in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_jev_post(&mut stream);
                if index == 0 {
                    assert_eq!(request["model"], "jev-latest");
                    assert_eq!(request["state"]["ticket"], "合成工单 A");
                    write_jev_json(&mut stream, 200, synthetic_probe_response("jev-1.13.0"));
                } else {
                    // 即使别名此刻已移至 1.14.0，后续候选仍应请求本批固定的 1.13.0。
                    assert_eq!(request["model"], "jev-1.13.0");
                    let response = if index == 1 {
                        let mut response = rejected_relation_response(&request, "jev-1.13.0");
                        response["answers"]["fixes_ab"] = serde_json::json!({"type":"choice","choice":"SUPPORTS",
                            "confidence":0.8,"probabilities":{"SUPPORTS":0.8,"REJECTS":0.1,"UNKNOWN":0.1}});
                        response
                    } else if index == 2 {
                        serde_json::json!({"model":"jev-1.13.0",
                            "answers":{"fixes_ab":{"type":"choice","choice":"p0","confidence":0.9,
                                "probabilities":{"p0":0.9,"INSUFFICIENT":0.1}}},
                            "usage":{"input_tokens":20,"output_tokens":5}})
                    } else {
                        rejected_relation_response(&request, "jev-1.13.0")
                    };
                    write_jev_json(&mut stream, 200, response);
                }
                requests.push(request);
            }
            requests
        });
        service
            .save_jev(base_url, "jev-latest".into(), Some("synthetic-key".into()))
            .await
            .unwrap();
        let preview = service
            .analysis_preview("project-test", AnalysisLimits::default())
            .await
            .unwrap();
        assert_eq!(preview.stages[1].pending_items, 3);
        assert_eq!(preview.stages[1].maximum_calls, 12); // 三对候选和一次探测，各自最多三次尝试
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        let completed = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!(completed.jev_pinned_model.as_deref(), Some("jev-1.13.0"));
        assert_eq!(completed.jev_probe_attempts, 1);
        assert_eq!(completed.total_calls, 9); // 三次总结、一次探测、三次分类、一次证据选择和一次命名
        assert_eq!(completed.total_questions, 50);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(
            requests[0].to_string().chars().count(),
            JevClient::synthetic_request_characters("jev-latest")
        );
        let outcomes = service
            .sessions
            .inferred_pair_outcomes("project-test")
            .unwrap();
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|item| item
            .jev_identity
            .as_ref()
            .is_some_and(|identity| identity.requested_model == "jev-latest"
                && identity.actual_model == "jev-1.13.0")));
        assert_eq!(
            outcomes
                .iter()
                .filter(|item| item.status == "valid")
                .count(),
            1
        );
        assert_eq!(
            outcomes.iter().filter(|item| item.status == "none").count(),
            2
        );
        let graph = service.project_graph("project-test").unwrap();
        assert_eq!(graph.inferred_relations.len(), 1);
        assert_eq!(graph.inferred_relations[0].requested_model, "jev-latest");
        assert_eq!(graph.inferred_relations[0].actual_model, "jev-1.13.0");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn alias_probe_without_a_version_pauses_before_classification() {
        let root = root("jev-alias-no-version");
        let service = service(&root, "ok", 2).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_jev_post(&mut stream);
            write_jev_json(&mut stream, 200, synthetic_probe_response("jev-latest"));
            request
        });
        service
            .save_jev(base_url, "jev-latest".into(), Some("synthetic-key".into()))
            .await
            .unwrap();
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(paused.total_calls, 3);
        assert_eq!(paused.total_questions, 1);
        assert_eq!(paused.jev_probe_attempts, 1);
        assert!(paused.jev_pinned_model.is_none());
        assert!(paused.pause_reason.unwrap().contains("无法确认"));
        assert!(paused
            .units
            .iter()
            .any(|unit| unit.stage == AnalysisStage::Relation
                && unit.state == AnalysisUnitState::Pending));
        assert!(service
            .sessions
            .inferred_pair_outcomes("project-test")
            .unwrap()
            .is_empty());
        assert_eq!(server.join().unwrap()["model"], "jev-latest");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn oversized_synthetic_probe_is_rejected_without_spending_a_call() {
        let root = root("jev-alias-probe-input-limit");
        let service = service(&root, "ok", 2).await;
        let model = format!("jev-{}", "x".repeat(2_500));
        service
            .save_jev(
                "http://127.0.0.1:4343".into(),
                model,
                Some("synthetic-key".into()),
            )
            .await
            .unwrap();
        let started = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    input_character_limit: 2_000,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(paused.total_calls, 2);
        assert_eq!(paused.jev_probe_attempts, 0);
        assert!(paused.jev_pinned_model.is_none());
        assert!(paused.pause_reason.unwrap().contains("输入字符上限"));
        assert!(service
            .sessions
            .inferred_pair_outcomes("project-test")
            .unwrap()
            .is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn alias_probe_and_retry_each_consume_the_batch_call_limit() {
        let root = root("jev-alias-probe-budget");
        let service = service(&root, "ok", 2).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        service
            .save_jev(base_url, "jev-latest".into(), Some("synthetic-key".into()))
            .await
            .unwrap();
        let started = service
            .start_project_analysis(
                "project-test".into(),
                AnalysisLimits {
                    call_limit: 2,
                    ..AnalysisLimits::default()
                },
                |_| {},
            )
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(paused.total_calls, 2);
        assert_eq!(paused.jev_probe_attempts, 0);
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        listener.set_nonblocking(false).unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for attempt in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_jev_post(&mut stream);
                match attempt {
                    0 => {
                        assert_eq!(request["model"], "jev-latest");
                        write_jev_json(&mut stream, 429, serde_json::json!({}));
                    }
                    1 | 2 => {
                        assert_eq!(request["model"], "jev-latest");
                        write_jev_json(
                            &mut stream,
                            200,
                            synthetic_probe_response(if attempt == 1 {
                                "jev-1.13.0"
                            } else {
                                "jev-1.14.0"
                            }),
                        );
                    }
                    _ => {
                        assert_eq!(request["model"], "jev-1.14.0");
                        write_jev_json(
                            &mut stream,
                            200,
                            rejected_relation_response(&request, "jev-1.14.0"),
                        );
                    }
                }
                requests.push(request);
            }
            requests
        });
        service
            .continue_analysis_run(&started.id, 2, |_| {})
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(paused.batch_calls, 2);
        assert_eq!(paused.total_calls, 4);
        assert_eq!(paused.total_questions, 2);
        assert_eq!(paused.jev_probe_attempts, 2);
        assert_eq!(paused.jev_pinned_model.as_deref(), Some("jev-1.13.0"));
        assert!(paused
            .units
            .iter()
            .any(|unit| unit.stage == AnalysisStage::Relation
                && unit.state == AnalysisUnitState::Pending));
        service
            .continue_analysis_run(&started.id, 2, |_| {})
            .await
            .unwrap();
        let completed = wait_state(&service, &started.id, AnalysisRunState::Complete).await;
        assert_eq!(completed.batch_calls, 2);
        assert_eq!(completed.total_calls, 6);
        assert_eq!(completed.jev_probe_attempts, 1);
        assert_eq!(completed.jev_pinned_model.as_deref(), Some("jev-1.14.0"));
        assert_eq!(
            service
                .sessions
                .inferred_pair_outcomes("project-test")
                .unwrap()[0]
                .jev_identity
                .as_ref()
                .unwrap()
                .actual_model,
            "jev-1.14.0"
        );
        assert_eq!(server.join().unwrap().len(), 4);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn pinned_batch_rejects_a_later_response_from_another_version() {
        let root = root("jev-alias-response-drift");
        let service = service(&root, "ok", 2).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let probe = read_jev_post(&mut stream);
            write_jev_json(&mut stream, 200, synthetic_probe_response("jev-1.13.0"));
            let (mut stream, _) = listener.accept().unwrap();
            let classification = read_jev_post(&mut stream);
            assert_eq!(classification["model"], "jev-1.13.0");
            write_jev_json(
                &mut stream,
                200,
                rejected_relation_response(&classification, "jev-1.14.0"),
            );
            (probe, classification)
        });
        service
            .save_jev(base_url, "jev-latest".into(), Some("synthetic-key".into()))
            .await
            .unwrap();
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(paused.total_calls, 4);
        assert_eq!(paused.jev_pinned_model.as_deref(), Some("jev-1.13.0"));
        assert!(paused.pause_reason.unwrap().contains("实际模型版本变化"));
        assert!(service
            .sessions
            .inferred_pair_outcomes("project-test")
            .unwrap()
            .is_empty());
        assert_eq!(server.join().unwrap().0["model"], "jev-latest");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn alias_batch_pauses_if_the_gateway_rejects_the_fixed_version() {
        let root = root("jev-alias-fixed-id-rejected");
        let service = service(&root, "ok", 2).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let probe = read_jev_post(&mut stream);
            write_jev_json(&mut stream, 200, synthetic_probe_response("jev-1.13.0"));
            let (mut stream, _) = listener.accept().unwrap();
            let classification = read_jev_post(&mut stream);
            assert_eq!(classification["model"], "jev-1.13.0");
            write_jev_json(&mut stream, 422, serde_json::json!({}));
            (probe, classification)
        });
        service
            .save_jev(base_url, "jev-latest".into(), Some("synthetic-key".into()))
            .await
            .unwrap();
        let started = service
            .start_project_analysis("project-test".into(), AnalysisLimits::default(), |_| {})
            .await
            .unwrap();
        let paused = wait_state(&service, &started.id, AnalysisRunState::Paused).await;
        assert_eq!(paused.total_calls, 4);
        assert_eq!(paused.jev_pinned_model.as_deref(), Some("jev-1.13.0"));
        assert!(paused.pause_reason.unwrap().contains("固定版本请求被拒绝"));
        assert!(service
            .sessions
            .inferred_pair_outcomes("project-test")
            .unwrap()
            .is_empty());
        assert_eq!(server.join().unwrap().0["model"], "jev-latest");
        let _ = fs::remove_dir_all(root);
    }
}
