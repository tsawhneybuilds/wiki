import {
  startTransition,
  useDeferredValue,
  useEffect,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { open } from "@tauri-apps/plugin-dialog";
import "./App.css";
import { EditorPane } from "./components/EditorPane";
import { InspectorPane } from "./components/InspectorPane";
import { Modal } from "./components/Modal";
import { Sidebar } from "./components/Sidebar";
import {
  answerQuestion,
  applyJob,
  bootstrapWorkspace,
  getInboxSettings,
  importAppleNotes,
  importNotionZip,
  lintWiki,
  listProviderSettings,
  proposeIngest,
  readDocument,
  refreshWorkspace,
  reindexWorkspace,
  saveInboxSettings,
  saveProviderConfig,
  saveWikiDocument,
  searchDocuments,
  syncInbox,
} from "./lib/api";
import type {
  DocumentDetails,
  InboxSettings,
  JobRecord,
  LintReport,
  ProviderSettings,
  QueryAnswer,
  SearchResult,
  WorkspaceSnapshot,
} from "./types";

type InboxDraft = InboxSettings & {
  password?: string;
  allowedSendersText: string;
};

function App() {
  const [snapshot, setSnapshot] = useState<WorkspaceSnapshot | null>(null);
  const [currentDocument, setCurrentDocument] = useState<DocumentDetails | null>(null);
  const [draftContent, setDraftContent] = useState("");
  const [selectedSourceIds, setSelectedSourceIds] = useState<string[]>([]);
  const [currentPath, setCurrentPath] = useState<string>();
  const [searchValue, setSearchValue] = useState("");
  const [searchResults, setSearchResults] = useState<SearchResult[]>([]);
  const [busyLabel, setBusyLabel] = useState("Bootstrapping workspace");
  const [statusMessage, setStatusMessage] = useState("Starting Tanush Wiki…");
  const [errorMessage, setErrorMessage] = useState("");

  const [showImportModal, setShowImportModal] = useState(false);
  const [showQueryModal, setShowQueryModal] = useState(false);
  const [showProvidersModal, setShowProvidersModal] = useState(false);
  const [jobModal, setJobModal] = useState<JobRecord | null>(null);
  const [queryAnswerModal, setQueryAnswerModal] = useState<QueryAnswer | null>(null);
  const [lintModal, setLintModal] = useState<LintReport | null>(null);

  const [notionZipPath, setNotionZipPath] = useState("");
  const [appleAccountFilter, setAppleAccountFilter] = useState("");
  const [queryPrompt, setQueryPrompt] = useState("");
  const [saveQueryAsPage, setSaveQueryAsPage] = useState(true);
  const [ingestPageHint, setIngestPageHint] = useState("");
  const [providerDrafts, setProviderDrafts] = useState<
    Record<string, ProviderSettings & { api_key?: string }>
  >({});
  const [inboxDraft, setInboxDraft] = useState<InboxDraft | null>(null);

  const deferredSearch = useDeferredValue(searchValue);
  const isDirty = currentDocument?.writable && currentDocument.content !== draftContent;
  const providerEntries = Object.values(providerDrafts);
  const subscriptionProviders = providerEntries.filter((provider) =>
    isSubscriptionProvider(provider.kind),
  );
  const apiProviders = providerEntries.filter(
    (provider) => !isSubscriptionProvider(provider.kind),
  );

  useEffect(() => {
    let cancelled = false;

    async function initialize() {
      try {
        const workspace = await bootstrapWorkspace();
        if (cancelled) {
          return;
        }

        setSnapshot(workspace);
        setBusyLabel("");
        setStatusMessage(`Vault ready at ${workspace.vault_root}`);
        const initialPath = firstDocumentPath(workspace);
        if (initialPath) {
          setCurrentPath(initialPath);
        }

        const inbox = await loadInboxSettings();
        if (cancelled || !inbox) {
          return;
        }

        if (inbox.enabled && inbox.sync_on_startup && inbox.configured) {
          await handleSyncInbox(true);
        }
      } catch (error) {
        if (!cancelled) {
          setBusyLabel("");
          setErrorMessage(String(error));
        }
      }
    }

    void initialize();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!currentPath) {
      return;
    }

    readDocument(currentPath)
      .then((document) => {
        setCurrentDocument(document);
        setDraftContent(document.content);
      })
      .catch((error) => {
        setErrorMessage(String(error));
      });
  }, [currentPath]);

  useEffect(() => {
    if (!deferredSearch.trim()) {
      setSearchResults([]);
      return;
    }

    searchDocuments(deferredSearch)
      .then(setSearchResults)
      .catch((error) => setErrorMessage(String(error)));
  }, [deferredSearch]);

  async function loadInboxSettings() {
    try {
      const settings = await getInboxSettings();
      setInboxDraft({
        ...settings,
        password: "",
        allowedSendersText: settings.allowed_senders.join("\n"),
      });
      return settings;
    } catch (error) {
      setErrorMessage(String(error));
      return null;
    }
  }

  async function refreshState() {
    const workspace = await refreshWorkspace();
    startTransition(() => {
      setSnapshot(workspace);
    });
  }

  async function handleSelectPath(path: string) {
    if (!snapshot) {
      return;
    }

    const resolved = resolveSelectedPath(snapshot, path);
    if (resolved) {
      setCurrentPath(resolved);
    }
  }

  async function handleSave() {
    if (!currentDocument?.writable) {
      return;
    }

    setBusyLabel("Saving page");
    setErrorMessage("");
    try {
      const saved = await saveWikiDocument(currentDocument.path, draftContent);
      setCurrentDocument(saved);
      setDraftContent(saved.content);
      await refreshState();
      setStatusMessage(`Saved ${saved.path}`);
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleImportNotion() {
    if (!notionZipPath) {
      setErrorMessage("Choose a Notion export ZIP first.");
      return;
    }

    setBusyLabel("Importing Notion export");
    setErrorMessage("");
    try {
      const result = await importNotionZip(notionZipPath);
      await refreshState();
      setSelectedSourceIds(result.source_ids);
      setStatusMessage(result.job.summary);
      setShowImportModal(false);
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleImportAppleNotes() {
    setBusyLabel("Importing Apple Notes");
    setErrorMessage("");
    try {
      const result = await importAppleNotes(appleAccountFilter);
      await refreshState();
      setSelectedSourceIds(result.source_ids);
      setStatusMessage(result.job.summary);
      setShowImportModal(false);
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handlePickZip() {
    const result = await open({
      filters: [{ name: "ZIP archive", extensions: ["zip"] }],
      multiple: false,
    });
    if (typeof result === "string") {
      setNotionZipPath(result);
    }
  }

  async function handleIngest() {
    if (!selectedSourceIds.length) {
      setErrorMessage("Select one or more raw sources from the sidebar first.");
      return;
    }

    setBusyLabel("Preparing ingest proposal");
    setErrorMessage("");
    try {
      const job = await proposeIngest({
        source_ids: selectedSourceIds,
        page_hint: ingestPageHint || null,
      });
      await refreshState();
      setJobModal(job);
      setStatusMessage(job.summary);
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleApplyJob(job: JobRecord) {
    setBusyLabel("Applying reviewed changes");
    setErrorMessage("");
    try {
      const workspace = await applyJob(job.id);
      setSnapshot(workspace);
      setJobModal(null);
      setStatusMessage(`Applied ${job.kind} changes`);
      if (job.changed_paths[0]) {
        setCurrentPath(job.changed_paths[0]);
      }
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleQuery() {
    if (!queryPrompt.trim()) {
      setErrorMessage("Enter a question first.");
      return;
    }

    setBusyLabel("Answering question");
    setErrorMessage("");
    try {
      const answer = await answerQuestion({
        prompt: queryPrompt,
        save_as_page: saveQueryAsPage,
      });
      await refreshState();
      setQueryAnswerModal(answer);
      setShowQueryModal(false);
      if (answer.saved_page) {
        setCurrentPath(answer.saved_page);
      }
      setStatusMessage("Query completed");
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleLint() {
    setBusyLabel("Linting wiki");
    setErrorMessage("");
    try {
      const report = await lintWiki({});
      await refreshState();
      setLintModal(report);
      setStatusMessage(report.job.summary);
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleReindex() {
    setBusyLabel("Reindexing vault");
    setErrorMessage("");
    try {
      const workspace = await reindexWorkspace();
      setSnapshot(workspace);
      setStatusMessage("Manifest rebuilt from disk");
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleOpenProviders() {
    try {
      const providers = await listProviderSettings();
      setProviderDrafts(
        Object.fromEntries(
          providers.map((provider) => [provider.id, { ...provider, api_key: "" }]),
        ),
      );
      setShowProvidersModal(true);
    } catch (error) {
      setErrorMessage(String(error));
    }
  }

  async function handleSaveProvider(providerId: string) {
    const provider = providerDrafts[providerId];
    if (!provider) {
      return;
    }

    setBusyLabel(`Saving ${provider.label}`);
    setErrorMessage("");
    try {
      const providers = await saveProviderConfig({
        id: provider.id,
        model: provider.model,
        base_url: provider.base_url || null,
        api_key: provider.api_key?.trim() ? provider.api_key : undefined,
        selected: provider.selected,
      });
      setProviderDrafts(
        Object.fromEntries(
          providers.map((entry) => [entry.id, { ...entry, api_key: "" }]),
        ),
      );
      await refreshState();
      setStatusMessage(`Saved provider settings for ${provider.label}`);
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleSaveInbox() {
    if (!inboxDraft) {
      return;
    }

    setBusyLabel("Saving inbox settings");
    setErrorMessage("");
    try {
      const saved = await saveInboxSettings({
        enabled: inboxDraft.enabled,
        imap_host: inboxDraft.imap_host,
        imap_port: Number(inboxDraft.imap_port) || 993,
        imap_username: inboxDraft.imap_username,
        imap_folder: inboxDraft.imap_folder,
        allowed_senders: parseAllowedSenders(inboxDraft.allowedSendersText),
        sync_on_startup: inboxDraft.sync_on_startup,
        password: inboxDraft.password?.trim() ? inboxDraft.password : undefined,
      });
      setInboxDraft({
        ...saved,
        password: "",
        allowedSendersText: saved.allowed_senders.join("\n"),
      });
      setStatusMessage("Saved inbox sync settings");
    } catch (error) {
      setErrorMessage(String(error));
    } finally {
      setBusyLabel("");
    }
  }

  async function handleSyncInbox(silent = false) {
    setBusyLabel("Syncing inbox");
    if (!silent) {
      setErrorMessage("");
    }

    try {
      const result = await syncInbox();
      await refreshState();
      if (result.source_ids.length) {
        setSelectedSourceIds(result.source_ids);
      }
      setStatusMessage(result.job.summary);
      if (result.errors.length) {
        setErrorMessage(result.errors.join(" "));
      } else if (!silent && result.warnings.length) {
        setErrorMessage(result.warnings.join(" "));
      } else if (!silent) {
        setErrorMessage("");
      }
      await loadInboxSettings();
      return result;
    } catch (error) {
      if (!silent) {
        setErrorMessage(String(error));
      }
      return null;
    } finally {
      setBusyLabel("");
    }
  }

  function handleToggleSource(sourceId: string) {
    setSelectedSourceIds((existing) =>
      existing.includes(sourceId)
        ? existing.filter((item) => item !== sourceId)
        : [...existing, sourceId],
    );
  }

  const linkTargets = collectLinkTargets(snapshot);

  return (
    <main className="app-shell">
      <header className="topbar">
        <div className="brand-block">
          <p className="eyebrow">Tanush Wiki</p>
          <h1>LLM-maintained personal knowledge system</h1>
        </div>
        <div className="topbar-actions">
          <button className="ghost-button" onClick={() => setShowImportModal(true)} type="button">
            Import
          </button>
          <button className="ghost-button" onClick={() => void handleSyncInbox()} type="button">
            Sync inbox
          </button>
          <button className="ghost-button" onClick={handleIngest} type="button">
            Ingest
          </button>
          <button className="ghost-button" onClick={() => setShowQueryModal(true)} type="button">
            Query
          </button>
          <button className="ghost-button" onClick={handleLint} type="button">
            Lint
          </button>
          <button className="ghost-button" onClick={handleReindex} type="button">
            Reindex
          </button>
          <button className="ghost-button" onClick={handleOpenProviders} type="button">
            Providers
          </button>
          <button
            className="primary-button"
            disabled={!isDirty}
            onClick={handleSave}
            type="button"
          >
            Save
          </button>
        </div>
      </header>

      <section className="status-strip">
        <div>
          <strong>{busyLabel || "Idle"}</strong>
          <span>{statusMessage}</span>
        </div>
        <div className="stat-cluster">
          <Stat label="Raw" value={snapshot?.stats.raw_documents ?? 0} />
          <Stat label="Wiki" value={snapshot?.stats.wiki_documents ?? 0} />
          <Stat label="Sources" value={snapshot?.stats.source_records ?? 0} />
          <Stat label="Selected" value={selectedSourceIds.length} />
        </div>
      </section>

      {errorMessage ? <div className="error-banner">{errorMessage}</div> : null}

      <section className="layout-grid">
        <Sidebar
          currentPath={currentPath}
          onSearchChange={setSearchValue}
          onSelectPath={handleSelectPath}
          onToggleSource={handleToggleSource}
          searchResults={searchResults}
          searchValue={searchValue}
          selectedSourceIds={selectedSourceIds}
          tree={snapshot?.tree ?? []}
        />
        <EditorPane
          document={currentDocument}
          draftContent={draftContent}
          linkTargets={linkTargets}
          onDraftChange={setDraftContent}
          onNavigate={handleSelectPath}
        />
        <InspectorPane
          document={currentDocument}
          jobs={snapshot?.jobs ?? []}
          onNavigate={handleSelectPath}
          onOpenJob={setJobModal}
        />
      </section>

      <Modal
        actions={
          <button className="ghost-button" onClick={() => setShowImportModal(false)} type="button">
            Close
          </button>
        }
        onClose={() => setShowImportModal(false)}
        open={showImportModal}
        title="Import raw sources"
      >
        <div className="modal-stack">
          <div className="workflow-card">
            <h4>Notion export ZIP</h4>
            <div className="inline-field">
              <input
                onChange={(event) => setNotionZipPath(event.currentTarget.value)}
                placeholder="/path/to/notion-export.zip"
                value={notionZipPath}
              />
              <button className="ghost-button" onClick={handlePickZip} type="button">
                Choose ZIP
              </button>
            </div>
            <button className="primary-button" onClick={handleImportNotion} type="button">
              Import Notion
            </button>
          </div>

          <div className="workflow-card">
            <h4>Apple Notes</h4>
            <input
              onChange={(event) => setAppleAccountFilter(event.currentTarget.value)}
              placeholder="Optional account filter, e.g. iCloud"
              value={appleAccountFilter}
            />
            <button className="primary-button" onClick={handleImportAppleNotes} type="button">
              Import Apple Notes
            </button>
          </div>

          <div className="workflow-card">
            <h4>Dedicated inbox sync</h4>
            {inboxDraft ? (
              <>
                <label className="checkbox-row">
                  <input
                    checked={inboxDraft.enabled}
                    onChange={(event) => {
                      const checked = event.currentTarget.checked;
                      setInboxDraft((existing) =>
                        existing
                          ? {
                              ...existing,
                              enabled: checked,
                            }
                          : existing,
                      );
                    }}
                    type="checkbox"
                  />
                  Enable inbox auto-ingest
                </label>
                <label className="checkbox-row">
                  <input
                    checked={inboxDraft.sync_on_startup}
                    onChange={(event) => {
                      const checked = event.currentTarget.checked;
                      setInboxDraft((existing) =>
                        existing
                          ? {
                              ...existing,
                              sync_on_startup: checked,
                            }
                          : existing,
                      );
                    }}
                    type="checkbox"
                  />
                  Sync on startup
                </label>
                <label>
                  <span>IMAP host</span>
                  <input
                    onChange={(event) => {
                      const value = event.currentTarget.value;
                      setInboxDraft((existing) =>
                        existing
                          ? {
                              ...existing,
                              imap_host: value,
                            }
                          : existing,
                      );
                    }}
                    placeholder="imap.gmail.com"
                    value={inboxDraft.imap_host}
                  />
                </label>
                <label>
                  <span>IMAP port</span>
                  <input
                    onChange={(event) => {
                      const value = Number(event.currentTarget.value) || 0;
                      setInboxDraft((existing) =>
                        existing
                          ? {
                              ...existing,
                              imap_port: value,
                            }
                          : existing,
                      );
                    }}
                    type="number"
                    value={inboxDraft.imap_port}
                  />
                </label>
                <label>
                  <span>IMAP username</span>
                  <input
                    onChange={(event) => {
                      const value = event.currentTarget.value;
                      setInboxDraft((existing) =>
                        existing
                          ? {
                              ...existing,
                              imap_username: value,
                            }
                          : existing,
                      );
                    }}
                    placeholder="your-dedicated-inbox@example.com"
                    value={inboxDraft.imap_username}
                  />
                </label>
                <label>
                  <span>IMAP password / app password</span>
                  <input
                    onChange={(event) => {
                      const value = event.currentTarget.value;
                      setInboxDraft((existing) =>
                        existing
                          ? {
                              ...existing,
                              password: value,
                            }
                          : existing,
                      );
                    }}
                    placeholder={
                      inboxDraft.password_configured
                        ? "Leave blank to keep current password"
                        : "Paste IMAP password"
                    }
                    type="password"
                    value={inboxDraft.password || ""}
                  />
                </label>
                <label>
                  <span>Mailbox folder</span>
                  <input
                    onChange={(event) => {
                      const value = event.currentTarget.value;
                      setInboxDraft((existing) =>
                        existing
                          ? {
                              ...existing,
                              imap_folder: value,
                            }
                          : existing,
                      );
                    }}
                    placeholder="INBOX"
                    value={inboxDraft.imap_folder}
                  />
                </label>
                <label>
                  <span>Allowed senders</span>
                  <textarea
                    className="query-textarea"
                    onChange={(event) => {
                      const value = event.currentTarget.value;
                      setInboxDraft((existing) =>
                        existing
                          ? {
                              ...existing,
                              allowedSendersText: value,
                            }
                          : existing,
                      );
                    }}
                    placeholder="one sender email per line"
                    value={inboxDraft.allowedSendersText}
                  />
                </label>
                <p className="eyebrow">
                  {inboxDraft.configured
                    ? "Inbox is configured and ready to sync."
                    : "Save host, username, folder, password, and at least one allowed sender."}
                </p>
                <div className="inline-field">
                  <button className="primary-button" onClick={() => void handleSaveInbox()} type="button">
                    Save inbox settings
                  </button>
                  <button className="ghost-button" onClick={() => void handleSyncInbox()} type="button">
                    Sync now
                  </button>
                </div>
              </>
            ) : (
              <button className="ghost-button" onClick={() => void loadInboxSettings()} type="button">
                Load inbox settings
              </button>
            )}
          </div>
        </div>
      </Modal>

      <Modal
        actions={
          <>
            <button className="ghost-button" onClick={() => setShowQueryModal(false)} type="button">
              Cancel
            </button>
            <button className="primary-button" onClick={handleQuery} type="button">
              Run query
            </button>
          </>
        }
        onClose={() => setShowQueryModal(false)}
        open={showQueryModal}
        title="Ask the wiki"
      >
        <div className="modal-stack">
          <textarea
            className="query-textarea"
            onChange={(event) => setQueryPrompt(event.currentTarget.value)}
            placeholder="What do you want to answer or synthesize?"
            value={queryPrompt}
          />
          <label className="checkbox-row">
            <input
              checked={saveQueryAsPage}
              onChange={(event) => setSaveQueryAsPage(event.currentTarget.checked)}
              type="checkbox"
            />
            Save result under `wiki/analyses/`
          </label>
        </div>
      </Modal>

      <Modal
        onClose={() => setShowProvidersModal(false)}
        open={showProvidersModal}
        title="Provider settings"
      >
        <div className="modal-stack">
          <div className="provider-section">
            <div className="provider-section-copy">
              <h4>Subscription agents</h4>
              <p className="mode-note">
                These use your local Claude Code or Codex login instead of API credits.
              </p>
            </div>
            {subscriptionProviders.map((provider) => (
              <ProviderCard
                key={provider.id}
                onSave={handleSaveProvider}
                onUpdate={setProviderDrafts}
                provider={provider}
              />
            ))}
          </div>
          <div className="provider-section">
            <div className="provider-section-copy">
              <h4>API providers</h4>
              <p className="mode-note">
                These use stored API keys and stay available as fallback providers.
              </p>
            </div>
            {apiProviders.map((provider) => (
              <ProviderCard
                key={provider.id}
                onSave={handleSaveProvider}
                onUpdate={setProviderDrafts}
                provider={provider}
              />
            ))}
          </div>
        </div>
      </Modal>

      <Modal
        actions={
          jobModal?.status === "pending_review" ? (
            <>
              <button className="ghost-button" onClick={() => setJobModal(null)} type="button">
                Keep reviewing
              </button>
              <button className="primary-button" onClick={() => jobModal && void handleApplyJob(jobModal)} type="button">
                Apply changes
              </button>
            </>
          ) : undefined
        }
        onClose={() => setJobModal(null)}
        open={Boolean(jobModal)}
        title={jobModal ? `${jobModal.kind} job` : "Job"}
      >
        {jobModal ? (
          <div className="modal-stack">
            <div className="workflow-card">
              <p className="eyebrow">{jobModal.status}</p>
              <h4>{jobModal.summary}</h4>
              <p>{jobModal.changed_paths.join(", ") || "No file changes"}</p>
            </div>
            {jobModal.diffs.map((diff) => (
              <div className="diff-card" key={diff.path}>
                <header>
                  <strong>{diff.path}</strong>
                </header>
                <pre>{diff.diff}</pre>
              </div>
            ))}
          </div>
        ) : null}
      </Modal>

      <Modal
        onClose={() => setQueryAnswerModal(null)}
        open={Boolean(queryAnswerModal)}
        title="Query answer"
      >
        {queryAnswerModal ? (
          <div className="modal-stack">
            <article className="response-card markdown-body">
              <p>{queryAnswerModal.answer}</p>
            </article>
            <div className="workflow-card">
              <h4>Citations</h4>
              {queryAnswerModal.citations.length ? (
                queryAnswerModal.citations.map((citation) => (
                  <button
                    className="plain-link"
                    key={citation.source_id}
                    onClick={() => void handleSelectPath(citation.path)}
                    type="button"
                  >
                    <strong>{citation.title}</strong>
                    <span>{citation.source_id}</span>
                  </button>
                ))
              ) : (
                <p>No raw-source citations in this answer.</p>
              )}
            </div>
          </div>
        ) : null}
      </Modal>

      <Modal onClose={() => setLintModal(null)} open={Boolean(lintModal)} title="Lint report">
        {lintModal ? (
          <div className="modal-stack">
            <div className="workflow-card">
              <h4>{lintModal.job.summary}</h4>
              <p>{lintModal.findings.length} findings</p>
            </div>
            {lintModal.findings.map((finding, index) => (
              <div className="workflow-card" key={`${finding.code}-${index}`}>
                <p className="lint-severity">{finding.severity}</p>
                <h4>{finding.code}</h4>
                <p>{finding.message}</p>
                {finding.path ? (
                  <button className="plain-link" onClick={() => void handleSelectPath(finding.path!)} type="button">
                    <span>{finding.path}</span>
                  </button>
                ) : null}
              </div>
            ))}
          </div>
        ) : null}
      </Modal>

      <section className="ingest-hint-bar">
        <input
          onChange={(event) => setIngestPageHint(event.currentTarget.value)}
          placeholder="Optional ingest hint, e.g. update people pages and project summaries"
          value={ingestPageHint}
        />
      </section>
    </main>
  );
}

function Stat({ label, value }: { label: string; value: number }) {
  return (
    <div className="stat-chip">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function parseAllowedSenders(value: string) {
  return value
    .split(/\r?\n|,/)
    .map((entry) => entry.trim().toLowerCase())
    .filter(Boolean);
}

function isSubscriptionProvider(kind: string) {
  return kind === "claude-code-cli" || kind === "codex-cli";
}

function ProviderCard({
  provider,
  onUpdate,
  onSave,
}: {
  provider: ProviderSettings & { api_key?: string };
  onUpdate: Dispatch<SetStateAction<Record<string, ProviderSettings & { api_key?: string }>>>;
  onSave: (providerId: string) => Promise<void>;
}) {
  const subscriptionProvider = isSubscriptionProvider(provider.kind);

  return (
    <div className="workflow-card">
      <div className="provider-header">
        <div>
          <h4>{provider.label}</h4>
          <p className="eyebrow">
            {provider.kind} · {provider.available ? "available" : "unavailable"} ·{" "}
            {provider.auth_status}
          </p>
        </div>
        <label className="checkbox-row">
          <input
            checked={provider.selected}
            onChange={(event) => {
              const checked = event.currentTarget.checked;
              onUpdate((existing) => {
                const next: typeof existing = {};
                for (const [key, value] of Object.entries(existing)) {
                  const sameGroup =
                    isSubscriptionProvider(value.kind) === subscriptionProvider;
                  next[key] = {
                    ...value,
                    selected:
                      sameGroup && checked
                        ? key === provider.id
                        : key === provider.id
                          ? checked
                          : value.selected,
                  };
                }
                return next;
              });
            }}
            type="checkbox"
          />
          Active
        </label>
      </div>
      <p className="provider-health">
        <strong>{provider.available ? "Available" : "Unavailable"}</strong>
        <span>{provider.auth_status}</span>
      </p>
      {provider.health_message ? <p className="mode-note">{provider.health_message}</p> : null}
      {provider.executable_path ? (
        <p className="mode-note">{provider.executable_path}</p>
      ) : null}
      <label>
        <span>Model</span>
        <input
          onChange={(event) => {
            const value = event.currentTarget.value;
            onUpdate((existing) => ({
              ...existing,
              [provider.id]: {
                ...existing[provider.id],
                model: value,
              },
            }));
          }}
          value={provider.model}
        />
      </label>
      {subscriptionProvider ? null : (
        <>
          <label>
            <span>Base URL</span>
            <input
              onChange={(event) => {
                const value = event.currentTarget.value;
                onUpdate((existing) => ({
                  ...existing,
                  [provider.id]: {
                    ...existing[provider.id],
                    base_url: value,
                  },
                }));
              }}
              value={provider.base_url || ""}
            />
          </label>
          <label>
            <span>API key</span>
            <input
              onChange={(event) => {
                const value = event.currentTarget.value;
                onUpdate((existing) => ({
                  ...existing,
                  [provider.id]: {
                    ...existing[provider.id],
                    api_key: value,
                  },
                }));
              }}
              placeholder={
                provider.configured ? "Leave blank to keep current key" : "Paste API key"
              }
              type="password"
              value={provider.api_key || ""}
            />
          </label>
        </>
      )}
      <button
        className="primary-button"
        disabled={!provider.available && subscriptionProvider}
        onClick={() => void onSave(provider.id)}
        type="button"
      >
        Save provider
      </button>
    </div>
  );
}

function collectLinkTargets(snapshot: WorkspaceSnapshot | null) {
  if (!snapshot) {
    return [];
  }

  const targets: string[] = [];
  const visit = (nodes: typeof snapshot.tree) => {
    for (const node of nodes) {
      if (node.kind !== "directory" && node.path.endsWith(".md")) {
        targets.push(node.path.replace(/^wiki\//, "").replace(/\.md$/, ""));
      }
      if (node.children.length) {
        visit(node.children);
      }
    }
  };
  visit(snapshot.tree);
  return targets.sort();
}

function firstDocumentPath(snapshot: WorkspaceSnapshot) {
  const queue = [...snapshot.tree];
  while (queue.length) {
    const node = queue.shift()!;
    if (node.kind !== "directory") {
      return node.path;
    }
    queue.unshift(...node.children);
  }
  return undefined;
}

function resolveSelectedPath(snapshot: WorkspaceSnapshot, rawTarget: string) {
  if (
    rawTarget.startsWith("raw/") ||
    rawTarget.startsWith("wiki/") ||
    rawTarget.startsWith("system/")
  ) {
    return rawTarget;
  }

  const normalized = rawTarget.replace(/\.md$/, "");
  let found: string | undefined;
  const visit = (nodes: typeof snapshot.tree) => {
    for (const node of nodes) {
      if (node.kind !== "directory") {
        const candidate = node.path.replace(/\.md$/, "").replace(/^wiki\//, "");
        if (candidate.toLowerCase() === normalized.toLowerCase()) {
          found = node.path;
          return;
        }
      }
      if (node.children.length) {
        visit(node.children);
      }
    }
  };
  visit(snapshot.tree);
  return found ?? rawTarget;
}

export default App;
