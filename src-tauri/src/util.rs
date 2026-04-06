use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

pub fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub fn sanitize_segment(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "untitled".to_string();
    }

    let mut cleaned = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        let next = if ch.is_ascii_alphanumeric()
            || matches!(ch, ' ' | '-' | '_' | '.' | '(' | ')' | '&' | '\'' | '+')
        {
            ch
        } else {
            '-'
        };
        cleaned.push(next);
    }

    let mut collapsed = String::with_capacity(cleaned.len());
    let mut last_dash = false;
    for ch in cleaned.chars() {
        if ch == '-' {
            if !last_dash {
                collapsed.push(ch);
            }
            last_dash = true;
        } else {
            collapsed.push(ch);
            last_dash = false;
        }
    }

    collapsed.trim_matches([' ', '-', '.']).to_string()
}

pub fn slugify(value: &str) -> String {
    let sanitized = sanitize_segment(value);
    let mut slug = String::with_capacity(sanitized.len());
    let mut last_dash = false;
    for ch in sanitized.chars() {
        let lowered = ch.to_ascii_lowercase();
        if lowered.is_ascii_alphanumeric() {
            slug.push(lowered);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub fn to_unix_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

pub fn safe_relative_path(path: &str) -> Result<PathBuf> {
    let parsed = PathBuf::from(path);
    if parsed.is_absolute() {
        return Err(anyhow!("absolute paths are not allowed"));
    }

    for component in parsed.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!("path traversal is not allowed"));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }

    Ok(parsed)
}

pub fn ensure_parent_dir(path: &Path) -> Result<()> {
    match path.parent() {
        Some(parent) => {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            Ok(())
        }
        None => Err(anyhow!("path has no parent: {}", path.display())),
    }
}

pub fn strip_json_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(stripped) = trimmed.strip_prefix("```json") {
        return stripped.trim().trim_end_matches("```").trim();
    }
    if let Some(stripped) = trimmed.strip_prefix("```") {
        return stripped.trim().trim_end_matches("```").trim();
    }
    trimmed
}
