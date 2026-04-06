use crate::index;
use crate::models::{ImportResult, SourceRecord};
use crate::util::{ensure_parent_dir, now_iso, sanitize_segment, sha256_hex, slugify, to_unix_string};
use crate::vault;
use anyhow::{Context, Result, anyhow};
use base64::Engine;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::NamedTempFile;
use zip::ZipArchive;

#[derive(Debug, Deserialize)]
struct AppleNotesEnvelope {
    exported_at: String,
    notes: Vec<AppleNotePayload>,
}

#[derive(Debug, Deserialize)]
struct AppleNotePayload {
    id: String,
    name: String,
    account: String,
    folder_path: Vec<String>,
    creation_date: Option<String>,
    modification_date: Option<String>,
    body_html: String,
    plaintext: String,
    password_protected: bool,
    shared: bool,
    tags: Vec<String>,
}

pub fn import_notion_zip(root: &Path, zip_path: &str) -> Result<ImportResult> {
    vault::ensure_layout(root)?;
    let zip_file = fs::File::open(zip_path).with_context(|| format!("failed to open {zip_path}"))?;
    let mut archive = ZipArchive::new(zip_file)?;

    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut warnings = Vec::new();
    let mut source_ids = Vec::new();
    let mut touched_paths = Vec::new();
    let mut seen_source_ids = HashSet::new();

    import_notion_archive(
        root,
        &mut archive,
        zip_path,
        &mut imported,
        &mut updated,
        &mut skipped,
        &mut warnings,
        &mut source_ids,
        &mut touched_paths,
        &mut seen_source_ids,
    )?;

    if source_ids.is_empty() {
        warnings.push("No Notion markdown or CSV files were detected in the export ZIP.".to_string());
    }

    let mut job = vault::default_job("import-notion", "Imported Notion export");
    job.summary = format!(
        "Imported {} new files, updated {}, skipped {} from {}",
        imported, updated, skipped, zip_path
    );
    job.source_ids = source_ids.clone();
    job.changed_paths = touched_paths;
    job.details = json!({
        "zip_path": zip_path,
        "warnings": warnings,
    });
    vault::save_job(root, &job)?;
    vault::append_log_entry(root, &job)?;
    index::reindex_workspace(root)?;

    Ok(ImportResult {
        imported,
        updated,
        skipped,
        warnings,
        source_ids,
        job,
    })
}

fn import_notion_archive<R: Read + std::io::Seek>(
    root: &Path,
    archive: &mut ZipArchive<R>,
    zip_path: &str,
    imported: &mut usize,
    updated: &mut usize,
    skipped: &mut usize,
    warnings: &mut Vec<String>,
    source_ids: &mut Vec<String>,
    touched_paths: &mut Vec<String>,
    seen_source_ids: &mut HashSet<String>,
) -> Result<()> {
    let nested_zip_entries = nested_zip_entries(archive)?;
    if !nested_zip_entries.is_empty() {
        for nested_name in nested_zip_entries {
            let mut nested_file = archive.by_name(&nested_name)?;
            let mut nested_bytes = Vec::new();
            nested_file.read_to_end(&mut nested_bytes)?;
            let cursor = Cursor::new(nested_bytes);
            let mut nested_archive = ZipArchive::new(cursor).with_context(|| {
                format!("failed to open nested Notion archive `{nested_name}` inside `{zip_path}`")
            })?;
            import_notion_archive(
                root,
                &mut nested_archive,
                zip_path,
                imported,
                updated,
                skipped,
                warnings,
                source_ids,
                touched_paths,
                seen_source_ids,
            )?;
        }
        return Ok(());
    }

    let prefix = common_archive_prefix(archive);
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let name = file.name().to_string();
        if file.is_dir() {
            continue;
        }

        let stripped = strip_prefix(&name, prefix.as_deref());
        if stripped.is_empty() {
            *skipped += 1;
            continue;
        }

        let normalized = normalize_notion_entry_path(stripped)?;
        let target = root.join("raw/notion").join(&normalized.clean_relative_path);
        ensure_parent_dir(&target)?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let checksum = sha256_hex(&bytes);

        let existing_checksum = if target.exists() {
            Some(sha256_hex(&fs::read(&target)?))
        } else {
            None
        };

        if existing_checksum.as_deref() != Some(checksum.as_str()) {
            fs::write(&target, &bytes)?;
            touched_paths.push(to_unix_string(target.strip_prefix(root)?));
            if existing_checksum.is_some() {
                *updated += 1;
            } else {
                *imported += 1;
            }
        } else {
            *skipped += 1;
        }

        if normalized.is_source_document {
            let title = Path::new(&normalized.clean_relative_path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Untitled")
                .to_string();
            let source_id = format!(
                "notion:{}",
                normalized
                    .external_id
                    .clone()
                    .unwrap_or_else(|| slugify(&normalized.clean_relative_path.to_string_lossy()))
            );

            if seen_source_ids.insert(source_id.clone()) {
                source_ids.push(source_id.clone());
            }

            let record = SourceRecord {
                source_id: source_id.clone(),
                origin: "notion".to_string(),
                external_id: normalized
                    .external_id
                    .clone()
                    .unwrap_or_else(|| normalized.original_relative_path.clone()),
                canonical_path: to_unix_string(target.strip_prefix(root)?),
                asset_root: target
                    .parent()
                    .map(|parent| to_unix_string(parent.strip_prefix(root).unwrap_or(parent))),
                checksum,
                imported_at: now_iso(),
                updated_at: now_iso(),
                tags: Vec::new(),
                status: "imported".to_string(),
                title,
                metadata: json!({
                    "original_path": normalized.original_relative_path,
                    "zip_path": zip_path,
                }),
            };
            vault::save_source_record(root, &record)?;
        }
    }

    if source_ids.is_empty() {
        warnings.push("No Notion markdown or CSV files were detected in the export ZIP.".to_string());
    }

    Ok(())
}

pub fn import_apple_notes(root: &Path, account_filter: Option<String>) -> Result<ImportResult> {
    vault::ensure_layout(root)?;
    let script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/export_apple_notes.js");
    let output_file = NamedTempFile::new()?;

    let mut command = Command::new("osascript");
    command.arg("-l").arg("JavaScript").arg(&script_path).arg(output_file.path());
    if let Some(filter) = account_filter.as_ref() {
        command.env("APPLE_NOTES_ACCOUNT_FILTER", filter);
    }

    let output = command.output().context("failed to invoke Apple Notes exporter")?;
    if !output.status.success() {
        return Err(anyhow!(
            "Apple Notes export failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let raw = fs::read_to_string(output_file.path())?;
    let envelope: AppleNotesEnvelope = serde_json::from_str(&raw)?;
    let mut imported = 0usize;
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut warnings = Vec::new();
    let mut source_ids = Vec::new();
    let mut changed_paths = Vec::new();

    for note in envelope.notes {
        let account_segment = sanitize_segment(&note.account);
        let folder_segments = note
            .folder_path
            .iter()
            .map(|segment| sanitize_segment(segment))
            .collect::<Vec<_>>();
        let folder_path = folder_segments
            .iter()
            .fold(PathBuf::new(), |acc, segment| acc.join(segment));
        let note_stem = sanitize_segment(&note.name);
        let note_slug = slugify(&format!("{}-{}", note_stem, note.id));
        let markdown_relative = PathBuf::from("raw/apple-notes")
            .join(&account_segment)
            .join(&folder_path)
            .join(format!("{note_stem}.md"));
        let markdown_path = root.join(&markdown_relative);
        ensure_parent_dir(&markdown_path)?;

        let asset_root = root
            .join("raw/assets/apple-notes")
            .join(&account_segment)
            .join(&folder_path)
            .join(&note_slug);
        fs::create_dir_all(&asset_root)?;

        let conversion = convert_note_html(root, &markdown_path, &asset_root, &note)?;
        let new_checksum = sha256_hex(conversion.markdown.as_bytes());
        let current_checksum = if markdown_path.exists() {
            Some(sha256_hex(&fs::read(&markdown_path)?))
        } else {
            None
        };

        if current_checksum.as_deref() != Some(new_checksum.as_str()) {
            fs::write(&markdown_path, conversion.markdown)?;
            changed_paths.push(to_unix_string(markdown_path.strip_prefix(root)?));
            if current_checksum.is_some() {
                updated += 1;
            } else {
                imported += 1;
            }
        } else {
            skipped += 1;
        }

        let mut note_warnings = conversion.warnings;
        if note.password_protected {
            note_warnings.push("password-protected".to_string());
        }
        if note.shared {
            note_warnings.push("shared-note".to_string());
        }

        let source_id = format!("apple-notes:{}", note_slug);
        source_ids.push(source_id.clone());
        if !note_warnings.is_empty() {
            warnings.push(format!("{}: {}", note.name, note_warnings.join(", ")));
        }

        let record = SourceRecord {
            source_id: source_id.clone(),
            origin: "apple-notes".to_string(),
            external_id: note.id.clone(),
            canonical_path: to_unix_string(markdown_path.strip_prefix(root)?),
            asset_root: Some(to_unix_string(asset_root.strip_prefix(root)?)),
            checksum: new_checksum,
            imported_at: envelope.exported_at.clone(),
            updated_at: note
                .modification_date
                .clone()
                .unwrap_or_else(|| envelope.exported_at.clone()),
            tags: note.tags.clone(),
            status: if note_warnings.is_empty() {
                "imported".to_string()
            } else {
                "warning".to_string()
            },
            title: note.name.clone(),
            metadata: json!({
                "account": note.account,
                "folder_path": note.folder_path,
                "created_at": note.creation_date,
                "shared": note.shared,
                "password_protected": note.password_protected,
                "warnings": note_warnings,
            }),
        };
        vault::save_source_record(root, &record)?;
    }

    let mut job = vault::default_job("import-apple-notes", "Imported Apple Notes");
    job.summary = format!(
        "Imported {} Apple Notes, updated {}, skipped {}",
        imported, updated, skipped
    );
    job.source_ids = source_ids.clone();
    job.changed_paths = changed_paths;
    job.details = json!({
        "account_filter": account_filter,
        "warnings": warnings,
    });
    vault::save_job(root, &job)?;
    vault::append_log_entry(root, &job)?;
    index::reindex_workspace(root)?;

    Ok(ImportResult {
        imported,
        updated,
        skipped,
        warnings,
        source_ids,
        job,
    })
}

struct HtmlConversion {
    markdown: String,
    warnings: Vec<String>,
}

fn convert_note_html(
    root: &Path,
    markdown_path: &Path,
    asset_root: &Path,
    note: &AppleNotePayload,
) -> Result<HtmlConversion> {
    let original_html_path = asset_root.join("original.html");
    fs::write(&original_html_path, &note.body_html)?;

    let data_uri_regex = Regex::new(r#"src="data:([^;"]+);base64,([^"]+)""#)?;
    let mut warnings = Vec::new();
    let mut html = note.body_html.clone();

    for (index, captures) in data_uri_regex.captures_iter(&note.body_html).enumerate() {
        let mime = captures.get(1).map(|capture| capture.as_str()).unwrap_or("application/octet-stream");
        let encoded = captures.get(2).map(|capture| capture.as_str()).unwrap_or("");
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        let extension = mime_to_extension(mime);
        let file_name = format!("inline-{:03}.{}", index + 1, extension);
        let asset_path = asset_root.join(&file_name);
        fs::write(&asset_path, bytes)?;
        let relative_asset = pathdiff::diff_paths(&asset_path, markdown_path.parent().unwrap_or(asset_root))
            .unwrap_or_else(|| asset_path.clone());
        let replacement = format!(r#"src="{}""#, to_unix_string(&relative_asset));
        if let Some(full_match) = captures.get(0) {
            html = html.replacen(full_match.as_str(), &replacement, 1);
        }
    }

    if note.body_html.contains("<table") {
        warnings.push("table-content-kept-in-html-fallback".to_string());
    }
    if note.password_protected {
        warnings.push("locked-note-metadata-only".to_string());
    }

    let markdown_body = html2md::parse_html(&html);
    let body = if markdown_body.trim().is_empty() {
        warnings.push("html-conversion-empty-fell-back-to-plaintext".to_string());
        note.plaintext.clone()
    } else {
        markdown_body
    };

    let frontmatter = format!(
        "---\ntitle: {}\norigin: apple-notes\naccount: {}\nfolder: {}\ncreated_at: {}\nupdated_at: {}\nsource_external_id: {}\ntags: [{}]\n---\n\n",
        note.name,
        note.account,
        note.folder_path.join(" / "),
        note.creation_date.clone().unwrap_or_default(),
        note.modification_date.clone().unwrap_or_default(),
        note.id,
        note.tags
            .iter()
            .map(|tag| format!("\"{}\"", tag))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let markdown = format!("{frontmatter}{body}");
    let _ = root;
    Ok(HtmlConversion { markdown, warnings })
}

fn mime_to_extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

struct NormalizedNotionPath {
    clean_relative_path: PathBuf,
    original_relative_path: String,
    external_id: Option<String>,
    is_source_document: bool,
}

fn normalize_notion_entry_path(path: &str) -> Result<NormalizedNotionPath> {
    let id_regex = Regex::new(
        r"^(?P<name>.+?) (?P<id>([0-9a-f]{32}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}))$",
    )?;

    let original = Path::new(path);
    let mut clean = PathBuf::new();
    let mut last_external_id = None;

    for component in original.components() {
        if let std::path::Component::Normal(part) = component {
            let raw = part.to_string_lossy();
            let (stem, ext) = match raw.rsplit_once('.') {
                Some((stem, ext)) => (stem.to_string(), Some(ext.to_string())),
                None => (raw.to_string(), None),
            };

            let mut clean_stem = stem.clone();
            if let Some(captures) = id_regex.captures(&stem) {
                clean_stem = captures
                    .name("name")
                    .map(|capture| capture.as_str().to_string())
                    .unwrap_or(stem.clone());
                last_external_id = captures.name("id").map(|capture| capture.as_str().to_string());
            }

            let segment = match ext {
                Some(ext) => format!("{}.{}", sanitize_segment(&clean_stem), ext),
                None => sanitize_segment(&clean_stem),
            };
            clean.push(segment);
        }
    }

    let extension = clean.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let is_source_document = matches!(extension, "md" | "csv");
    Ok(NormalizedNotionPath {
        clean_relative_path: clean,
        original_relative_path: path.to_string(),
        external_id: last_external_id,
        is_source_document,
    })
}

fn common_archive_prefix<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> Option<String> {
    let mut prefix: Option<String> = None;
    for index in 0..archive.len() {
        let file = archive.by_index(index).ok()?;
        let name = file.name();
        let head = name.split('/').next().unwrap_or("").to_string();
        if head.is_empty() {
            return None;
        }
        match prefix.as_ref() {
            Some(existing) if existing != &head => return None,
            None => prefix = Some(head),
            _ => {}
        }
    }
    prefix
}

fn strip_prefix<'a>(name: &'a str, prefix: Option<&str>) -> &'a str {
    match prefix {
        Some(prefix) => name.strip_prefix(prefix).unwrap_or(name).trim_start_matches('/'),
        None => name,
    }
}

fn nested_zip_entries<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> Result<Vec<String>> {
    let mut nested = Vec::new();
    let mut file_count = 0usize;
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        file_count += 1;
        if file.name().to_ascii_lowercase().ends_with(".zip") {
            nested.push(file.name().to_string());
        }
    }

    if file_count > 0 && nested.len() == file_count {
        Ok(nested)
    } else {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    #[test]
    fn imports_nested_notion_part_zip() -> Result<()> {
        let root_dir = tempdir()?;
        let export_dir = tempdir()?;
        let zip_path = export_dir.path().join("notion-wrapper.zip");

        let mut inner_cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut inner_zip = ZipWriter::new(&mut inner_cursor);
            inner_zip.start_file(
                "Export-1234/My Page 1234567890abcdef1234567890abcdef.md",
                SimpleFileOptions::default(),
            )?;
            inner_zip.write_all(b"# My Page\n\nBody")?;
            inner_zip.finish()?;
        }

        let inner_bytes = inner_cursor.into_inner();
        {
            let zip_file = fs::File::create(&zip_path)?;
            let mut outer_zip = ZipWriter::new(zip_file);
            outer_zip.start_file("Export-1234-Part-1.zip", SimpleFileOptions::default())?;
            outer_zip.write_all(&inner_bytes)?;
            outer_zip.finish()?;
        }

        let result = import_notion_zip(root_dir.path(), zip_path.to_str().unwrap())?;

        assert_eq!(result.imported, 1);
        assert!(!result.source_ids.is_empty());
        assert!(
            root_dir.path().join("raw/notion/My Page.md").exists(),
            "expected nested markdown file to be imported"
        );

        Ok(())
    }
}
