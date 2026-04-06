mod inbox;
mod importers;
mod index;
mod models;
mod providers;
mod util;
mod vault;
mod workflows;

use crate::models::{
    DocumentDetails, ImportResult, InboxSettings, InboxSyncResult, IngestRequest, JobRecord,
    LintReport, LintRequest, ProviderSettings, QueryAnswer, QueryRequest, SaveAppSettingsRequest,
    SaveInboxSettingsRequest, SaveProviderRequest, SearchResult, WorkspaceSnapshot,
};
use crate::vault::workspace_root;

fn snapshot() -> Result<WorkspaceSnapshot, String> {
    let root = workspace_root();
    vault::ensure_layout(&root).map_err(|error| error.to_string())?;
    let stats = index::workspace_stats(&root).map_err(|error| error.to_string())?;
    let records = vault::source_record_map(&root).map_err(|error| error.to_string())?;
    let tree = vault::build_tree(&root, &records).map_err(|error| error.to_string())?;
    let providers = providers::provider_statuses(&root).map_err(|error| error.to_string())?;
    let jobs = index::recent_jobs(&root).map_err(|error| error.to_string())?;
    let settings = vault::load_settings(&root).map_err(|error| error.to_string())?;

    Ok(WorkspaceSnapshot {
        vault_root: root.display().to_string(),
        tree,
        stats,
        providers,
        jobs,
        provider_mode: settings.provider_mode,
        selected_subscription_provider: settings.selected_subscription_provider,
        selected_api_provider: settings.selected_api_provider,
        default_execution_mode: settings.default_execution_mode,
    })
}

#[tauri::command]
fn bootstrap_workspace() -> Result<WorkspaceSnapshot, String> {
    let root = workspace_root();
    vault::ensure_layout(&root).map_err(|error| error.to_string())?;
    vault::refresh_system_index(&root).map_err(|error| error.to_string())?;
    index::reindex_workspace(&root).map_err(|error| error.to_string())?;
    snapshot()
}

#[tauri::command]
fn refresh_workspace() -> Result<WorkspaceSnapshot, String> {
    snapshot()
}

#[tauri::command]
fn read_document(path: String) -> Result<DocumentDetails, String> {
    index::read_document_details(&workspace_root(), &path).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_wiki_document(path: String, content: String) -> Result<DocumentDetails, String> {
    let root = workspace_root();
    vault::write_wiki_document(&root, &path, &content).map_err(|error| error.to_string())?;
    vault::refresh_system_index(&root).map_err(|error| error.to_string())?;
    index::reindex_workspace(&root).map_err(|error| error.to_string())?;
    index::read_document_details(&root, &path).map_err(|error| error.to_string())
}

#[tauri::command]
fn search_documents(query: String) -> Result<Vec<SearchResult>, String> {
    index::search_documents(&workspace_root(), &query).map_err(|error| error.to_string())
}

#[tauri::command]
fn import_notion_zip(zip_path: String) -> Result<ImportResult, String> {
    importers::import_notion_zip(&workspace_root(), &zip_path).map_err(|error| error.to_string())
}

#[tauri::command]
fn import_apple_notes(account_filter: Option<String>) -> Result<ImportResult, String> {
    importers::import_apple_notes(&workspace_root(), account_filter).map_err(|error| error.to_string())
}

#[tauri::command]
fn reindex_workspace() -> Result<WorkspaceSnapshot, String> {
    let root = workspace_root();
    index::reindex_workspace(&root).map_err(|error| error.to_string())?;
    snapshot()
}

#[tauri::command]
fn propose_ingest(request: IngestRequest) -> Result<JobRecord, String> {
    workflows::propose_ingest(&workspace_root(), request).map_err(|error| error.to_string())
}

#[tauri::command]
fn apply_job(job_id: String) -> Result<WorkspaceSnapshot, String> {
    workflows::apply_job(&workspace_root(), &job_id).map_err(|error| error.to_string())?;
    snapshot()
}

#[tauri::command]
fn answer_question(request: QueryRequest) -> Result<QueryAnswer, String> {
    workflows::answer_question(&workspace_root(), request).map_err(|error| error.to_string())
}

#[tauri::command]
fn lint_wiki(request: LintRequest) -> Result<LintReport, String> {
    workflows::lint_wiki(&workspace_root(), request).map_err(|error| error.to_string())
}

#[tauri::command]
fn get_job(job_id: String) -> Result<JobRecord, String> {
    vault::load_job(&workspace_root(), &job_id).map_err(|error| error.to_string())
}

#[tauri::command]
fn list_provider_settings() -> Result<Vec<ProviderSettings>, String> {
    providers::provider_statuses(&workspace_root()).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_provider_config(request: SaveProviderRequest) -> Result<Vec<ProviderSettings>, String> {
    providers::save_provider(&workspace_root(), request).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_app_settings(request: SaveAppSettingsRequest) -> Result<WorkspaceSnapshot, String> {
    let root = workspace_root();
    let mut settings = vault::load_settings(&root).map_err(|error| error.to_string())?;
    if let Some(provider_mode) = request.provider_mode {
        settings.provider_mode = provider_mode;
    }
    if let Some(default_execution_mode) = request.default_execution_mode {
        settings.default_execution_mode = default_execution_mode;
    }
    vault::save_settings(&root, &settings).map_err(|error| error.to_string())?;
    snapshot()
}

#[tauri::command]
fn get_inbox_settings() -> Result<InboxSettings, String> {
    inbox::get_settings(&workspace_root()).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_inbox_settings(request: SaveInboxSettingsRequest) -> Result<InboxSettings, String> {
    inbox::save_settings(&workspace_root(), request).map_err(|error| error.to_string())
}

#[tauri::command]
fn sync_inbox() -> Result<InboxSyncResult, String> {
    inbox::sync_inbox(&workspace_root()).map_err(|error| error.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            bootstrap_workspace,
            refresh_workspace,
            read_document,
            save_wiki_document,
            search_documents,
            import_notion_zip,
            import_apple_notes,
            reindex_workspace,
            propose_ingest,
            apply_job,
            answer_question,
            lint_wiki,
            get_job,
            list_provider_settings,
            save_provider_config,
            save_app_settings,
            get_inbox_settings,
            save_inbox_settings,
            sync_inbox
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
