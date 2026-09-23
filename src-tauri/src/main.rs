use codexflow_core::SourceService;
use codexflow_domain::{
    AppError, DisplayTheme, ProjectCatalog, ProjectSessions, SessionList, SourceStatus,
};
use serde::Serialize;
use tauri::Manager;

struct AppState {
    service: Result<SourceService, AppError>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    theme: DisplayTheme,
    source: SourceStatus,
}

fn service<'a>(state: &'a tauri::State<'_, AppState>) -> Result<&'a SourceService, AppError> {
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
fn get_session_list(state: tauri::State<'_, AppState>) -> Result<SessionList, AppError> {
    service(&state)?.cached_sessions()
}

#[tauri::command]
async fn refresh_session_list(state: tauri::State<'_, AppState>) -> Result<SessionList, AppError> {
    service(&state)?.refresh_sessions().await
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
                service: SourceService::new(data_dir),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            get_source_status,
            connect_source,
            set_display_theme,
            get_session_list,
            refresh_session_list,
            get_project_catalog,
            get_project_sessions,
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
