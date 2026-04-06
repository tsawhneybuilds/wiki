use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub kind: String,
    pub writable: bool,
    pub source_id: Option<String>,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceStats {
    pub raw_documents: usize,
    pub wiki_documents: usize,
    pub system_documents: usize,
    pub source_records: usize,
    pub jobs: usize,
    pub last_reindex_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub model: String,
    pub configured: bool,
    pub selected: bool,
    pub supports_embeddings: bool,
    pub available: bool,
    pub auth_status: String,
    pub health_message: Option<String>,
    pub executable_path: Option<String>,
    pub supports_direct_edit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub vault_root: String,
    pub tree: Vec<TreeNode>,
    pub stats: WorkspaceStats,
    pub providers: Vec<ProviderSettings>,
    pub jobs: Vec<JobRecord>,
    pub provider_mode: String,
    pub selected_subscription_provider: Option<String>,
    pub selected_api_provider: Option<String>,
    pub default_execution_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkReference {
    pub path: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitationReference {
    pub source_id: String,
    pub title: String,
    pub path: String,
    pub origin: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentDetails {
    pub path: String,
    pub title: String,
    pub scope: String,
    pub content: String,
    pub writable: bool,
    pub source_id: Option<String>,
    pub backlinks: Vec<LinkReference>,
    pub outgoing_links: Vec<LinkReference>,
    pub citations: Vec<CitationReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub path: String,
    pub title: String,
    pub scope: String,
    pub excerpt: String,
    pub writable: bool,
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub source_id: String,
    pub origin: String,
    pub external_id: String,
    pub canonical_path: String,
    pub asset_root: Option<String>,
    pub checksum: String,
    pub imported_at: String,
    pub updated_at: String,
    pub tags: Vec<String>,
    pub status: String,
    pub title: String,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffEntry {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub diff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub summary: String,
    pub source_ids: Vec<String>,
    pub changed_paths: Vec<String>,
    pub diffs: Vec<DiffEntry>,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
    pub source_ids: Vec<String>,
    pub job: JobRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowOptions {
    pub provider_mode: Option<String>,
    pub provider_id: Option<String>,
    pub execution_mode: Option<String>,
}

impl Default for WorkflowOptions {
    fn default() -> Self {
        Self {
            provider_mode: None,
            provider_id: None,
            execution_mode: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRequest {
    pub source_ids: Vec<String>,
    pub page_hint: Option<String>,
    #[serde(default)]
    pub workflow_options: WorkflowOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    pub prompt: String,
    pub save_as_page: bool,
    #[serde(default)]
    pub workflow_options: WorkflowOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryAnswer {
    pub answer: String,
    pub citations: Vec<CitationReference>,
    pub saved_page: Option<String>,
    pub job: Option<JobRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintFinding {
    pub severity: String,
    pub code: String,
    pub message: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintReport {
    pub findings: Vec<LintFinding>,
    pub job: JobRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintRequest {
    #[serde(default)]
    pub workflow_options: WorkflowOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub model: String,
    pub supports_embeddings: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InboxConfig {
    pub enabled: bool,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_username: String,
    pub imap_folder: String,
    pub allowed_senders: Vec<String>,
    pub sync_on_startup: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxSettings {
    pub enabled: bool,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_username: String,
    pub imap_folder: String,
    pub allowed_senders: Vec<String>,
    pub sync_on_startup: bool,
    pub password_configured: bool,
    pub configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub selected_provider: Option<String>,
    pub provider_mode: String,
    pub selected_subscription_provider: Option<String>,
    pub selected_api_provider: Option<String>,
    pub default_execution_mode: String,
    pub inbox: InboxConfig,
    pub providers: Vec<ProviderConfig>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            selected_provider: None,
            provider_mode: "subscription".to_string(),
            selected_subscription_provider: Some("claude-code-cli".to_string()),
            selected_api_provider: Some("openai".to_string()),
            default_execution_mode: "review-first".to_string(),
            inbox: InboxConfig::default(),
            providers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveProviderRequest {
    pub id: String,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveInboxSettingsRequest {
    pub enabled: bool,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_username: String,
    pub imap_folder: String,
    pub allowed_senders: Vec<String>,
    pub sync_on_startup: bool,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxSyncResult {
    pub processed_messages: usize,
    pub skipped_messages: usize,
    pub imported_sources: usize,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub source_ids: Vec<String>,
    pub job: JobRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveAppSettingsRequest {
    pub provider_mode: Option<String>,
    pub default_execution_mode: Option<String>,
}
