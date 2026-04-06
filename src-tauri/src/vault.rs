use crate::models::{AppSettings, InboxConfig, JobRecord, ProviderConfig, SourceRecord, TreeNode};
use crate::util::{
    ensure_parent_dir, now_iso, safe_relative_path, sanitize_segment, slugify, to_unix_string,
};
use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

const AGENTS_TEMPLATE: &str = r#"# AGENTS

This vault follows the `raw -> wiki -> schema` pattern.

## Page Taxonomy
- `wiki/people/` for people pages.
- `wiki/projects/` for active efforts, products, and long-running bodies of work.
- `wiki/topics/` for concepts, domains, and recurring themes.
- `wiki/timelines/` for time-oriented rollups or event histories.
- `wiki/analyses/` for one-off answers, synthesis pages, and saved query outputs.

## Naming Rules
- Prefer lowercase folder names and descriptive page titles.
- Use one topic per page.
- Link related pages with `[[wikilinks]]`.
- Keep raw sources immutable under `raw/`.

## Citation Format
- Every factual claim sourced from imported material should cite one or more raw records using `[@source_id]`.
- Prefer citing the narrowest raw source that supports the statement.

## Workflows
1. `Import` creates or refreshes raw sources and source-record manifests.
2. `Ingest` reads raw sources, proposes wiki page edits, and never writes automatically.
3. `Query` answers from the wiki first and only leans on raw material when citations are needed.
4. `Lint` checks for broken links, orphan pages, uncited claims, and weak cross-linking.
5. `Reindex` rebuilds the SQLite manifest from disk.

## Update Rules
- `system/index.md` is generated from the current wiki tree and refreshed after writes.
- `system/log.md` is append-only and records imports, ingest runs, query saves, and lint passes.
- Raw files are read-only in the UI.
"#;

const INDEX_TEMPLATE: &str = r#"# Wiki Index

The index is generated from the current `wiki/` tree after reindex or apply steps.
"#;

const LOG_TEMPLATE: &str = r#"# Activity Log

This log is append-only.
"#;

fn settings_template() -> AppSettings {
    AppSettings {
        selected_provider: Some("openai".to_string()),
        provider_mode: "subscription".to_string(),
        selected_subscription_provider: Some("claude-code-cli".to_string()),
        selected_api_provider: Some("openai".to_string()),
        default_execution_mode: "review-first".to_string(),
        inbox: InboxConfig {
            enabled: false,
            imap_host: String::new(),
            imap_port: 993,
            imap_username: String::new(),
            imap_folder: "INBOX".to_string(),
            allowed_senders: Vec::new(),
            sync_on_startup: true,
        },
        providers: vec![
            ProviderConfig {
                id: "claude-code-cli".to_string(),
                label: "Claude Code".to_string(),
                kind: "claude-code-cli".to_string(),
                base_url: None,
                model: "sonnet".to_string(),
                supports_embeddings: false,
            },
            ProviderConfig {
                id: "codex-cli".to_string(),
                label: "Codex".to_string(),
                kind: "codex-cli".to_string(),
                base_url: None,
                model: "gpt-5-codex".to_string(),
                supports_embeddings: false,
            },
            ProviderConfig {
                id: "openai".to_string(),
                label: "OpenAI".to_string(),
                kind: "openai".to_string(),
                base_url: Some("https://api.openai.com/v1".to_string()),
                model: "gpt-5.4-mini".to_string(),
                supports_embeddings: true,
            },
            ProviderConfig {
                id: "anthropic".to_string(),
                label: "Anthropic".to_string(),
                kind: "anthropic".to_string(),
                base_url: Some("https://api.anthropic.com/v1".to_string()),
                model: "claude-3-7-sonnet-latest".to_string(),
                supports_embeddings: false,
            },
            ProviderConfig {
                id: "openai-compatible".to_string(),
                label: "OpenAI-Compatible".to_string(),
                kind: "openai-compatible".to_string(),
                base_url: Some("http://localhost:11434/v1".to_string()),
                model: "llama3.1".to_string(),
                supports_embeddings: false,
            },
        ],
    }
}

pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

pub fn manifest_db_path(root: &Path) -> PathBuf {
    root.join(".wiki/manifest.db")
}

pub fn settings_path(root: &Path) -> PathBuf {
    root.join(".wiki/settings.json")
}

pub fn source_records_dir(root: &Path) -> PathBuf {
    root.join(".wiki/sources")
}

pub fn jobs_dir(root: &Path) -> PathBuf {
    root.join(".wiki/jobs")
}

pub fn inbox_state_path(root: &Path) -> PathBuf {
    root.join(".wiki/inbox_state.json")
}

pub fn ensure_layout(root: &Path) -> Result<()> {
    for relative in [
        "raw/notion",
        "raw/apple-notes",
        "raw/email",
        "raw/assets",
        "raw/assets/email",
        "wiki/analyses",
        "system",
        ".wiki",
        ".wiki/jobs",
        ".wiki/sources",
    ] {
        fs::create_dir_all(root.join(relative))
            .with_context(|| format!("failed to create {}", root.join(relative).display()))?;
    }

    seed_if_missing(&root.join("system/AGENTS.md"), AGENTS_TEMPLATE)?;
    seed_if_missing(&root.join("system/index.md"), INDEX_TEMPLATE)?;
    seed_if_missing(&root.join("system/log.md"), LOG_TEMPLATE)?;

    if !settings_path(root).exists() {
        save_settings(root, &settings_template())?;
    }

    Ok(())
}

fn seed_if_missing(path: &Path, content: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    ensure_parent_dir(path)?;
    fs::write(path, content).with_context(|| format!("failed to seed {}", path.display()))?;
    Ok(())
}

pub fn load_settings(root: &Path) -> Result<AppSettings> {
    ensure_layout(root)?;
    let raw = fs::read_to_string(settings_path(root))
        .with_context(|| format!("failed to read {}", settings_path(root).display()))?;
    let parsed: AppSettings = serde_json::from_str(&raw)?;
    Ok(normalize_settings(parsed))
}

pub fn save_settings(root: &Path, settings: &AppSettings) -> Result<()> {
    ensure_layout(root)?;
    let mut normalized = normalize_settings(settings.clone());
    normalized.selected_provider = normalized.selected_api_provider.clone();
    let serialized = serde_json::to_string_pretty(&normalized)?;
    fs::write(settings_path(root), serialized)?;
    Ok(())
}

pub fn read_document(root: &Path, relative_path: &str) -> Result<String> {
    let safe = safe_relative_path(relative_path)?;
    let allowed = ["raw", "wiki", "system"];
    match safe.components().next() {
        Some(component) => {
            let first = component.as_os_str().to_string_lossy();
            if !allowed.contains(&first.as_ref()) {
                return Err(anyhow!("path must live under raw/, wiki/, or system/"));
            }
        }
        None => return Err(anyhow!("empty path")),
    }
    let absolute = root.join(safe);
    Ok(fs::read_to_string(&absolute)
        .with_context(|| format!("failed to read {}", absolute.display()))?)
}

pub fn write_wiki_document(root: &Path, relative_path: &str, content: &str) -> Result<()> {
    let safe = safe_relative_path(relative_path)?;
    let first = safe
        .components()
        .next()
        .ok_or_else(|| anyhow!("empty path"))?
        .as_os_str()
        .to_string_lossy()
        .into_owned();

    if first != "wiki" {
        return Err(anyhow!("only wiki/ documents are writable"));
    }

    let absolute = root.join(&safe);
    ensure_parent_dir(&absolute)?;
    fs::write(&absolute, content)
        .with_context(|| format!("failed to write {}", absolute.display()))?;
    Ok(())
}

pub fn normalize_wiki_path(input: &str) -> String {
    let trimmed = input.trim().trim_start_matches('/');
    let without_prefix = trimmed.strip_prefix("wiki/").unwrap_or(trimmed);
    let with_extension = if without_prefix.ends_with(".md") {
        without_prefix.to_string()
    } else {
        format!("{without_prefix}.md")
    };

    let clean_segments = Path::new(&with_extension)
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .map(|segment| {
            if let Some((stem, ext)) = segment.rsplit_once('.') {
                let clean = sanitize_segment(stem);
                format!("{clean}.{ext}")
            } else {
                sanitize_segment(&segment)
            }
        })
        .collect::<Vec<_>>();

    format!("wiki/{}", clean_segments.join("/"))
}

pub fn save_source_record(root: &Path, record: &SourceRecord) -> Result<()> {
    ensure_layout(root)?;
    let file = source_records_dir(root).join(format!("{}.json", source_record_file_name(&record.source_id)));
    save_json(&file, record)
}

pub fn load_source_records(root: &Path) -> Result<Vec<SourceRecord>> {
    ensure_layout(root)?;
    let mut records: Vec<SourceRecord> = Vec::new();
    for entry in fs::read_dir(source_records_dir(root))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let raw = fs::read_to_string(entry.path())?;
            records.push(serde_json::from_str(&raw)?);
        }
    }
    records.sort_by(|left, right| left.title.cmp(&right.title));
    Ok(records)
}

pub fn source_record_map(root: &Path) -> Result<HashMap<String, SourceRecord>> {
    Ok(load_source_records(root)?
        .into_iter()
        .map(|record| (record.canonical_path.clone(), record))
        .collect())
}

pub fn save_job(root: &Path, job: &JobRecord) -> Result<()> {
    ensure_layout(root)?;
    let file = jobs_dir(root).join(format!("{}.json", slugify(&job.id)));
    save_json(&file, job)
}

pub fn load_job(root: &Path, job_id: &str) -> Result<JobRecord> {
    let file = jobs_dir(root).join(format!("{}.json", slugify(job_id)));
    let raw = fs::read_to_string(&file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn load_jobs(root: &Path) -> Result<Vec<JobRecord>> {
    ensure_layout(root)?;
    let mut jobs: Vec<JobRecord> = Vec::new();
    for entry in fs::read_dir(jobs_dir(root))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let raw = fs::read_to_string(entry.path())?;
            jobs.push(serde_json::from_str(&raw)?);
        }
    }
    jobs.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(jobs)
}

pub fn append_log_entry(root: &Path, job: &JobRecord) -> Result<()> {
    let log_path = root.join("system/log.md");
    let mut existing = fs::read_to_string(&log_path).unwrap_or_else(|_| LOG_TEMPLATE.to_string());
    if !existing.ends_with('\n') {
        existing.push('\n');
    }

    let changed = if job.changed_paths.is_empty() {
        "none".to_string()
    } else {
        job.changed_paths.join(", ")
    };

    existing.push_str(&format!(
        "\n## {}\n- id: `{}`\n- kind: `{}`\n- status: `{}`\n- summary: {}\n- sources: {}\n- changed_paths: {}\n",
        job.updated_at,
        job.id,
        job.kind,
        job.status,
        job.summary,
        if job.source_ids.is_empty() {
            "none".to_string()
        } else {
            job.source_ids.join(", ")
        },
        changed
    ));

    fs::write(log_path, existing)?;
    Ok(())
}

pub fn refresh_system_index(root: &Path) -> Result<()> {
    let wiki_root = root.join("wiki");
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    if wiki_root.exists() {
        for entry in walk_markdown(&wiki_root)? {
            let relative = entry.strip_prefix(root)?.to_path_buf();
            let relative_str = to_unix_string(&relative);
            let category = relative
                .components()
                .nth(1)
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_else(|| "misc".to_string());

            let stem = relative
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("untitled")
                .to_string();

            groups
                .entry(category)
                .or_default()
                .push(format!("- [[{}]] — `{}`", stem, relative_str));
        }
    }

    let mut sections = vec!["# Wiki Index".to_string(), String::new()];
    if groups.is_empty() {
        sections.push("No wiki pages yet.".to_string());
    } else {
        let mut keys = groups.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        for key in keys {
            sections.push(format!("## {key}"));
            if let Some(items) = groups.get(&key) {
                let mut sorted = items.clone();
                sorted.sort();
                sections.extend(sorted);
            }
            sections.push(String::new());
        }
    }

    fs::write(root.join("system/index.md"), sections.join("\n"))?;
    Ok(())
}

fn walk_markdown(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let path = entry.path();
            let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
            if matches!(extension, "md" | "markdown") {
                files.push(path.to_path_buf());
            }
        }
    }
    Ok(files)
}

pub fn build_tree(root: &Path, records: &HashMap<String, SourceRecord>) -> Result<Vec<TreeNode>> {
    let mut roots = Vec::new();
    for relative in ["raw", "wiki", "system"] {
        let absolute = root.join(relative);
        if absolute.exists() {
            roots.push(build_tree_node(root, &absolute, records)?);
        }
    }
    Ok(roots)
}

fn build_tree_node(root: &Path, path: &Path, records: &HashMap<String, SourceRecord>) -> Result<TreeNode> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative_str = to_unix_string(relative);
    let file_type = fs::metadata(path)?;
    let writable = relative_str.starts_with("wiki/");

    if file_type.is_dir() {
        let mut children = fs::read_dir(path)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                !name.starts_with('.')
            })
            .map(|entry| build_tree_node(root, &entry.path(), records))
            .collect::<Result<Vec<_>>>()?;

        children.sort_by(|left, right| {
            let left_dir = !left.children.is_empty() || left.kind == "directory";
            let right_dir = !right.children.is_empty() || right.kind == "directory";
            right_dir
                .cmp(&left_dir)
                .then(left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });

        return Ok(TreeNode {
            id: relative_str.clone(),
            name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("root")
                .to_string(),
            path: relative_str,
            kind: "directory".to_string(),
            writable,
            source_id: None,
            children,
        });
    }

    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let kind = match extension {
        "md" | "markdown" => "markdown",
        "csv" => "csv",
        "json" => "json",
        _ => "asset",
    }
    .to_string();

    Ok(TreeNode {
        id: relative_str.clone(),
        name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("untitled")
            .to_string(),
        path: relative_str.clone(),
        kind,
        writable,
        source_id: records.get(&relative_str).map(|record| record.source_id.clone()),
        children: Vec::new(),
    })
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    ensure_parent_dir(path)?;
    fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn source_record_file_name(source_id: &str) -> String {
    source_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

pub fn default_job(kind: &str, summary: &str) -> JobRecord {
    let now = now_iso();
    JobRecord {
        id: format!("{kind}-{}", slugify(&now)),
        kind: kind.to_string(),
        status: "completed".to_string(),
        created_at: now.clone(),
        updated_at: now,
        summary: summary.to_string(),
        source_ids: Vec::new(),
        changed_paths: Vec::new(),
        diffs: Vec::new(),
        details: serde_json::json!({}),
    }
}

fn normalize_settings(mut settings: AppSettings) -> AppSettings {
    let defaults = settings_template();
    if settings.providers.is_empty() {
        settings.providers = defaults.providers.clone();
    } else {
        for default_provider in defaults.providers {
            if !settings
                .providers
                .iter()
                .any(|provider| provider.id == default_provider.id)
            {
                settings.providers.push(default_provider);
            }
        }
    }

    if settings.provider_mode.trim().is_empty() {
        settings.provider_mode = defaults.provider_mode;
    }
    if settings.default_execution_mode.trim().is_empty() {
        settings.default_execution_mode = defaults.default_execution_mode;
    }
    if settings.selected_api_provider.is_none() {
        settings.selected_api_provider = settings
            .selected_provider
            .clone()
            .or(defaults.selected_api_provider.clone());
    }
    if settings.selected_subscription_provider.is_none() {
        settings.selected_subscription_provider = defaults.selected_subscription_provider;
    }
    if settings.inbox.imap_port == 0 {
        settings.inbox.imap_port = defaults.inbox.imap_port;
    }
    if settings.inbox.imap_folder.trim().is_empty() {
        settings.inbox.imap_folder = defaults.inbox.imap_folder;
    }
    settings.selected_provider = settings.selected_api_provider.clone();
    settings
}
