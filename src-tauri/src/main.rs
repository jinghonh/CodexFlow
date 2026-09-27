use codexflow_core::{ProjectThreadQuery, ProjectThreadQueryResult, SourceService};
use codexflow_domain::{
    AnalysisLimits, AnalysisPreview, AnalysisRun, AnalysisRunState, AnalysisStage,
    AnalysisStageSelection, AnalysisUnitState, AppError, CandidatePreview, DisplayTheme,
    EmbeddingStatus, EmbeddingValidation, EvidenceCheck, EvidencePage, FactPage, GlobalTopicView,
    HistoryCoverage, HistoryItemLocation, HistoryItemPage, HistoryTurnPage, IndexRun,
    JevConnectionResult, JevInferenceResult, JevStatus, ProjectCatalog, ProjectGraph,
    ProjectSessions, ProjectTimeline, ProjectWorkstreams, RelationReview, SemanticIndexResult,
    SemanticIndexRun, SemanticIndexStage, SessionList, SourceStatus, SummaryEvidenceCheck,
    SummaryPreview, SummaryRun, SummaryRunState, TextStatus, TextValidation, UserRelationDecision,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::{Emitter, Manager};

#[cfg(feature = "perf-probe")]
static PERF_STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

#[tauri::command]
fn record_perf_sample(
    app: tauri::AppHandle,
    kind: String,
    elapsed_ms: Option<f64>,
) -> Result<(), String> {
    #[cfg(feature = "perf-probe")]
    {
        use std::io::Write;
        const KINDS: &[&str] = &[
            "startup",
            "query",
            "filter",
            "evidence_turns",
            "evidence_items",
            "evidence_facts",
        ];
        if !KINDS.contains(&kind.as_str()) {
            return Err("未知性能样本".into());
        }
        let elapsed = if kind == "startup" {
            PERF_STARTED
                .get()
                .ok_or("缺少启动时间")?
                .elapsed()
                .as_secs_f64()
                * 1_000.0
        } else {
            elapsed_ms
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or("无效性能样本")?
        };
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("perf-samples.txt"))
            .map_err(|error| error.to_string())?;
        writeln!(file, "{kind}={elapsed:.3}").map_err(|error| error.to_string())?;
    }
    #[cfg(not(feature = "perf-probe"))]
    let _ = (app, kind, elapsed_ms);
    Ok(())
}

struct AppState {
    service: Result<Arc<SourceService>, AppError>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    theme: DisplayTheme,
    source: SourceStatus,
}

fn service<'a>(state: &'a tauri::State<'_, AppState>) -> Result<&'a Arc<SourceService>, AppError> {
    state.service.as_ref().map_err(Clone::clone)
}

#[tauri::command]
fn get_runtime_platform() -> &'static str {
    std::env::consts::OS
}

#[tauri::command]
async fn get_settings(state: tauri::State<'_, AppState>) -> Result<SettingsView, AppError> {
    let (theme, source) = service(&state)?.settings().await;
    Ok(SettingsView { theme, source })
}

#[tauri::command]
async fn get_source_status(state: tauri::State<'_, AppState>) -> Result<SourceStatus, AppError> {
    Ok(service(&state)?.status().await)
}

#[tauri::command]
async fn connect_source(
    state: tauri::State<'_, AppState>,
    selected_binary: Option<String>,
) -> Result<SourceStatus, AppError> {
    let selected_binary = if cfg!(windows) { None } else { selected_binary };
    service(&state)?.connect(selected_binary).await
}

#[tauri::command]
async fn set_display_theme(
    state: tauri::State<'_, AppState>,
    theme: DisplayTheme,
) -> Result<DisplayTheme, AppError> {
    service(&state)?.set_theme(theme).await
}

#[tauri::command]
async fn get_jev_status(state: tauri::State<'_, AppState>) -> Result<JevStatus, AppError> {
    service(&state)?.jev_status().await
}

#[tauri::command]
async fn save_jev_settings(
    state: tauri::State<'_, AppState>,
    base_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<JevStatus, AppError> {
    service(&state)?.save_jev(base_url, model, api_key).await
}

#[tauri::command]
async fn delete_jev_credential(state: tauri::State<'_, AppState>) -> Result<JevStatus, AppError> {
    service(&state)?.delete_jev_credential().await
}

#[tauri::command]
async fn check_jev_connection(
    state: tauri::State<'_, AppState>,
) -> Result<JevConnectionResult, AppError> {
    service(&state)?.check_jev_connection().await
}

#[tauri::command]
async fn test_jev_inference(
    state: tauri::State<'_, AppState>,
) -> Result<JevInferenceResult, AppError> {
    service(&state)?.test_jev_inference().await
}

#[tauri::command]
async fn cancel_jev_request(state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    service(&state)?.cancel_jev().await;
    Ok(())
}

#[tauri::command]
async fn get_text_status(state: tauri::State<'_, AppState>) -> Result<TextStatus, AppError> {
    Ok(service(&state)?.text_status().await)
}

#[tauri::command]
async fn save_text_settings(
    state: tauri::State<'_, AppState>,
    base_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<TextStatus, AppError> {
    service(&state)?.save_text(base_url, model, api_key).await
}

#[tauri::command]
async fn delete_text_credential(state: tauri::State<'_, AppState>) -> Result<TextStatus, AppError> {
    service(&state)?.delete_text_credential().await
}

#[tauri::command]
async fn validate_text_settings(
    state: tauri::State<'_, AppState>,
) -> Result<TextValidation, AppError> {
    service(&state)?.validate_text().await
}

#[tauri::command]
async fn cancel_text_request(state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    service(&state)?.cancel_text_request().await;
    Ok(())
}

#[tauri::command]
async fn get_embedding_status(
    state: tauri::State<'_, AppState>,
) -> Result<EmbeddingStatus, AppError> {
    Ok(service(&state)?.embedding_status().await)
}

#[tauri::command]
async fn save_embedding_settings(
    state: tauri::State<'_, AppState>,
    base_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<EmbeddingStatus, AppError> {
    service(&state)?
        .save_embedding(base_url, model, api_key)
        .await
}

#[tauri::command]
async fn delete_embedding_credential(
    state: tauri::State<'_, AppState>,
) -> Result<EmbeddingStatus, AppError> {
    service(&state)?.delete_embedding_credential().await
}

#[tauri::command]
async fn test_embedding_settings(
    state: tauri::State<'_, AppState>,
) -> Result<EmbeddingValidation, AppError> {
    service(&state)?.test_embedding().await
}

#[tauri::command]
async fn cancel_embedding_request(state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    service(&state)?.cancel_embedding_request().await;
    Ok(())
}

#[tauri::command]
fn get_global_topic_view(state: tauri::State<'_, AppState>) -> Result<GlobalTopicView, AppError> {
    service(&state)?.global_topic_view()
}

#[tauri::command]
async fn create_topic_label(
    state: tauri::State<'_, AppState>,
    name: String,
    description: String,
) -> Result<GlobalTopicView, AppError> {
    service(&state)?.create_topic_label(name, description).await
}

#[tauri::command]
async fn update_topic_label(
    state: tauri::State<'_, AppState>,
    id: String,
    name: String,
    description: String,
) -> Result<GlobalTopicView, AppError> {
    service(&state)?
        .update_topic_label(id, name, description)
        .await
}

#[tauri::command]
async fn delete_topic_label(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<GlobalTopicView, AppError> {
    service(&state)?.delete_topic_label(&id).await
}

#[tauri::command]
async fn set_thread_topic(
    state: tauri::State<'_, AppState>,
    thread_id: String,
    topic_id: Option<String>,
) -> Result<GlobalTopicView, AppError> {
    service(&state)?
        .set_thread_topic(&thread_id, topic_id)
        .await
}

#[tauri::command]
async fn rebuild_global_semantic_index(
    state: tauri::State<'_, AppState>,
) -> Result<SemanticIndexResult, AppError> {
    service(&state)?.rebuild_global_semantic_index().await
}

#[tauri::command]
async fn start_semantic_index_run(
    state: tauri::State<'_, AppState>,
) -> Result<SemanticIndexRun, AppError> {
    service(&state)?.clone().start_semantic_index_run().await
}

#[tauri::command]
fn get_semantic_index_run(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<Option<SemanticIndexRun>, AppError> {
    service(&state)?.semantic_index_run(&run_id)
}

#[tauri::command]
fn get_latest_semantic_index_run(
    state: tauri::State<'_, AppState>,
) -> Result<Option<SemanticIndexRun>, AppError> {
    Ok(service(&state)?.semantic_index_runs()?.into_iter().next())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRunsSnapshot {
    runs: Vec<ModelRunSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelRunSummary {
    id: String,
    kind: &'static str,
    name: &'static str,
    state: AnalysisRunState,
    stage: &'static str,
    project_id: Option<String>,
    thread_id: Option<String>,
    completed: u64,
    total: Option<u64>,
    unit: &'static str,
    calls: u64,
    started_at_unix_ms: i64,
    error: Option<String>,
    result: String,
    stage_selection: Option<AnalysisStageSelection>,
}

fn project_run_summary(run: AnalysisRun) -> ModelRunSummary {
    let selection = run.stage_selection;
    let (name, stage, unit, work_stage) =
        if selection.relations && !selection.summary && !selection.naming {
            (
                "候选关系判断",
                if run.units.is_empty() && run.state != AnalysisRunState::Complete {
                    "准备候选会话对"
                } else {
                    "候选关系判断"
                },
                "对",
                Some(AnalysisStage::Relation),
            )
        } else if selection.naming && !selection.summary && !selection.relations {
            (
                "工作流命名",
                "工作流命名",
                "组",
                Some(AnalysisStage::Naming),
            )
        } else if selection.summary && !selection.relations && !selection.naming {
            (
                "项目会话总结",
                "会话总结",
                "条",
                Some(AnalysisStage::Summary),
            )
        } else {
            let stage = match run
                .units
                .iter()
                .find(|unit| unit.state == AnalysisUnitState::Running)
                .map(|unit| unit.stage)
            {
                Some(AnalysisStage::Relation | AnalysisStage::EvidenceSelection) => "候选关系判断",
                Some(AnalysisStage::Naming) => "工作流命名",
                _ => "会话总结",
            };
            ("项目分析", stage, "项", None)
        };
    let units: Vec<_> = run
        .units
        .iter()
        .filter(|unit| {
            work_stage.map_or(true, |stage| {
                unit.stage == stage
                    || (stage == AnalysisStage::Relation
                        && unit.stage == AnalysisStage::EvidenceSelection)
            })
        })
        .collect();
    let completed = units
        .iter()
        .filter(|unit| {
            matches!(
                unit.state,
                AnalysisUnitState::Succeeded | AnalysisUnitState::Failed
            )
        })
        .count() as u64;
    let total = u64::from(run.planned_items).max(units.len() as u64);
    ModelRunSummary {
        id: run.id,
        kind: "project",
        name,
        state: run.state,
        stage,
        project_id: Some(run.project_id),
        thread_id: None,
        completed,
        total: (total > 0 || run.state == AnalysisRunState::Complete).then_some(total),
        unit,
        calls: u64::from(run.total_calls),
        started_at_unix_ms: run.started_at_unix_ms,
        error: run.error.map(|error| error.message).or(run.pause_reason),
        result: format!(
            "成功 {} · 失败 {} · 待处理 {}",
            run.succeeded, run.failed, run.pending
        ),
        stage_selection: Some(selection),
    }
}

fn summary_run_summary(run: SummaryRun) -> ModelRunSummary {
    let state = match run.state {
        SummaryRunState::Running => AnalysisRunState::Running,
        SummaryRunState::Cancelling => AnalysisRunState::Cancelling,
        SummaryRunState::Complete => AnalysisRunState::Complete,
        SummaryRunState::Failed => AnalysisRunState::Failed,
        SummaryRunState::Cancelled => AnalysisRunState::Cancelled,
    };
    ModelRunSummary {
        id: run.id,
        kind: "summary",
        name: "单条会话总结",
        state,
        stage: "生成会话总结",
        project_id: run.project_id,
        thread_id: Some(run.thread_id),
        completed: u64::from(matches!(
            run.state,
            SummaryRunState::Complete | SummaryRunState::Failed
        )),
        total: Some(1),
        unit: "条",
        calls: u64::from(run.model_calls),
        started_at_unix_ms: run.started_at_unix_ms,
        error: run.error.map(|error| error.message),
        result: if run.state == SummaryRunState::Complete {
            if run.reused_cache {
                "已复用有效总结"
            } else {
                "总结已保存"
            }
        } else if run.state == SummaryRunState::Failed {
            "旧总结仍保留"
        } else {
            ""
        }
        .into(),
        stage_selection: None,
    }
}

fn semantic_run_summary(run: SemanticIndexRun) -> ModelRunSummary {
    let (stage, completed, total, unit) = match run.stage {
        SemanticIndexStage::Preparing => ("准备全局主题分析", 0, None, "项"),
        SemanticIndexStage::Embedding => (
            "生成语义向量",
            run.embedding_completed,
            Some(run.embedding_total),
            "条",
        ),
        SemanticIndexStage::TopicAssignment => (
            "分配主题",
            run.assignment_completed,
            Some(run.assignment_total),
            "条",
        ),
        SemanticIndexStage::CrossProjectRelation => (
            "判断跨项目候选关系",
            run.relation_completed,
            Some(run.relation_total),
            "对",
        ),
    };
    ModelRunSummary {
        id: run.id,
        kind: "semantic",
        name: "全局主题处理",
        state: run.state,
        stage,
        project_id: None,
        thread_id: None,
        completed,
        total,
        unit,
        calls: run.total_calls,
        started_at_unix_ms: run.started_at_unix_ms,
        error: run.error.map(|error| error.message),
        result: run
            .result
            .map(|result| {
                format!(
                    "新增向量 {} · 主题归属 {} · 跨项目关系 {}",
                    result.indexed, result.topic_assignments, result.cross_project_relations
                )
            })
            .unwrap_or_default(),
        stage_selection: None,
    }
}

#[tauri::command]
fn get_model_runs(state: tauri::State<'_, AppState>) -> Result<ModelRunsSnapshot, AppError> {
    let service = service(&state)?;
    let mut runs: Vec<ModelRunSummary> = service
        .analysis_runs()?
        .into_iter()
        .map(project_run_summary)
        .collect();
    runs.extend(
        service
            .summary_runs()?
            .into_iter()
            .filter(|run| !run.from_batch)
            .map(summary_run_summary),
    );
    runs.extend(
        service
            .semantic_index_runs()?
            .into_iter()
            .map(semantic_run_summary),
    );
    runs.sort_by(|left, right| right.started_at_unix_ms.cmp(&left.started_at_unix_ms));
    Ok(ModelRunsSnapshot { runs })
}

#[tauri::command]
async fn cancel_semantic_index(state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    service(&state)?.cancel_semantic_index().await;
    Ok(())
}

#[tauri::command]
fn get_session_list(state: tauri::State<'_, AppState>) -> Result<SessionList, AppError> {
    service(&state)?.cached_sessions()
}

#[tauri::command]
async fn load_thread_history(
    state: tauri::State<'_, AppState>,
    thread_id: String,
) -> Result<HistoryCoverage, AppError> {
    service(&state)?.load_thread_history(&thread_id).await
}

#[tauri::command]
async fn get_summary_preview(
    state: tauri::State<'_, AppState>,
    thread_id: String,
) -> Result<SummaryPreview, AppError> {
    service(&state)?.summary_preview(&thread_id).await
}

#[tauri::command]
async fn start_thread_summary(
    state: tauri::State<'_, AppState>,
    thread_id: String,
) -> Result<SummaryRun, AppError> {
    service(&state)?
        .clone()
        .start_thread_summary(thread_id)
        .await
}

#[tauri::command]
fn get_latest_summary_run(
    state: tauri::State<'_, AppState>,
    thread_id: String,
) -> Result<Option<SummaryRun>, AppError> {
    service(&state)?.latest_summary_run(&thread_id)
}

#[tauri::command]
fn get_summary_run(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<Option<SummaryRun>, AppError> {
    service(&state)?.summary_run(&run_id)
}

#[tauri::command]
fn inspect_summary_evidence(
    state: tauri::State<'_, AppState>,
    thread_id: String,
    evidence_id: String,
) -> Result<SummaryEvidenceCheck, AppError> {
    service(&state)?.inspect_summary_evidence(&thread_id, &evidence_id)
}

#[tauri::command]
fn cancel_summary_run(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<SummaryRun, AppError> {
    service(&state)?.cancel_summary_run(&run_id)
}

#[tauri::command]
fn get_history_turns(
    state: tauri::State<'_, AppState>,
    thread_id: String,
    offset: u64,
    limit: u32,
) -> Result<HistoryTurnPage, AppError> {
    service(&state)?.history_turns(&thread_id, offset, limit)
}

#[tauri::command]
fn get_history_items(
    state: tauri::State<'_, AppState>,
    thread_id: String,
    turn_id: String,
    offset: u64,
    limit: u32,
) -> Result<HistoryItemPage, AppError> {
    service(&state)?.history_items(&thread_id, &turn_id, offset, limit)
}

#[tauri::command]
fn locate_history_item(
    state: tauri::State<'_, AppState>,
    thread_id: String,
    turn_id: String,
    item_id: String,
) -> Result<Option<HistoryItemLocation>, AppError> {
    service(&state)?.locate_history_item(&thread_id, &turn_id, &item_id)
}

#[tauri::command]
fn get_source_facts(
    state: tauri::State<'_, AppState>,
    thread_id: String,
    offset: u64,
    limit: u32,
) -> Result<FactPage, AppError> {
    service(&state)?.source_facts(&thread_id, offset, limit)
}

#[tauri::command]
fn get_source_evidence(
    state: tauri::State<'_, AppState>,
    thread_id: String,
    offset: u64,
    limit: u32,
) -> Result<EvidencePage, AppError> {
    service(&state)?.source_evidence(&thread_id, offset, limit)
}

#[tauri::command]
fn validate_source_evidence(
    state: tauri::State<'_, AppState>,
    evidence_id: String,
) -> Result<EvidenceCheck, AppError> {
    service(&state)?.validate_source_evidence(&evidence_id)
}

#[tauri::command]
async fn refresh_session_list(state: tauri::State<'_, AppState>) -> Result<SessionList, AppError> {
    service(&state)?.refresh_sessions().await
}

#[tauri::command]
async fn start_index_run(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    project_id: Option<String>,
) -> Result<IndexRun, AppError> {
    service(&state)?.start_refresh(project_id, move |run| {
        let _ = app.emit("index-run", run);
    })
}

#[tauri::command]
fn get_latest_index_run(state: tauri::State<'_, AppState>) -> Result<Option<IndexRun>, AppError> {
    service(&state)?.latest_refresh()
}

#[tauri::command]
fn get_index_run(state: tauri::State<'_, AppState>, id: String) -> Result<IndexRun, AppError> {
    service(&state)?.refresh_status(&id)
}

#[tauri::command]
fn cancel_index_run(state: tauri::State<'_, AppState>, id: String) -> Result<IndexRun, AppError> {
    service(&state)?.cancel_refresh(&id)
}

#[tauri::command]
fn get_project_catalog(state: tauri::State<'_, AppState>) -> Result<ProjectCatalog, AppError> {
    service(&state)?.project_catalog()
}

#[tauri::command]
fn get_project_sessions(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<ProjectSessions, AppError> {
    service(&state)?.project_sessions(&project_id)
}

#[tauri::command]
async fn query_project_threads(
    state: tauri::State<'_, AppState>,
    project_id: String,
    query: ProjectThreadQuery,
) -> Result<ProjectThreadQueryResult, AppError> {
    let service = service(&state)?.clone();
    tauri::async_runtime::spawn_blocking(move || service.query_project_threads(&project_id, query))
        .await
        .map_err(|_| AppError::store("后台会话查询失败。"))?
}

#[tauri::command]
async fn get_project_graph(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<ProjectGraph, AppError> {
    let service = service(&state)?.clone();
    tauri::async_runtime::spawn_blocking(move || service.project_graph(&project_id))
        .await
        .map_err(|_| AppError::store("后台关系图查询失败。"))?
}

#[tauri::command]
async fn get_project_workstreams(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<ProjectWorkstreams, AppError> {
    let service = service(&state)?.clone();
    tauri::async_runtime::spawn_blocking(move || service.project_workstreams(&project_id))
        .await
        .map_err(|_| AppError::store("后台工作流查询失败。"))?
}

#[tauri::command]
fn rename_workstream(
    state: tauri::State<'_, AppState>,
    project_id: String,
    workstream_id: String,
    name: String,
    expected_revision: u64,
) -> Result<u64, AppError> {
    service(&state)?.rename_workstream(&project_id, &workstream_id, &name, expected_revision)
}

#[tauri::command]
fn restore_workstream_name(
    state: tauri::State<'_, AppState>,
    project_id: String,
    workstream_id: String,
    expected_revision: u64,
) -> Result<u64, AppError> {
    service(&state)?.restore_workstream_name(&project_id, &workstream_id, expected_revision)
}

#[tauri::command]
fn move_thread_to_workstream(
    state: tauri::State<'_, AppState>,
    project_id: String,
    thread_id: String,
    target_id: Option<String>,
    expected_revision: u64,
) -> Result<u64, AppError> {
    service(&state)?.move_thread_to_workstream(
        &project_id,
        &thread_id,
        target_id.as_deref(),
        expected_revision,
    )
}

#[tauri::command]
fn restore_thread_workstream(
    state: tauri::State<'_, AppState>,
    project_id: String,
    thread_id: String,
    expected_revision: u64,
) -> Result<u64, AppError> {
    service(&state)?.restore_thread_workstream(&project_id, &thread_id, expected_revision)
}

#[tauri::command]
fn decide_inferred_relation(
    state: tauri::State<'_, AppState>,
    project_id: String,
    relation_id: String,
    decision: UserRelationDecision,
    expected_revision: u64,
    expected_evidence_version: Option<String>,
) -> Result<RelationReview, AppError> {
    service(&state)?.decide_inferred_relation(
        &project_id,
        &relation_id,
        decision,
        expected_revision,
        expected_evidence_version.as_deref(),
    )
}

#[tauri::command]
async fn get_candidate_preview(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<CandidatePreview, AppError> {
    let service = service(&state)?.clone();
    tauri::async_runtime::spawn_blocking(move || service.candidate_preview(&project_id))
        .await
        .map_err(|_| AppError::store("后台候选查询失败。"))?
}

#[tauri::command]
async fn get_analysis_preview(
    state: tauri::State<'_, AppState>,
    project_id: String,
    limits: AnalysisLimits,
    stage_selection: AnalysisStageSelection,
) -> Result<AnalysisPreview, AppError> {
    service(&state)?
        .analysis_preview_with_selection(&project_id, limits, stage_selection)
        .await
}

#[tauri::command]
async fn start_project_analysis(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    project_id: String,
    limits: AnalysisLimits,
    stage_selection: AnalysisStageSelection,
) -> Result<AnalysisRun, AppError> {
    service(&state)?
        .clone()
        .start_project_analysis_with_selection(project_id, limits, stage_selection, move |run| {
            let _ = app.emit("analysis-run", run);
        })
        .await
}

#[tauri::command]
fn get_analysis_run(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<Option<AnalysisRun>, AppError> {
    service(&state)?.analysis_run(&run_id)
}

#[tauri::command]
fn get_latest_analysis_run(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<Option<AnalysisRun>, AppError> {
    service(&state)?.latest_analysis_run(&project_id)
}

#[tauri::command]
fn pause_analysis_run(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<AnalysisRun, AppError> {
    service(&state)?.pause_analysis_run(&run_id)
}

#[tauri::command]
async fn cancel_analysis_run(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    run_id: String,
) -> Result<AnalysisRun, AppError> {
    let run = service(&state)?.cancel_analysis_run(&run_id).await?;
    let _ = app.emit("analysis-run", &run);
    Ok(run)
}

#[tauri::command]
async fn continue_analysis_run(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    run_id: String,
    call_limit: u32,
) -> Result<AnalysisRun, AppError> {
    service(&state)?
        .clone()
        .continue_analysis_run(&run_id, call_limit, move |run| {
            let _ = app.emit("analysis-run", run);
        })
        .await
}

#[tauri::command]
async fn get_project_timeline(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<ProjectTimeline, AppError> {
    let service = service(&state)?.clone();
    tauri::async_runtime::spawn_blocking(move || service.project_timeline(&project_id))
        .await
        .map_err(|_| AppError::store("后台时间线查询失败。"))?
}

#[tauri::command]
fn choose_project(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<ProjectCatalog, AppError> {
    service(&state)?.choose_project(&path)
}

#[tauri::command]
fn choose_existing_project(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<ProjectCatalog, AppError> {
    service(&state)?.choose_existing_project(&project_id)
}

fn main() {
    #[cfg(feature = "perf-probe")]
    let _ = PERF_STARTED.set(std::time::Instant::now());
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let service = SourceService::new(data_dir).map(Arc::new);
            if let Ok(recovery) = &service {
                let recovery = Arc::clone(recovery);
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Ok(runs) = recovery.analysis_runs() {
                        for run in runs.into_iter().filter(|run| {
                            run.interrupted
                                && run.state == codexflow_domain::AnalysisRunState::Paused
                        }) {
                            let app = handle.clone();
                            if let Err(error) = recovery
                                .continue_analysis_run(
                                    &run.id,
                                    run.limits.call_limit,
                                    move |updated| {
                                        let _ = app.emit("analysis-run", updated);
                                    },
                                )
                                .await
                            {
                                let _ = recovery.mark_analysis_recovery_failed(&run.id, error);
                            }
                        }
                    }
                    if let Ok(runs) = recovery.summary_runs() {
                        for run in runs.into_iter().filter(|run| {
                            !run.from_batch
                                && run.interrupted
                                && run.state == codexflow_domain::SummaryRunState::Failed
                        }) {
                            if recovery
                                .latest_summary_run(&run.thread_id)
                                .ok()
                                .flatten()
                                .as_ref()
                                .map(|latest| latest.id.as_str())
                                != Some(run.id.as_str())
                            {
                                continue;
                            }
                            if let Err(error) =
                                recovery.start_thread_summary(run.thread_id.clone()).await
                            {
                                let _ = recovery.mark_summary_recovery_failed(&run.id, error);
                            }
                        }
                    }
                    if let Ok(runs) = recovery.semantic_index_runs() {
                        for run in runs.into_iter().filter(|run| {
                            run.interrupted
                                && run.state == codexflow_domain::AnalysisRunState::Paused
                        }) {
                            if let Err(error) = recovery.resume_semantic_index_run(&run.id).await {
                                let _ = recovery.mark_semantic_recovery_failed(&run.id, error);
                            }
                        }
                    }
                });
            }
            app.manage(AppState { service });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_runtime_platform,
            get_settings,
            record_perf_sample,
            get_source_status,
            connect_source,
            set_display_theme,
            get_jev_status,
            save_jev_settings,
            delete_jev_credential,
            check_jev_connection,
            test_jev_inference,
            cancel_jev_request,
            get_text_status,
            save_text_settings,
            delete_text_credential,
            validate_text_settings,
            cancel_text_request,
            get_embedding_status,
            save_embedding_settings,
            delete_embedding_credential,
            test_embedding_settings,
            cancel_embedding_request,
            get_global_topic_view,
            create_topic_label,
            update_topic_label,
            delete_topic_label,
            set_thread_topic,
            rebuild_global_semantic_index,
            start_semantic_index_run,
            get_semantic_index_run,
            get_latest_semantic_index_run,
            get_model_runs,
            cancel_semantic_index,
            get_session_list,
            load_thread_history,
            get_summary_preview,
            start_thread_summary,
            get_latest_summary_run,
            get_summary_run,
            inspect_summary_evidence,
            cancel_summary_run,
            get_history_turns,
            get_history_items,
            locate_history_item,
            get_source_facts,
            get_source_evidence,
            validate_source_evidence,
            refresh_session_list,
            start_index_run,
            get_latest_index_run,
            get_index_run,
            cancel_index_run,
            get_project_catalog,
            get_project_sessions,
            query_project_threads,
            get_project_graph,
            get_project_workstreams,
            rename_workstream,
            restore_workstream_name,
            move_thread_to_workstream,
            restore_thread_workstream,
            decide_inferred_relation,
            get_candidate_preview,
            get_analysis_preview,
            start_project_analysis,
            get_analysis_run,
            get_latest_analysis_run,
            pause_analysis_run,
            cancel_analysis_run,
            continue_analysis_run,
            get_project_timeline,
            choose_project,
            choose_existing_project
        ])
        .build(tauri::generate_context!())
        .expect("无法启动 CodexFlow 桌面应用")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<AppState>();
                if let Ok(service) = &state.service {
                    tauri::async_runtime::block_on(service.shutdown());
                }
            }
        });
}
