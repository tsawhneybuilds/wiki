export type TreeNode = {
  id: string;
  name: string;
  path: string;
  kind: string;
  writable: boolean;
  source_id?: string | null;
  children: TreeNode[];
};

export type WorkspaceStats = {
  raw_documents: number;
  wiki_documents: number;
  system_documents: number;
  source_records: number;
  jobs: number;
  last_reindex_at?: string | null;
};

export type ProviderSettings = {
  id: string;
  label: string;
  kind: string;
  base_url?: string | null;
  model: string;
  configured: boolean;
  selected: boolean;
  supports_embeddings: boolean;
  available: boolean;
  auth_status: string;
  health_message?: string | null;
  executable_path?: string | null;
  supports_direct_edit: boolean;
};

export type WorkspaceSnapshot = {
  vault_root: string;
  tree: TreeNode[];
  stats: WorkspaceStats;
  providers: ProviderSettings[];
  jobs: JobRecord[];
  provider_mode: string;
  selected_subscription_provider?: string | null;
  selected_api_provider?: string | null;
  default_execution_mode: string;
};

export type LinkReference = {
  path: string;
  title: string;
};

export type CitationReference = {
  source_id: string;
  title: string;
  path: string;
  origin: string;
  status: string;
};

export type DocumentDetails = {
  path: string;
  title: string;
  scope: string;
  content: string;
  writable: boolean;
  source_id?: string | null;
  backlinks: LinkReference[];
  outgoing_links: LinkReference[];
  citations: CitationReference[];
};

export type SearchResult = {
  path: string;
  title: string;
  scope: string;
  excerpt: string;
  writable: boolean;
  source_id?: string | null;
};

export type DiffEntry = {
  path: string;
  before?: string | null;
  after?: string | null;
  diff: string;
};

export type JobRecord = {
  id: string;
  kind: string;
  status: string;
  created_at: string;
  updated_at: string;
  summary: string;
  source_ids: string[];
  changed_paths: string[];
  diffs: DiffEntry[];
  details: Record<string, unknown>;
};

export type ImportResult = {
  imported: number;
  updated: number;
  skipped: number;
  warnings: string[];
  source_ids: string[];
  job: JobRecord;
};

export type IngestRequest = {
  source_ids: string[];
  page_hint?: string | null;
  workflow_options?: WorkflowOptions;
};

export type QueryRequest = {
  prompt: string;
  save_as_page: boolean;
  workflow_options?: WorkflowOptions;
};

export type QueryAnswer = {
  answer: string;
  citations: CitationReference[];
  saved_page?: string | null;
  job?: JobRecord | null;
};

export type LintFinding = {
  severity: string;
  code: string;
  message: string;
  path?: string | null;
};

export type LintReport = {
  findings: LintFinding[];
  job: JobRecord;
};

export type LintRequest = {
  workflow_options?: WorkflowOptions;
};

export type SaveProviderRequest = {
  id: string;
  model: string;
  base_url?: string | null;
  api_key?: string;
  selected: boolean;
};

export type InboxSettings = {
  enabled: boolean;
  imap_host: string;
  imap_port: number;
  imap_username: string;
  imap_folder: string;
  allowed_senders: string[];
  sync_on_startup: boolean;
  password_configured: boolean;
  configured: boolean;
};

export type SaveInboxSettingsRequest = {
  enabled: boolean;
  imap_host: string;
  imap_port: number;
  imap_username: string;
  imap_folder: string;
  allowed_senders: string[];
  sync_on_startup: boolean;
  password?: string;
};

export type InboxSyncResult = {
  processed_messages: number;
  skipped_messages: number;
  imported_sources: number;
  warnings: string[];
  errors: string[];
  source_ids: string[];
  job: JobRecord;
};

export type WorkflowOptions = {
  provider_mode?: string | null;
  provider_id?: string | null;
  execution_mode?: string | null;
};

export type SaveAppSettingsRequest = {
  provider_mode?: string | null;
  default_execution_mode?: string | null;
};
