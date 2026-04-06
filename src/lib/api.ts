import { invoke } from "@tauri-apps/api/core";
import type {
  DocumentDetails,
  InboxSettings,
  InboxSyncResult,
  ImportResult,
  IngestRequest,
  JobRecord,
  LintRequest,
  LintReport,
  ProviderSettings,
  QueryAnswer,
  QueryRequest,
  SaveAppSettingsRequest,
  SaveInboxSettingsRequest,
  SaveProviderRequest,
  SearchResult,
  WorkspaceSnapshot,
} from "../types";

export function bootstrapWorkspace() {
  return invoke<WorkspaceSnapshot>("bootstrap_workspace");
}

export function refreshWorkspace() {
  return invoke<WorkspaceSnapshot>("refresh_workspace");
}

export function readDocument(path: string) {
  return invoke<DocumentDetails>("read_document", { path });
}

export function saveWikiDocument(path: string, content: string) {
  return invoke<DocumentDetails>("save_wiki_document", { path, content });
}

export function searchDocuments(query: string) {
  return invoke<SearchResult[]>("search_documents", { query });
}

export function importNotionZip(zipPath: string) {
  return invoke<ImportResult>("import_notion_zip", { zipPath });
}

export function importAppleNotes(accountFilter?: string) {
  return invoke<ImportResult>("import_apple_notes", {
    accountFilter: accountFilter || null,
  });
}

export function reindexWorkspace() {
  return invoke<WorkspaceSnapshot>("reindex_workspace");
}

export function proposeIngest(request: IngestRequest) {
  return invoke<JobRecord>("propose_ingest", { request });
}

export function applyJob(jobId: string) {
  return invoke<WorkspaceSnapshot>("apply_job", { jobId });
}

export function answerQuestion(request: QueryRequest) {
  return invoke<QueryAnswer>("answer_question", { request });
}

export function lintWiki(request: LintRequest) {
  return invoke<LintReport>("lint_wiki", { request });
}

export function getJob(jobId: string) {
  return invoke<JobRecord>("get_job", { jobId });
}

export function listProviderSettings() {
  return invoke<ProviderSettings[]>("list_provider_settings");
}

export function saveProviderConfig(request: SaveProviderRequest) {
  return invoke<ProviderSettings[]>("save_provider_config", { request });
}

export function saveAppSettings(request: SaveAppSettingsRequest) {
  return invoke<WorkspaceSnapshot>("save_app_settings", { request });
}

export function getInboxSettings() {
  return invoke<InboxSettings>("get_inbox_settings");
}

export function saveInboxSettings(request: SaveInboxSettingsRequest) {
  return invoke<InboxSettings>("save_inbox_settings", { request });
}

export function syncInbox() {
  return invoke<InboxSyncResult>("sync_inbox");
}
