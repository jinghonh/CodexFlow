use codexflow_core::SourceService;
use codexflow_domain::{
    AppError, CandidatePreview, DisplayTheme, EvidenceCheck, EvidencePage, FactPage,
    HistoryCoverage, HistoryItemLocation, HistoryItemPage, HistoryTurnPage, IndexRun,
    JevConnectionResult, JevInferenceResult, JevStatus, ProjectCatalog, ProjectGraph,
    ProjectSessions, ProjectTimeline, SessionList, SourceStatus, SummaryEvidenceCheck,
    SummaryPreview, SummaryRun,
};
use serde::Serialize;
use std::sync::Arc;
use tauri::{Emitter, Manager};

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
fn start_index_run(
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
fn get_project_graph(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<ProjectGraph, AppError> {
    service(&state)?.project_graph(&project_id)
}

#[tauri::command]
fn get_candidate_preview(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<CandidatePreview, AppError> {
    service(&state)?.candidate_preview(&project_id)
}

#[tauri::command]
fn get_project_timeline(
    state: tauri::State<'_, AppState>,
    project_id: String,
) -> Result<ProjectTimeline, AppError> {
    service(&state)?.project_timeline(&project_id)
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
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            app.manage(AppState {
                service: SourceService::new(data_dir).map(Arc::new),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_source_status,
            connect_source,
            set_display_theme,
            get_jev_status,
            save_jev_settings,
            delete_jev_credential,
            check_jev_connection,
            test_jev_inference,
            cancel_jev_request,
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
            get_project_graph,
            get_candidate_preview,
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
