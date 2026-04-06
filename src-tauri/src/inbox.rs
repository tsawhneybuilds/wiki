use crate::models::{
    InboxConfig, InboxSettings, InboxSyncResult, IngestRequest, SaveInboxSettingsRequest,
    SourceRecord, WorkflowOptions,
};
use crate::util::{ensure_parent_dir, now_iso, sanitize_segment, sha256_hex, slugify, to_unix_string};
use crate::{index, vault, workflows};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use imap::ClientBuilder;
use keyring::{Entry, Error as KeyringError};
use mailparse::{MailAddr, MailAddrList, MailHeaderMap, ParsedMail, addrparse_header, parse_mail};
use regex::Regex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const KEYCHAIN_SERVICE: &str = "com.tanush.wiki";
const INBOX_SECRET_ACCOUNT: &str = "inbox-imap-password";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InboxState {
    mailboxes: HashMap<String, MailboxState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct MailboxState {
    uid_validity: Option<u32>,
    processed_uids: Vec<u32>,
}

#[derive(Debug, Default)]
struct SyncAccumulator {
    processed_messages: usize,
    skipped_messages: usize,
    imported_sources: usize,
    warnings: Vec<String>,
    errors: Vec<String>,
    source_ids: Vec<String>,
    changed_paths: Vec<String>,
}

#[derive(Debug)]
struct MessageContext {
    uid: u32,
    subject: String,
    sender_addresses: Vec<String>,
    sender_display: String,
    message_id: Option<String>,
    received_at: DateTime<Utc>,
    body_markdown: String,
    body_urls: Vec<String>,
    folder_slug: String,
    mailbox_key: String,
}

#[derive(Debug)]
struct ImportedMessage {
    source_ids: Vec<String>,
    changed_paths: Vec<String>,
}

#[derive(Debug)]
struct SavedSource {
    source_id: String,
    changed_paths: Vec<String>,
}

pub fn get_settings(root: &Path) -> Result<InboxSettings> {
    let settings = vault::load_settings(root)?;
    settings_view(&settings.inbox)
}

pub fn save_settings(root: &Path, request: SaveInboxSettingsRequest) -> Result<InboxSettings> {
    let mut settings = vault::load_settings(root)?;
    settings.inbox = normalize_inbox_config(InboxConfig {
        enabled: request.enabled,
        imap_host: request.imap_host,
        imap_port: request.imap_port,
        imap_username: request.imap_username,
        imap_folder: request.imap_folder,
        allowed_senders: request.allowed_senders,
        sync_on_startup: request.sync_on_startup,
    });

    if let Some(password) = request.password.as_ref() {
        let entry = Entry::new(KEYCHAIN_SERVICE, INBOX_SECRET_ACCOUNT)?;
        if password.trim().is_empty() {
            let _ = entry.delete_password();
        } else {
            entry
                .set_password(password.trim())
                .context("failed to save inbox IMAP password")?;
        }
    }

    vault::save_settings(root, &settings)?;
    settings_view(&settings.inbox)
}

pub fn sync_inbox(root: &Path) -> Result<InboxSyncResult> {
    let settings = vault::load_settings(root)?;
    let config = normalize_inbox_config(settings.inbox);
    let mut job = vault::default_job("sync-inbox", "Inbox sync completed");
    let mut accumulator = SyncAccumulator::default();

    if !config.enabled {
        job.summary = "Inbox sync skipped because it is disabled.".to_string();
        job.details = json!({ "skipped_reason": "disabled" });
        vault::save_job(root, &job)?;
        vault::append_log_entry(root, &job)?;
        index::reindex_workspace(root)?;
        return Ok(InboxSyncResult {
            processed_messages: 0,
            skipped_messages: 0,
            imported_sources: 0,
            warnings: Vec::new(),
            errors: Vec::new(),
            source_ids: Vec::new(),
            job,
        });
    }

    let mailbox_key = mailbox_key(&config);
    let password = match read_secret()? {
        Some(secret) => secret,
        None => {
            let error = "Inbox sync is not configured: save an IMAP password first.".to_string();
            accumulator.errors.push(error.clone());
            job.status = "failed".to_string();
            job.summary = error;
            job.details = json!({ "errors": accumulator.errors });
            vault::save_job(root, &job)?;
            vault::append_log_entry(root, &job)?;
            index::reindex_workspace(root)?;
            return Ok(InboxSyncResult {
                processed_messages: 0,
                skipped_messages: 0,
                imported_sources: 0,
                warnings: Vec::new(),
                errors: accumulator.errors,
                source_ids: Vec::new(),
                job,
            });
        }
    };

    if !is_config_complete(&config, true) {
        let error = "Inbox sync is not configured: IMAP host, username, folder, allowed senders, and password are required.".to_string();
        accumulator.errors.push(error.clone());
        job.status = "failed".to_string();
        job.summary = error;
        job.details = json!({ "errors": accumulator.errors });
        vault::save_job(root, &job)?;
        vault::append_log_entry(root, &job)?;
        index::reindex_workspace(root)?;
        return Ok(InboxSyncResult {
            processed_messages: 0,
            skipped_messages: 0,
            imported_sources: 0,
            warnings: Vec::new(),
            errors: accumulator.errors,
            source_ids: Vec::new(),
            job,
        });
    }

    let mut state = load_state(root)?;
    let run_result = run_sync(root, &config, &mailbox_key, &password, &mut state, &mut accumulator);
    if run_result.is_ok() {
        save_state(root, &state)?;
    } else if let Err(error) = run_result {
        accumulator.errors.push(error.to_string());
    }

    job.source_ids = accumulator.source_ids.clone();
    job.changed_paths = dedupe(accumulator.changed_paths.clone());
    job.status = if !accumulator.errors.is_empty() {
        "failed".to_string()
    } else if !accumulator.warnings.is_empty() {
        "warning".to_string()
    } else {
        "completed".to_string()
    };
    job.summary = format!(
        "Inbox sync processed {} messages, imported {} sources, skipped {}",
        accumulator.processed_messages, accumulator.imported_sources, accumulator.skipped_messages
    );
    job.details = json!({
        "warnings": accumulator.warnings,
        "errors": accumulator.errors,
        "mailbox": {
            "host": config.imap_host,
            "folder": config.imap_folder,
            "username": config.imap_username,
        }
    });

    vault::save_job(root, &job)?;
    vault::append_log_entry(root, &job)?;
    index::reindex_workspace(root)?;

    Ok(InboxSyncResult {
        processed_messages: accumulator.processed_messages,
        skipped_messages: accumulator.skipped_messages,
        imported_sources: accumulator.imported_sources,
        warnings: accumulator.warnings,
        errors: accumulator.errors,
        source_ids: accumulator.source_ids,
        job,
    })
}

fn run_sync(
    root: &Path,
    config: &InboxConfig,
    mailbox_key: &str,
    password: &str,
    state: &mut InboxState,
    accumulator: &mut SyncAccumulator,
) -> Result<()> {
    let client = ClientBuilder::new(&config.imap_host, config.imap_port)
        .connect()
        .with_context(|| format!("failed to connect to {}:{}", config.imap_host, config.imap_port))?;
    let mut session = client
        .login(&config.imap_username, password)
        .map_err(|error| error.0)
        .with_context(|| format!("failed to authenticate to {}", config.imap_host))?;

    let select_result = session
        .select(&config.imap_folder)
        .with_context(|| format!("failed to select mailbox {}", config.imap_folder));
    let mailbox = match select_result {
        Ok(mailbox) => mailbox,
        Err(error) => {
            let _ = session.logout();
            return Err(error);
        }
    };

    let mailbox_state = state
        .mailboxes
        .entry(mailbox_key.to_string())
        .or_default();
    if mailbox_state.uid_validity != mailbox.uid_validity {
        mailbox_state.uid_validity = mailbox.uid_validity;
        mailbox_state.processed_uids.clear();
    }

    let processed: BTreeSet<u32> = mailbox_state.processed_uids.iter().copied().collect();
    let mut uids = session
        .uid_search("ALL")
        .context("failed to enumerate inbox messages")?
        .into_iter()
        .collect::<Vec<_>>();
    uids.sort_unstable();

    let allowlist = config
        .allowed_senders
        .iter()
        .map(|sender| sender.trim().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();

    for uid in uids {
        let uid_u32 = uid as u32;
        if processed.contains(&uid_u32) {
            accumulator.skipped_messages += 1;
            continue;
        }

        let fetches = session
            .uid_fetch(uid.to_string(), "RFC822 UID")
            .with_context(|| format!("failed to fetch message UID {}", uid_u32))?;
        let Some(fetch) = fetches.iter().next() else {
            accumulator.skipped_messages += 1;
            continue;
        };
        let Some(body) = fetch.body() else {
            accumulator.skipped_messages += 1;
            accumulator.warnings.push(format!("Message UID {} had no RFC822 body.", uid_u32));
            continue;
        };

        let parsed = parse_mail(body).with_context(|| format!("failed to parse message UID {}", uid_u32))?;
        let context = build_message_context(mailbox_key, uid_u32, &parsed)?;
        if !sender_allowed(&context.sender_addresses, &allowlist) {
            accumulator.skipped_messages += 1;
            accumulator
                .warnings
                .push(format!("Skipped message `{}` from non-allowlisted sender {}.", context.subject, context.sender_display));
            continue;
        }

        match import_and_apply_message(root, &parsed, &context) {
            Ok(imported) => {
                accumulator.processed_messages += 1;
                accumulator.imported_sources += imported.source_ids.len();
                accumulator.source_ids.extend(imported.source_ids);
                accumulator.changed_paths.extend(imported.changed_paths);
                mailbox_state.processed_uids.push(uid_u32);
            }
            Err(error) => {
                accumulator.errors.push(format!("`{}`: {}", context.subject, error));
            }
        }
    }

    mailbox_state.processed_uids.sort_unstable();
    mailbox_state.processed_uids.dedup();
    let _ = session.logout();
    Ok(())
}

fn import_and_apply_message(root: &Path, parsed: &ParsedMail<'_>, context: &MessageContext) -> Result<ImportedMessage> {
    let raw_root = raw_message_root(root, context);
    let asset_root = asset_message_root(root, context);
    fs::create_dir_all(&raw_root)?;
    fs::create_dir_all(&asset_root)?;

    let mut source_ids = Vec::new();
    let mut changed_paths = Vec::new();

    let message_source = save_message_source(root, context, &raw_root)?;
    source_ids.push(message_source.source_id.clone());
    changed_paths.extend(message_source.changed_paths);

    let mut attachment_counter = 0usize;
    for part in parsed.parts() {
        let disposition = part.get_content_disposition();
        let filename = attachment_filename(part, &disposition);
        if filename.is_none() {
            continue;
        }
        let filename = filename.unwrap();
        let mime = part.ctype.mimetype.to_ascii_lowercase();
        let lower_name = filename.to_ascii_lowercase();

        if is_markdown_attachment(&mime, &lower_name) {
            attachment_counter += 1;
            let saved = save_markdown_attachment(root, context, &raw_root, part, &filename, attachment_counter)?;
            source_ids.push(saved.source_id.clone());
            changed_paths.extend(saved.changed_paths);
            continue;
        }

        if mime == "application/pdf" || lower_name.ends_with(".pdf") {
            attachment_counter += 1;
            let saved = save_pdf_attachment(root, context, &raw_root, &asset_root, part, &filename, attachment_counter)?;
            source_ids.push(saved.source_id.clone());
            changed_paths.extend(saved.changed_paths);
        }
    }

    for (index, url) in context.body_urls.iter().enumerate() {
        let saved = save_url_source(root, context, &raw_root, &asset_root, url, index + 1)?;
        source_ids.push(saved.source_id.clone());
        changed_paths.extend(saved.changed_paths);
    }

    let ingest_request = IngestRequest {
        source_ids: source_ids.clone(),
        page_hint: Some(context.subject.clone()),
        workflow_options: WorkflowOptions::default(),
    };
    let ingest_job = workflows::propose_ingest(root, ingest_request)
        .with_context(|| format!("failed to propose ingest for `{}`", context.subject))?;
    let applied_job = workflows::apply_job(root, &ingest_job.id)
        .with_context(|| format!("failed to auto-apply ingest for `{}`", context.subject))?;
    changed_paths.extend(applied_job.changed_paths);

    Ok(ImportedMessage {
        source_ids: dedupe(source_ids),
        changed_paths: dedupe(changed_paths),
    })
}

fn save_message_source(root: &Path, context: &MessageContext, raw_root: &Path) -> Result<SavedSource> {
    let path = raw_root.join("message.md");
    let urls_section = if context.body_urls.is_empty() {
        String::new()
    } else {
        format!(
            "\n## URLs In Body\n{}\n",
            context
                .body_urls
                .iter()
                .map(|url| format!("- {}", url))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let body = format!(
        "---\ntitle: {}\norigin: email\nsender: {}\nreceived_at: {}\nmessage_id: {}\nuid: {}\nmailbox_key: {}\n---\n\n# {}\n\nFrom: {}\n\nReceived: {}\n{}\n## Body\n\n{}\n",
        yaml_escape(&context.subject),
        yaml_escape(&context.sender_display),
        context.received_at.to_rfc3339(),
        yaml_escape(context.message_id.as_deref().unwrap_or("")),
        context.uid,
        yaml_escape(&context.mailbox_key),
        context.subject,
        context.sender_display,
        context.received_at.to_rfc3339(),
        urls_section,
        context.body_markdown
    );

    let write = write_text_if_changed(&path, &body)?;
    let source_id = format!("email:{}:{}:message", short_hash(&context.mailbox_key), context.uid);
    let record = SourceRecord {
        source_id: source_id.clone(),
        origin: "email".to_string(),
        external_id: context
            .message_id
            .clone()
            .unwrap_or_else(|| format!("uid-{}", context.uid)),
        canonical_path: to_unix_string(path.strip_prefix(root)?),
        asset_root: Some(to_unix_string(asset_message_root(root, context).strip_prefix(root)?)),
        checksum: write.checksum,
        imported_at: context.received_at.to_rfc3339(),
        updated_at: now_iso(),
        tags: vec!["email".to_string()],
        status: "imported".to_string(),
        title: context.subject.clone(),
        metadata: json!({
            "mailbox_key": context.mailbox_key,
            "uid": context.uid,
            "sender_addresses": context.sender_addresses,
            "message_id": context.message_id,
            "body_urls": context.body_urls,
        }),
    };
    vault::save_source_record(root, &record)?;
    Ok(SavedSource {
        source_id,
        changed_paths: vec![to_unix_string(path.strip_prefix(root)?)],
    })
}

fn save_markdown_attachment(
    root: &Path,
    context: &MessageContext,
    raw_root: &Path,
    part: &ParsedMail<'_>,
    filename: &str,
    index: usize,
) -> Result<SavedSource> {
    let attachment_name = sanitize_segment(filename);
    let target_name = if attachment_name.to_ascii_lowercase().ends_with(".md")
        || attachment_name.to_ascii_lowercase().ends_with(".markdown")
    {
        attachment_name
    } else {
        format!("{attachment_name}.md")
    };
    let path = raw_root.join("attachments").join(target_name);
    let markdown = part
        .get_body()
        .with_context(|| format!("failed to decode markdown attachment {}", filename))?;
    let write = write_text_if_changed(&path, &markdown)?;
    let source_id = format!(
        "email:{}:{}:attachment:{}",
        short_hash(&context.mailbox_key),
        context.uid,
        slugify(&format!("{}-{index}", filename))
    );
    let record = SourceRecord {
        source_id: source_id.clone(),
        origin: "email-markdown".to_string(),
        external_id: format!("{}#{}", context.uid, filename),
        canonical_path: to_unix_string(path.strip_prefix(root)?),
        asset_root: None,
        checksum: write.checksum,
        imported_at: context.received_at.to_rfc3339(),
        updated_at: now_iso(),
        tags: vec!["email".to_string(), "markdown-attachment".to_string()],
        status: "imported".to_string(),
        title: filename.to_string(),
        metadata: json!({
            "parent_uid": context.uid,
            "parent_message_id": context.message_id,
            "filename": filename,
            "sender": context.sender_display,
        }),
    };
    vault::save_source_record(root, &record)?;
    Ok(SavedSource {
        source_id,
        changed_paths: vec![to_unix_string(path.strip_prefix(root)?)],
    })
}

fn save_pdf_attachment(
    root: &Path,
    context: &MessageContext,
    raw_root: &Path,
    asset_root: &Path,
    part: &ParsedMail<'_>,
    filename: &str,
    index: usize,
) -> Result<SavedSource> {
    let bytes = part
        .get_body_raw()
        .with_context(|| format!("failed to decode PDF attachment {}", filename))?;
    let asset_name = ensure_pdf_extension(filename);
    let original_path = asset_root.join("attachments").join(&asset_name);
    let original_write = write_bytes_if_changed(&original_path, &bytes)?;
    let extracted = pdf_extract::extract_text_from_mem(&bytes)
        .with_context(|| format!("failed to extract text from PDF attachment {}", filename))?;
    if extracted.trim().is_empty() {
        return Err(anyhow!("PDF attachment {} contained no extractable text", filename));
    }

    let canonical_name = format!("{}.md", sanitize_segment(filename.trim_end_matches(".pdf")));
    let canonical_path = raw_root.join("attachments").join(canonical_name);
    let markdown = format!(
        "---\ntitle: {}\norigin: email-pdf\nsource_file: {}\nreceived_at: {}\n---\n\n# {}\n\n{}\n",
        yaml_escape(filename),
        yaml_escape(&asset_name),
        context.received_at.to_rfc3339(),
        filename,
        extracted.trim()
    );
    let canonical_write = write_text_if_changed(&canonical_path, &markdown)?;
    let source_id = format!(
        "email:{}:{}:attachment:{}",
        short_hash(&context.mailbox_key),
        context.uid,
        slugify(&format!("{}-{index}", filename))
    );
    let record = SourceRecord {
        source_id: source_id.clone(),
        origin: "email-pdf".to_string(),
        external_id: format!("{}#{}", context.uid, filename),
        canonical_path: to_unix_string(canonical_path.strip_prefix(root)?),
        asset_root: Some(to_unix_string(original_path.parent().unwrap_or(asset_root).strip_prefix(root)?)),
        checksum: canonical_write.checksum,
        imported_at: context.received_at.to_rfc3339(),
        updated_at: now_iso(),
        tags: vec!["email".to_string(), "pdf-attachment".to_string()],
        status: "imported".to_string(),
        title: filename.to_string(),
        metadata: json!({
            "parent_uid": context.uid,
            "parent_message_id": context.message_id,
            "filename": filename,
            "original_asset": to_unix_string(original_path.strip_prefix(root)?),
            "original_checksum": original_write.checksum,
        }),
    };
    vault::save_source_record(root, &record)?;
    Ok(SavedSource {
        source_id,
        changed_paths: vec![
            to_unix_string(original_path.strip_prefix(root)?),
            to_unix_string(canonical_path.strip_prefix(root)?),
        ],
    })
}

fn save_url_source(
    root: &Path,
    context: &MessageContext,
    raw_root: &Path,
    asset_root: &Path,
    url: &str,
    index: usize,
) -> Result<SavedSource> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()?;
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("failed to fetch URL {}", url))?
        .error_for_status()
        .with_context(|| format!("failed to fetch URL {}", url))?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let fetched_at = now_iso();

    if content_type.contains("application/pdf") || url.to_ascii_lowercase().ends_with(".pdf") {
        let bytes = response.bytes().context("failed to read fetched PDF body")?;
        let file_name = format!("link-{:02}.pdf", index);
        let original_path = asset_root.join("links").join(&file_name);
        let original_write = write_bytes_if_changed(&original_path, bytes.as_ref())?;
        let extracted = pdf_extract::extract_text_from_mem(bytes.as_ref())
            .with_context(|| format!("failed to extract text from linked PDF {}", url))?;
        if extracted.trim().is_empty() {
            return Err(anyhow!("linked PDF {} contained no extractable text", url));
        }
        let canonical_path = raw_root.join("links").join(format!("link-{:02}.md", index));
        let markdown = format!(
            "---\ntitle: {}\norigin: email-url-pdf\nsource_url: {}\nfetched_at: {}\n---\n\n# Linked PDF {}\n\nSource URL: {}\n\n{}\n",
            yaml_escape(url),
            yaml_escape(url),
            fetched_at,
            index,
            url,
            extracted.trim()
        );
        let canonical_write = write_text_if_changed(&canonical_path, &markdown)?;
        let source_id = format!(
            "email:{}:{}:url:{}",
            short_hash(&context.mailbox_key),
            context.uid,
            short_hash(url)
        );
        let record = SourceRecord {
            source_id: source_id.clone(),
            origin: "email-url-pdf".to_string(),
            external_id: url.to_string(),
            canonical_path: to_unix_string(canonical_path.strip_prefix(root)?),
            asset_root: Some(to_unix_string(original_path.parent().unwrap_or(asset_root).strip_prefix(root)?)),
            checksum: canonical_write.checksum,
            imported_at: context.received_at.to_rfc3339(),
            updated_at: fetched_at.clone(),
            tags: vec!["email".to_string(), "url".to_string(), "pdf".to_string()],
            status: "imported".to_string(),
            title: format!("Linked PDF {}", index),
            metadata: json!({
                "parent_uid": context.uid,
                "parent_message_id": context.message_id,
                "url": url,
                "content_type": content_type,
                "original_asset": to_unix_string(original_path.strip_prefix(root)?),
                "original_checksum": original_write.checksum,
            }),
        };
        vault::save_source_record(root, &record)?;
        return Ok(SavedSource {
            source_id,
            changed_paths: vec![
                to_unix_string(original_path.strip_prefix(root)?),
                to_unix_string(canonical_path.strip_prefix(root)?),
            ],
        });
    }

    let html = response.text().context("failed to read fetched HTML body")?;
    let title = extract_html_title(&html).unwrap_or_else(|| format!("Link {}", index));
    let main_html = extract_main_html(&html);
    let original_path = asset_root.join("links").join(format!("link-{:02}.html", index));
    let original_write = write_text_if_changed(&original_path, &main_html)?;
    let converted = html2md::parse_html(&main_html);
    if converted.trim().is_empty() {
        return Err(anyhow!("linked page {} produced no readable content", url));
    }
    let canonical_path = raw_root.join("links").join(format!("link-{:02}.md", index));
    let markdown = format!(
        "---\ntitle: {}\norigin: email-url\nsource_url: {}\nfetched_at: {}\n---\n\n# {}\n\nSource URL: {}\n\n{}\n",
        yaml_escape(&title),
        yaml_escape(url),
        fetched_at,
        title,
        url,
        converted.trim()
    );
    let canonical_write = write_text_if_changed(&canonical_path, &markdown)?;
    let source_id = format!(
        "email:{}:{}:url:{}",
        short_hash(&context.mailbox_key),
        context.uid,
        short_hash(url)
    );
    let record = SourceRecord {
        source_id: source_id.clone(),
        origin: "email-url".to_string(),
        external_id: url.to_string(),
        canonical_path: to_unix_string(canonical_path.strip_prefix(root)?),
        asset_root: Some(to_unix_string(original_path.parent().unwrap_or(asset_root).strip_prefix(root)?)),
        checksum: canonical_write.checksum,
        imported_at: context.received_at.to_rfc3339(),
        updated_at: fetched_at,
        tags: vec!["email".to_string(), "url".to_string()],
        status: "imported".to_string(),
        title,
        metadata: json!({
            "parent_uid": context.uid,
            "parent_message_id": context.message_id,
            "url": url,
            "content_type": content_type,
            "original_asset": to_unix_string(original_path.strip_prefix(root)?),
            "original_checksum": original_write.checksum,
        }),
    };
    vault::save_source_record(root, &record)?;
    Ok(SavedSource {
        source_id,
        changed_paths: vec![
            to_unix_string(original_path.strip_prefix(root)?),
            to_unix_string(canonical_path.strip_prefix(root)?),
        ],
    })
}

fn build_message_context(mailbox_key: &str, uid: u32, parsed: &ParsedMail<'_>) -> Result<MessageContext> {
    let subject = parsed
        .headers
        .get_first_value("Subject")
        .unwrap_or_else(|| format!("Email {}", uid))
        .trim()
        .to_string();
    let sender_addresses = sender_addresses(parsed);
    let sender_display = if sender_addresses.is_empty() {
        parsed
            .headers
            .get_first_value("From")
            .unwrap_or_else(|| "unknown sender".to_string())
    } else {
        sender_addresses.join(", ")
    };
    let message_id = parsed
        .headers
        .get_first_value("Message-ID")
        .map(|value| value.trim().trim_matches(['<', '>']).to_string())
        .filter(|value| !value.is_empty());
    let received_at = parsed
        .headers
        .get_first_value("Date")
        .and_then(|value| mailparse::dateparse(&value).ok())
        .and_then(|timestamp| Utc.timestamp_opt(timestamp, 0).single())
        .unwrap_or_else(Utc::now);
    let plain_body = preferred_text_body(parsed);
    let html_body = preferred_html_body(parsed);
    let body_markdown = if let Some(body) = plain_body.as_ref() {
        body.trim().to_string()
    } else if let Some(html) = html_body.as_ref() {
        html2md::parse_html(html).trim().to_string()
    } else {
        String::new()
    };
    let mut urls = Vec::new();
    if let Some(body) = plain_body.as_ref() {
        urls.extend(extract_urls(body));
    }
    if let Some(html) = html_body.as_ref() {
        urls.extend(extract_urls(html));
    }

    Ok(MessageContext {
        uid,
        subject: if subject.is_empty() { format!("Email {}", uid) } else { subject.clone() },
        sender_addresses,
        sender_display,
        message_id,
        received_at,
        body_markdown: if body_markdown.is_empty() {
            "_No text body_".to_string()
        } else {
            body_markdown
        },
        body_urls: dedupe(urls),
        folder_slug: build_message_folder_slug(uid, &subject, received_at),
        mailbox_key: mailbox_key.to_string(),
    })
}

fn sender_addresses(parsed: &ParsedMail<'_>) -> Vec<String> {
    parsed
        .headers
        .get_first_header("From")
        .and_then(|header| addrparse_header(&header).ok())
        .map(|addresses: MailAddrList| {
            let mut values = Vec::new();
            for address in addresses.iter() {
                flatten_address(address, &mut values);
            }
            dedupe(values.into_iter().map(|value| value.to_ascii_lowercase()).collect())
        })
        .unwrap_or_default()
}

fn flatten_address(address: &MailAddr, values: &mut Vec<String>) {
    match address {
        MailAddr::Single(info) => values.push(info.addr.clone()),
        MailAddr::Group(group) => {
            for address in &group.addrs {
                values.push(address.addr.clone());
            }
        }
    }
}

fn preferred_text_body(parsed: &ParsedMail<'_>) -> Option<String> {
    parsed
        .parts()
        .filter(|part| is_inline_text(part, "text/plain"))
        .find_map(|part| part.get_body().ok().map(|body| body.trim().to_string()))
        .filter(|body| !body.is_empty())
}

fn preferred_html_body(parsed: &ParsedMail<'_>) -> Option<String> {
    parsed
        .parts()
        .filter(|part| is_inline_text(part, "text/html"))
        .find_map(|part| part.get_body().ok())
        .filter(|body| !body.trim().is_empty())
}

fn is_inline_text(part: &ParsedMail<'_>, mime: &str) -> bool {
    let disposition = part.get_content_disposition();
    part.ctype.mimetype.eq_ignore_ascii_case(mime)
        && !matches!(disposition.disposition, mailparse::DispositionType::Attachment)
}

fn attachment_filename(part: &ParsedMail<'_>, disposition: &mailparse::ParsedContentDisposition) -> Option<String> {
    disposition
        .params
        .get("filename")
        .cloned()
        .or_else(|| part.ctype.params.get("name").cloned())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn is_markdown_attachment(mime: &str, filename: &str) -> bool {
    matches!(mime, "text/markdown" | "text/x-markdown")
        || filename.ends_with(".md")
        || filename.ends_with(".markdown")
}

fn sender_allowed(sender_addresses: &[String], allowlist: &BTreeSet<String>) -> bool {
    sender_addresses.iter().any(|address| allowlist.contains(address))
}

fn normalize_inbox_config(config: InboxConfig) -> InboxConfig {
    InboxConfig {
        enabled: config.enabled,
        imap_host: config.imap_host.trim().to_string(),
        imap_port: if config.imap_port == 0 { 993 } else { config.imap_port },
        imap_username: config.imap_username.trim().to_string(),
        imap_folder: if config.imap_folder.trim().is_empty() {
            "INBOX".to_string()
        } else {
            config.imap_folder.trim().to_string()
        },
        allowed_senders: dedupe(
            config
                .allowed_senders
                .into_iter()
                .map(|sender| sender.trim().to_ascii_lowercase())
                .filter(|sender| !sender.is_empty())
                .collect(),
        ),
        sync_on_startup: config.sync_on_startup,
    }
}

fn settings_view(config: &InboxConfig) -> Result<InboxSettings> {
    let normalized = normalize_inbox_config(config.clone());
    let password_configured = read_secret()?.is_some();
    Ok(InboxSettings {
        enabled: normalized.enabled,
        imap_host: normalized.imap_host.clone(),
        imap_port: normalized.imap_port,
        imap_username: normalized.imap_username.clone(),
        imap_folder: normalized.imap_folder.clone(),
        allowed_senders: normalized.allowed_senders.clone(),
        sync_on_startup: normalized.sync_on_startup,
        password_configured,
        configured: is_config_complete(&normalized, password_configured),
    })
}

fn is_config_complete(config: &InboxConfig, password_configured: bool) -> bool {
    !config.imap_host.is_empty()
        && config.imap_port > 0
        && !config.imap_username.is_empty()
        && !config.imap_folder.is_empty()
        && !config.allowed_senders.is_empty()
        && password_configured
}

fn mailbox_key(config: &InboxConfig) -> String {
    format!(
        "{}:{}:{}:{}",
        config.imap_host, config.imap_port, config.imap_username, config.imap_folder
    )
}

fn read_secret() -> Result<Option<String>> {
    let entry = Entry::new(KEYCHAIN_SERVICE, INBOX_SECRET_ACCOUNT)?;
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(anyhow!(error.to_string())).context("failed to read inbox IMAP password"),
    }
}

fn load_state(root: &Path) -> Result<InboxState> {
    let path = vault::inbox_state_path(root);
    if !path.exists() {
        return Ok(InboxState::default());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

fn save_state(root: &Path, state: &InboxState) -> Result<()> {
    let path = vault::inbox_state_path(root);
    ensure_parent_dir(&path)?;
    fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

fn raw_message_root(root: &Path, context: &MessageContext) -> PathBuf {
    root.join("raw/email")
        .join(format!("{:04}", context.received_at.year()))
        .join(format!("{:02}", context.received_at.month()))
        .join(&context.folder_slug)
}

fn asset_message_root(root: &Path, context: &MessageContext) -> PathBuf {
    root.join("raw/assets/email")
        .join(format!("{:04}", context.received_at.year()))
        .join(format!("{:02}", context.received_at.month()))
        .join(&context.folder_slug)
}

fn build_message_folder_slug(uid: u32, subject: &str, received_at: DateTime<Utc>) -> String {
    let subject_slug = slugify(subject);
    format!(
        "{}-{}-uid-{}",
        received_at.format("%Y%m%d"),
        if subject_slug.is_empty() { "email".to_string() } else { subject_slug },
        uid
    )
}

fn extract_urls(input: &str) -> Vec<String> {
    let regex = Regex::new(r#"https?://[^\s<>()"']+"#).expect("url regex");
    regex
        .find_iter(input)
        .map(|capture| clean_url(capture.as_str()))
        .filter(|url| !url.is_empty())
        .collect()
}

fn clean_url(value: &str) -> String {
    value
        .trim_end_matches(['.', ',', ';', ')', ']', '>', '"', '\''])
        .trim()
        .to_string()
}

fn extract_html_title(html: &str) -> Option<String> {
    let regex = Regex::new("(?is)<title[^>]*>(.*?)</title>").ok()?;
    regex
        .captures(html)
        .and_then(|captures| captures.get(1))
        .map(|capture| strip_html_tags(capture.as_str()).trim().to_string())
        .filter(|title| !title.is_empty())
}

fn extract_main_html(html: &str) -> String {
    for pattern in [
        "(?is)<main[^>]*>(.*?)</main>",
        "(?is)<article[^>]*>(.*?)</article>",
        "(?is)<body[^>]*>(.*?)</body>",
    ] {
        if let Ok(regex) = Regex::new(pattern) {
            if let Some(captures) = regex.captures(html) {
                if let Some(body) = captures.get(1) {
                    return body.as_str().to_string();
                }
            }
        }
    }
    html.to_string()
}

fn strip_html_tags(value: &str) -> String {
    Regex::new("(?is)<[^>]+>")
        .map(|regex| regex.replace_all(value, " ").to_string())
        .unwrap_or_else(|_| value.to_string())
}

fn ensure_pdf_extension(filename: &str) -> String {
    if filename.to_ascii_lowercase().ends_with(".pdf") {
        sanitize_segment(filename)
    } else {
        format!("{}.pdf", sanitize_segment(filename))
    }
}

fn short_hash(value: &str) -> String {
    sha256_hex(value.as_bytes()).chars().take(12).collect()
}

fn yaml_escape(value: &str) -> String {
    value.replace('"', "\\\"")
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            deduped.push(value);
        }
    }
    deduped
}

struct WriteResult {
    checksum: String,
}

fn write_text_if_changed(path: &Path, content: &str) -> Result<WriteResult> {
    write_bytes_if_changed(path, content.as_bytes())
}

fn write_bytes_if_changed(path: &Path, bytes: &[u8]) -> Result<WriteResult> {
    ensure_parent_dir(path)?;
    let checksum = sha256_hex(bytes);
    let existing_checksum = if path.exists() {
        Some(sha256_hex(&fs::read(path)?))
    } else {
        None
    };
    if existing_checksum.as_deref() != Some(checksum.as_str()) {
        fs::write(path, bytes).with_context(|| format!("failed to write {}", path.display()))?;
    }
    Ok(WriteResult { checksum })
}

#[cfg(test)]
mod tests {
    use super::{build_message_context, extract_urls};
    use mailparse::parse_mail;

    #[test]
    fn extracts_urls_and_drops_trailing_punctuation() {
        let urls = extract_urls("One https://example.com/test, and two https://openai.com/docs.");
        assert_eq!(
            urls,
            vec![
                "https://example.com/test".to_string(),
                "https://openai.com/docs".to_string()
            ]
        );
    }

    #[test]
    fn builds_message_context_from_text_and_html_bodies() {
        let raw = concat!(
            "From: Me <me@example.com>\r\n",
            "Subject: Inbox test\r\n",
            "Date: Tue, 04 Apr 2023 10:11:12 +0000\r\n",
            "Message-ID: <abc@example.com>\r\n",
            "Content-Type: multipart/alternative; boundary=xyz\r\n",
            "\r\n",
            "--xyz\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Body with https://example.com/one\r\n",
            "--xyz\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "\r\n",
            "<html><body><a href=\"https://example.com/two\">two</a></body></html>\r\n",
            "--xyz--\r\n"
        );
        let parsed = parse_mail(raw.as_bytes()).unwrap();
        let context = build_message_context("mailbox", 42, &parsed).unwrap();
        assert_eq!(context.subject, "Inbox test");
        assert_eq!(context.sender_addresses, vec!["me@example.com".to_string()]);
        assert_eq!(
            context.body_urls,
            vec![
                "https://example.com/one".to_string(),
                "https://example.com/two".to_string()
            ]
        );
    }
}
