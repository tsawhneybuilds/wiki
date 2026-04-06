use crate::models::{
    CitationReference, DocumentDetails, JobRecord, LinkReference, SearchResult, SourceRecord,
    WorkspaceStats,
};
use crate::util::{sha256_hex, to_unix_string};
use crate::vault;
use anyhow::{Context, Result, anyhow};
use regex::Regex;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
struct CollectedDocument {
    path: String,
    title: String,
    scope: String,
    content: String,
    writable: bool,
    checksum: String,
    modified_at: String,
    source_id: Option<String>,
}

pub fn reindex_workspace(root: &Path) -> Result<WorkspaceStats> {
    vault::ensure_layout(root)?;
    let mut conn = Connection::open(vault::manifest_db_path(root))?;
    recreate_schema(&conn)?;

    let records = vault::load_source_records(root)?;
    let jobs = vault::load_jobs(root)?;
    let documents = collect_documents(root, &records)?;
    let alias_map = build_alias_map(&documents);

    let tx = conn.transaction()?;
    for record in &records {
        insert_source_record(&tx, record)?;
    }
    for job in &jobs {
        insert_job(&tx, job)?;
    }
    for document in &documents {
        insert_document(&tx, document)?;
    }
    for document in &documents {
        insert_references(&tx, document, &alias_map)?;
    }
    tx.commit()?;

    let raw_documents = documents.iter().filter(|doc| doc.scope == "raw").count();
    let wiki_documents = documents.iter().filter(|doc| doc.scope == "wiki").count();
    let system_documents = documents.iter().filter(|doc| doc.scope == "system").count();

    Ok(WorkspaceStats {
        raw_documents,
        wiki_documents,
        system_documents,
        source_records: records.len(),
        jobs: jobs.len(),
        last_reindex_at: Some(crate::util::now_iso()),
    })
}

pub fn workspace_stats(root: &Path) -> Result<WorkspaceStats> {
    ensure_index(root)?;
    let conn = Connection::open(vault::manifest_db_path(root))?;
    let raw_documents =
        count_where(&conn, "SELECT COUNT(*) FROM documents WHERE scope = 'raw'".to_string())?;
    let wiki_documents =
        count_where(&conn, "SELECT COUNT(*) FROM documents WHERE scope = 'wiki'".to_string())?;
    let system_documents =
        count_where(&conn, "SELECT COUNT(*) FROM documents WHERE scope = 'system'".to_string())?;
    let source_records = count_where(&conn, "SELECT COUNT(*) FROM source_records".to_string())?;
    let jobs = count_where(&conn, "SELECT COUNT(*) FROM jobs".to_string())?;

    Ok(WorkspaceStats {
        raw_documents,
        wiki_documents,
        system_documents,
        source_records,
        jobs,
        last_reindex_at: Some(crate::util::now_iso()),
    })
}

pub fn recent_jobs(root: &Path) -> Result<Vec<JobRecord>> {
    ensure_index(root)?;
    let conn = Connection::open(vault::manifest_db_path(root))?;
    let mut stmt = conn.prepare(
        "SELECT id, kind, status, created_at, updated_at, summary, source_ids_json, changed_paths_json, diffs_json, details_json
         FROM jobs
         ORDER BY created_at DESC
         LIMIT 10",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(JobRecord {
            id: row.get(0)?,
            kind: row.get(1)?,
            status: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            summary: row.get(5)?,
            source_ids: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(6)?)
                .unwrap_or_default(),
            changed_paths: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(7)?)
                .unwrap_or_default(),
            diffs: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
            details: serde_json::from_str(&row.get::<_, String>(9)?)
                .unwrap_or_else(|_| serde_json::json!({})),
        })
    })?;

    let mut jobs = Vec::new();
    for row in rows {
        jobs.push(row?);
    }
    Ok(jobs)
}

pub fn search_documents(root: &Path, query: &str) -> Result<Vec<SearchResult>> {
    ensure_index(root)?;
    let conn = Connection::open(vault::manifest_db_path(root))?;
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let fts_query = build_fts_query(query);
    let mut stmt = conn.prepare(
        "SELECT d.path, d.title, d.scope,
                snippet(doc_fts, 2, '<mark>', '</mark>', ' … ', 18) AS excerpt,
                d.writable, d.source_id
         FROM doc_fts
         JOIN documents d ON d.path = doc_fts.path
         WHERE doc_fts MATCH ?
         ORDER BY CASE d.scope WHEN 'wiki' THEN 0 WHEN 'system' THEN 1 ELSE 2 END, bm25(doc_fts)
         LIMIT 24",
    )?;

    let rows = stmt.query_map(params![fts_query], |row| {
        Ok(SearchResult {
            path: row.get(0)?,
            title: row.get(1)?,
            scope: row.get(2)?,
            excerpt: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            writable: row.get::<_, i64>(4)? == 1,
            source_id: row.get(5)?,
        })
    });

    match rows {
        Ok(rows) => {
            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok(results)
        }
        Err(_) => fallback_search(&conn, query),
    }
}

pub fn read_document_details(root: &Path, path: &str) -> Result<DocumentDetails> {
    ensure_index(root)?;
    let conn = Connection::open(vault::manifest_db_path(root))?;
    let mut stmt = conn.prepare(
        "SELECT path, title, scope, content, writable, source_id
         FROM documents
         WHERE path = ?1",
    )?;

    let document = stmt
        .query_row(params![path], |row| {
            Ok(DocumentDetails {
                path: row.get(0)?,
                title: row.get(1)?,
                scope: row.get(2)?,
                content: row.get(3)?,
                writable: row.get::<_, i64>(4)? == 1,
                source_id: row.get(5)?,
                backlinks: Vec::new(),
                outgoing_links: Vec::new(),
                citations: Vec::new(),
            })
        })
        .with_context(|| format!("document not found: {path}"))?;

    let backlinks = query_links(
        &conn,
        "SELECT d.path, d.title
         FROM links
         JOIN documents d ON d.path = links.from_path
         WHERE links.resolved_path = ?1
         ORDER BY d.title",
        path,
    )?;

    let outgoing_links = query_links(
        &conn,
        "SELECT d.path, d.title
         FROM links
         JOIN documents d ON d.path = links.resolved_path
         WHERE links.from_path = ?1
         ORDER BY d.title",
        path,
    )?;

    let mut citation_stmt = conn.prepare(
        "SELECT s.source_id, s.title, s.canonical_path, s.origin, s.status
         FROM citations
         JOIN source_records s ON s.source_id = citations.source_id
         WHERE citations.from_path = ?1
         ORDER BY s.title",
    )?;

    let citation_rows = citation_stmt.query_map(params![path], |row| {
        Ok(CitationReference {
            source_id: row.get(0)?,
            title: row.get(1)?,
            path: row.get(2)?,
            origin: row.get(3)?,
            status: row.get(4)?,
        })
    })?;

    let mut citations = Vec::new();
    for row in citation_rows {
        citations.push(row?);
    }

    Ok(DocumentDetails {
        backlinks,
        outgoing_links,
        citations,
        ..document
    })
}

pub fn source_records_by_ids(root: &Path, source_ids: &[String]) -> Result<Vec<SourceRecord>> {
    ensure_index(root)?;
    let conn = Connection::open(vault::manifest_db_path(root))?;
    let mut records = Vec::new();
    for source_id in source_ids {
        let mut stmt = conn.prepare(
            "SELECT source_id, origin, external_id, canonical_path, asset_root, checksum, imported_at, updated_at, tags_json, status, title, metadata_json
             FROM source_records
             WHERE source_id = ?1",
        )?;

        if let Ok(record) = stmt.query_row(params![source_id], |row| {
            Ok(SourceRecord {
                source_id: row.get(0)?,
                origin: row.get(1)?,
                external_id: row.get(2)?,
                canonical_path: row.get(3)?,
                asset_root: row.get(4)?,
                checksum: row.get(5)?,
                imported_at: row.get(6)?,
                updated_at: row.get(7)?,
                tags: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(8)?)
                    .unwrap_or_default(),
                status: row.get(9)?,
                title: row.get(10)?,
                metadata: serde_json::from_str(&row.get::<_, String>(11)?)
                    .unwrap_or_else(|_| serde_json::json!({})),
            })
        }) {
            records.push(record);
        }
    }
    Ok(records)
}

fn fallback_search(conn: &Connection, query: &str) -> Result<Vec<SearchResult>> {
    let like = format!("%{}%", query.trim());
    let mut stmt = conn.prepare(
        "SELECT path, title, scope, substr(content, 1, 200), writable, source_id
         FROM documents
         WHERE content LIKE ?1 OR title LIKE ?1
         LIMIT 24",
    )?;

    let rows = stmt.query_map(params![like], |row| {
        Ok(SearchResult {
            path: row.get(0)?,
            title: row.get(1)?,
            scope: row.get(2)?,
            excerpt: row.get(3)?,
            writable: row.get::<_, i64>(4)? == 1,
            source_id: row.get(5)?,
        })
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

fn query_links(conn: &Connection, sql: &str, path: &str) -> Result<Vec<LinkReference>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![path], |row| {
        Ok(LinkReference {
            path: row.get(0)?,
            title: row.get(1)?,
        })
    })?;

    let mut links = Vec::new();
    for row in rows {
        links.push(row?);
    }
    Ok(links)
}

fn recreate_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        DROP TABLE IF EXISTS documents;
        DROP TABLE IF EXISTS links;
        DROP TABLE IF EXISTS citations;
        DROP TABLE IF EXISTS source_records;
        DROP TABLE IF EXISTS jobs;
        DROP TABLE IF EXISTS doc_fts;

        CREATE TABLE documents (
            path TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            scope TEXT NOT NULL,
            content TEXT NOT NULL,
            writable INTEGER NOT NULL,
            checksum TEXT NOT NULL,
            modified_at TEXT NOT NULL,
            source_id TEXT
        );

        CREATE VIRTUAL TABLE doc_fts USING fts5(path, title, content, scope);

        CREATE TABLE links (
            from_path TEXT NOT NULL,
            raw_target TEXT NOT NULL,
            resolved_path TEXT
        );

        CREATE TABLE citations (
            from_path TEXT NOT NULL,
            source_id TEXT NOT NULL
        );

        CREATE TABLE source_records (
            source_id TEXT PRIMARY KEY,
            origin TEXT NOT NULL,
            external_id TEXT NOT NULL,
            canonical_path TEXT NOT NULL,
            asset_root TEXT,
            checksum TEXT NOT NULL,
            imported_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            tags_json TEXT NOT NULL,
            status TEXT NOT NULL,
            title TEXT NOT NULL,
            metadata_json TEXT NOT NULL
        );

        CREATE TABLE jobs (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            summary TEXT NOT NULL,
            source_ids_json TEXT NOT NULL,
            changed_paths_json TEXT NOT NULL,
            diffs_json TEXT NOT NULL,
            details_json TEXT NOT NULL
        );
        ",
    )?;
    Ok(())
}

fn insert_source_record(conn: &Connection, record: &SourceRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO source_records (source_id, origin, external_id, canonical_path, asset_root, checksum, imported_at, updated_at, tags_json, status, title, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            record.source_id,
            record.origin,
            record.external_id,
            record.canonical_path,
            record.asset_root,
            record.checksum,
            record.imported_at,
            record.updated_at,
            serde_json::to_string(&record.tags)?,
            record.status,
            record.title,
            serde_json::to_string(&record.metadata)?,
        ],
    )?;
    Ok(())
}

fn insert_job(conn: &Connection, job: &JobRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO jobs (id, kind, status, created_at, updated_at, summary, source_ids_json, changed_paths_json, diffs_json, details_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            job.id,
            job.kind,
            job.status,
            job.created_at,
            job.updated_at,
            job.summary,
            serde_json::to_string(&job.source_ids)?,
            serde_json::to_string(&job.changed_paths)?,
            serde_json::to_string(&job.diffs)?,
            serde_json::to_string(&job.details)?,
        ],
    )?;
    Ok(())
}

fn insert_document(conn: &Connection, document: &CollectedDocument) -> Result<()> {
    conn.execute(
        "INSERT INTO documents (path, title, scope, content, writable, checksum, modified_at, source_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            document.path,
            document.title,
            document.scope,
            document.content,
            i64::from(document.writable),
            document.checksum,
            document.modified_at,
            document.source_id,
        ],
    )?;
    conn.execute(
        "INSERT INTO doc_fts (path, title, content, scope) VALUES (?1, ?2, ?3, ?4)",
        params![
            document.path,
            document.title,
            document.content,
            document.scope,
        ],
    )?;
    Ok(())
}

fn insert_references(
    conn: &Connection,
    document: &CollectedDocument,
    alias_map: &HashMap<String, String>,
) -> Result<()> {
    let link_regex = Regex::new(r"\[\[([^\]|]+)(?:\|[^\]]+)?\]\]")?;
    let citation_regex = Regex::new(r"\[@([A-Za-z0-9:_\-]+)\]")?;

    for captures in link_regex.captures_iter(&document.content) {
        let raw_target = captures.get(1).map(|m| m.as_str()).unwrap_or_default().trim();
        let resolved = resolve_link(raw_target, alias_map);
        conn.execute(
            "INSERT INTO links (from_path, raw_target, resolved_path) VALUES (?1, ?2, ?3)",
            params![document.path, raw_target, resolved],
        )?;
    }

    for captures in citation_regex.captures_iter(&document.content) {
        if let Some(source_id) = captures.get(1) {
            conn.execute(
                "INSERT INTO citations (from_path, source_id) VALUES (?1, ?2)",
                params![document.path, source_id.as_str()],
            )?;
        }
    }

    Ok(())
}

fn resolve_link(raw_target: &str, alias_map: &HashMap<String, String>) -> Option<String> {
    let normalized = raw_target.trim().trim_end_matches(".md").to_lowercase();
    alias_map
        .get(&normalized)
        .cloned()
        .or_else(|| alias_map.get(&format!("{normalized}.md")).cloned())
}

fn build_alias_map(documents: &[CollectedDocument]) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for document in documents {
        let path_no_ext = document.path.trim_end_matches(".md").to_lowercase();
        aliases.entry(document.title.to_lowercase()).or_insert_with(|| document.path.clone());
        aliases.entry(path_no_ext.clone()).or_insert_with(|| document.path.clone());

        if let Some(stem) = Path::new(&document.path).file_stem().and_then(|stem| stem.to_str()) {
            aliases.entry(stem.to_lowercase()).or_insert_with(|| document.path.clone());
        }

        if document.path.starts_with("wiki/") {
            let wiki_relative = path_no_ext.trim_start_matches("wiki/").to_string();
            aliases.entry(wiki_relative).or_insert_with(|| document.path.clone());
        }
    }
    aliases
}

fn collect_documents(root: &Path, records: &[SourceRecord]) -> Result<Vec<CollectedDocument>> {
    let source_lookup = records
        .iter()
        .map(|record| (record.canonical_path.clone(), record.source_id.clone()))
        .collect::<HashMap<_, _>>();

    let mut documents = Vec::new();
    for relative in ["raw", "wiki", "system"] {
        let absolute = root.join(relative);
        if !absolute.exists() {
            continue;
        }

        for entry in walkdir::WalkDir::new(&absolute) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
            if !matches!(extension, "md" | "markdown" | "txt" | "csv" | "json") {
                continue;
            }

            let content = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let relative_path = to_unix_string(path.strip_prefix(root)?);
            let scope = relative_path
                .split('/')
                .next()
                .ok_or_else(|| anyhow!("invalid path"))?
                .to_string();

            let modified = entry
                .metadata()?
                .modified()
                .ok()
                .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339())
                .unwrap_or_else(crate::util::now_iso);

            documents.push(CollectedDocument {
                title: infer_title(path, &content),
                path: relative_path.clone(),
                scope: scope.clone(),
                writable: scope == "wiki",
                checksum: sha256_hex(content.as_bytes()),
                modified_at: modified,
                source_id: source_lookup.get(&relative_path).cloned(),
                content,
            });
        }
    }

    documents.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(documents)
}

fn infer_title(path: &Path, content: &str) -> String {
    for line in content.lines() {
        if let Some(title) = line.strip_prefix("# ") {
            return title.trim().to_string();
        }
    }

    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("untitled")
        .to_string()
}

fn ensure_index(root: &Path) -> Result<()> {
    if !vault::manifest_db_path(root).exists() {
        reindex_workspace(root)?;
    }
    Ok(())
}

fn count_where(conn: &Connection, sql: String) -> Result<usize> {
    let count = conn.query_row(&sql, [], |row| row.get::<_, i64>(0))?;
    Ok(count as usize)
}

fn build_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| {
            token
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
                .collect::<String>()
        })
        .filter(|token| !token.is_empty())
        .map(|token| format!("{token}*"))
        .collect::<Vec<_>>()
        .join(" AND ")
}
