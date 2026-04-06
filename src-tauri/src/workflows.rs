use crate::index;
use crate::models::{
    CitationReference, DiffEntry, IngestRequest, JobRecord, LintFinding, LintReport, LintRequest,
    QueryAnswer, QueryRequest, SearchResult, WorkflowOptions,
};
use crate::providers;
use crate::util::{ensure_parent_dir, now_iso, slugify};
use crate::vault;
use anyhow::{Context, Result, anyhow};
use regex::Regex;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::TextDiff;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Debug, Deserialize)]
struct IngestModelResponse {
    summary: String,
    pages: Vec<IngestModelPage>,
}

#[derive(Debug, Deserialize)]
struct IngestModelPage {
    path: String,
    title: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct QuerySaveModelResponse {
    answer: String,
    page: QuerySavePage,
}

#[derive(Debug, Deserialize)]
struct QuerySavePage {
    path: Option<String>,
    title: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct LintModelResponse {
    summary: String,
    findings: Vec<LintFinding>,
}

pub fn propose_ingest(root: &Path, request: IngestRequest) -> Result<JobRecord> {
    if request.source_ids.is_empty() {
        return Err(anyhow!("select at least one raw source to ingest"));
    }

    let sources = index::source_records_by_ids(root, &request.source_ids)?;
    if sources.is_empty() {
        return Err(anyhow!("no matching source records were found"));
    }

    let agents = vault::read_document(root, "system/AGENTS.md")?;
    let index_page = vault::read_document(root, "system/index.md")?;
    let source_sections = build_source_sections(root, &sources)?;
    let related_context = related_pages_for_sources(root, &sources)?;
    let page_hint = request.page_hint.clone().unwrap_or_default();
    let workflow = normalize_workflow(root, &request.workflow_options)?;

    if workflow.provider_mode == "subscription" && workflow.execution_mode == "direct-edit" {
        let system = "You are maintaining a markdown wiki inside this repository. You may edit wiki pages only. Do not edit raw sources, application code, settings, or system/AGENTS.md. The app will refresh system/index.md and system/log.md after you finish. Final response: a short summary of the wiki pages you changed.";
        let user_prompt = format!(
            "Use the schema rules below and update the wiki directly.\n\nRules:\n- Only edit files under wiki/.\n- Do not touch raw/.\n- Use [[wikilinks]] between related wiki pages.\n- Cite raw factual material with [@source_id].\n- Prefer concise, high-signal pages.\n\nOptional page hint: {page_hint}\n\nSchema guide:\n{agents}\n\nExisting index:\n{index_page}\n\nRelated wiki context:\n{related_context}\n\nRaw sources:\n{}",
            source_sections.join("\n\n")
        );
        let metadata = execute_direct_edit(
            root,
            "ingest",
            request.source_ids.clone(),
            request.workflow_options.clone(),
            |root, options| providers::direct_edit(root, options, system, &user_prompt),
        )?;
        return Ok(metadata);
    }

    let user_prompt = format!(
        "You are maintaining a markdown wiki.\n\nUse the schema rules below and propose complete page contents.\n\nRules:\n- Put new or updated pages under wiki/.\n- Use [[wikilinks]] between related wiki pages.\n- Cite raw factual material with [@source_id].\n- Return only pages that should actually be created or updated.\n- Prefer concise, high-signal pages over verbose dumps.\n\nOptional page hint: {page_hint}\n\nSchema guide:\n{agents}\n\nExisting index:\n{index_page}\n\nRelated wiki context:\n{related_context}\n\nRaw sources:\n{}",
        source_sections.join("\n\n")
    );

    let schema_hint = r##"{
  "summary": "short explanation of what the ingest run will add or update",
  "pages": [
    {
      "path": "wiki/topics/example.md",
      "title": "Example",
      "content": "# Example\n\nPage body with [[links]] and [@source_id] citations."
    }
  ]
}"##;
    let json_schema = json!({
        "type": "object",
        "required": ["summary", "pages"],
        "properties": {
            "summary": { "type": "string" },
            "pages": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["path", "title", "content"],
                    "properties": {
                        "path": { "type": "string" },
                        "title": { "type": "string" },
                        "content": { "type": "string" }
                    }
                }
            }
        }
    });
    let response = providers::structured_json::<IngestModelResponse>(
        root,
        request.workflow_options.clone(),
        "You are a careful knowledge-base editor. Return only valid JSON.",
        &user_prompt,
        schema_hint,
        &json_schema,
    )?;

    let mut diffs = Vec::new();
    let mut changed_paths = Vec::new();
    for page in response.value.pages {
        let normalized_path = if page.path.trim().is_empty() {
            vault::normalize_wiki_path(&page.title)
        } else {
            vault::normalize_wiki_path(&page.path)
        };
        let before = vault::read_document(root, &normalized_path).ok();
        let after = if page.content.starts_with('#') {
            page.content
        } else {
            format!("# {}\n\n{}", page.title, page.content)
        };
        diffs.push(build_diff_entry(&normalized_path, before, Some(after)));
        changed_paths.push(normalized_path);
    }

    let mut details = response.metadata.as_json();
    merge_json_object(
        &mut details,
        json!({
            "page_hint": page_hint,
        }),
    );
    let now = now_iso();
    let job = JobRecord {
        id: format!("ingest-{}", Uuid::new_v4()),
        kind: "ingest".to_string(),
        status: "pending_review".to_string(),
        created_at: now.clone(),
        updated_at: now,
        summary: response.value.summary,
        source_ids: request.source_ids,
        changed_paths,
        diffs,
        details,
    };
    vault::save_job(root, &job)?;
    index::reindex_workspace(root)?;
    Ok(job)
}

pub fn apply_job(root: &Path, job_id: &str) -> Result<JobRecord> {
    let mut job = vault::load_job(root, job_id)?;
    apply_diffs_with_rollback(root, &job.diffs)?;

    vault::refresh_system_index(root)?;
    job.status = "applied".to_string();
    job.updated_at = now_iso();
    vault::save_job(root, &job)?;
    vault::append_log_entry(root, &job)?;
    index::reindex_workspace(root)?;
    Ok(job)
}

fn apply_diffs_with_rollback(root: &Path, diffs: &[DiffEntry]) -> Result<()> {
    let backups = diffs
        .iter()
        .map(|diff| {
            let absolute = root.join(&diff.path);
            let before = if absolute.exists() {
                Some(
                    fs::read_to_string(&absolute)
                        .with_context(|| format!("failed to read {}", absolute.display()))?,
                )
            } else {
                None
            };
            Ok((absolute, before))
        })
        .collect::<Result<Vec<_>>>()?;

    let result = (|| -> Result<()> {
        for (diff, (absolute, _before)) in diffs.iter().zip(backups.iter()) {
            if let Some(after) = diff.after.as_ref() {
                ensure_parent_dir(absolute)?;
                fs::write(absolute, after)
                    .with_context(|| format!("failed to write {}", absolute.display()))?;
            } else if absolute.exists() {
                fs::remove_file(absolute)
                    .with_context(|| format!("failed to delete {}", absolute.display()))?;
            }
        }
        Ok(())
    })();

    if let Err(error) = result {
        rollback_applied_diffs(&backups)?;
        return Err(error);
    }

    Ok(())
}

fn rollback_applied_diffs(backups: &[(PathBuf, Option<String>)]) -> Result<()> {
    for (path, before) in backups {
        match before {
            Some(content) => {
                ensure_parent_dir(path)?;
                fs::write(path, content)
                    .with_context(|| format!("failed to restore {}", path.display()))?;
            }
            None => {
                if path.exists() {
                    fs::remove_file(path)
                        .with_context(|| format!("failed to remove {}", path.display()))?;
                }
            }
        }
    }
    Ok(())
}

pub fn answer_question(root: &Path, request: QueryRequest) -> Result<QueryAnswer> {
    let results = index::search_documents(root, &request.prompt)?;
    let wiki_results = results
        .iter()
        .filter(|result| result.scope == "wiki" || result.scope == "system")
        .take(6)
        .cloned()
        .collect::<Vec<_>>();
    let raw_results = results
        .iter()
        .filter(|result| result.scope == "raw")
        .take(4)
        .cloned()
        .collect::<Vec<_>>();

    let context = build_related_context(root, &wiki_results)?;
    let raw_context = build_related_context(root, &raw_results)?;
    let workflow = normalize_workflow(root, &request.workflow_options)?;

    if request.save_as_page && workflow.provider_mode == "subscription" && workflow.execution_mode == "direct-edit" {
        let target = analysis_target_path(&request.prompt);
        let system = "You are answering a question against this wiki. You may edit only the requested analysis page under wiki/analyses/. Do not edit raw sources, application code, settings, or system/AGENTS.md. The app will refresh system/index.md and system/log.md after you finish. Final response: one short summary of the saved analysis page.";
        let user_prompt = format!(
            "Question:\n{}\n\nCreate or update this file:\n{}\n\nRules:\n- Write a concise markdown analysis with a top-level heading matching the question.\n- Prefer wiki context first, then raw sources when needed.\n- Cite raw-source claims with [@source_id].\n- Do not edit any file outside wiki/analyses/ unless absolutely necessary.\n\nWiki context:\n{}\n\nRaw source context:\n{}",
            request.prompt, target, context, raw_context
        );
        let job = execute_direct_edit(
            root,
            "query",
            Vec::new(),
            request.workflow_options.clone(),
            |root, options| providers::direct_edit(root, options, system, &user_prompt),
        )?;
        if job.status == "failed" {
            return Ok(QueryAnswer {
                answer: job.summary.clone(),
                citations: Vec::new(),
                saved_page: None,
                job: Some(job),
            });
        }

        let saved_content = vault::read_document(root, &target).unwrap_or_default();
        let answer = markdown_body_without_heading(&saved_content);
        let citations = citations_for_text(root, &saved_content)?;
        return Ok(QueryAnswer {
            answer,
            citations,
            saved_page: Some(target),
            job: Some(job),
        });
    }

    if request.save_as_page {
        let target = analysis_target_path(&request.prompt);
        let schema_hint = r##"{
  "answer": "direct answer text for the user",
  "page": {
    "path": "wiki/analyses/example.md",
    "title": "Question title",
    "content": "# Question title\n\nAnswer body with [@source_id] citations."
  }
}"##;
        let json_schema = json!({
            "type": "object",
            "required": ["answer", "page"],
            "properties": {
                "answer": { "type": "string" },
                "page": {
                    "type": "object",
                    "required": ["title", "content"],
                    "properties": {
                        "path": { "type": ["string", "null"] },
                        "title": { "type": "string" },
                        "content": { "type": "string" }
                    }
                }
            }
        });
        let prompt = format!(
            "Answer the user question using the wiki first and raw sources when needed.\n\nRules:\n- Keep the answer concise and grounded.\n- Cite raw-source claims with [@source_id].\n- If the wiki is thin, say so directly.\n- Propose a saved page under wiki/analyses/ that captures the same answer.\n\nQuestion:\n{}\n\nPreferred target path:\n{}\n\nWiki context:\n{}\n\nRaw source context:\n{}",
            request.prompt, target, context, raw_context
        );
        let response = providers::structured_json::<QuerySaveModelResponse>(
            root,
            request.workflow_options.clone(),
            "You answer questions against a personal wiki and prepare reviewable markdown diffs. Return only valid JSON.",
            &prompt,
            schema_hint,
            &json_schema,
        )?;
        let target_path = response
            .value
            .page
            .path
            .as_deref()
            .map(vault::normalize_wiki_path)
            .unwrap_or_else(|| target.clone());
        let before = vault::read_document(root, &target_path).ok();
        let after = if response.value.page.content.starts_with('#') {
            response.value.page.content
        } else {
            format!("# {}\n\n{}", response.value.page.title, response.value.page.content)
        };
        let diff = build_diff_entry(&target_path, before, Some(after.clone()));
        let citations = citations_for_text(root, &after)?;
        let now = now_iso();
        let mut details = response.metadata.as_json();
        merge_json_object(
            &mut details,
            json!({
                "results_considered": results.iter().map(|result| result.path.clone()).collect::<Vec<_>>(),
            }),
        );
        let job = JobRecord {
            id: format!("query-{}", Uuid::new_v4()),
            kind: "query".to_string(),
            status: "pending_review".to_string(),
            created_at: now.clone(),
            updated_at: now,
            summary: request.prompt.clone(),
            source_ids: citations.iter().map(|citation| citation.source_id.clone()).collect(),
            changed_paths: vec![target_path.clone()],
            diffs: vec![diff],
            details,
        };
        vault::save_job(root, &job)?;
        index::reindex_workspace(root)?;
        return Ok(QueryAnswer {
            answer: response.value.answer,
            citations,
            saved_page: None,
            job: Some(job),
        });
    }

    let prompt = format!(
        "Answer the user question using the wiki first and raw sources when needed.\n\nRules:\n- Keep the answer concise and grounded.\n- Cite raw-source claims with [@source_id].\n- If the wiki is thin, say so directly.\n\nQuestion:\n{}\n\nWiki context:\n{}\n\nRaw source context:\n{}",
        request.prompt, context, raw_context
    );
    let answer = match providers::chat(
        root,
        request.workflow_options.clone(),
        "You answer questions against a personal wiki. Prefer precision over breadth.",
        &prompt,
    ) {
        Ok(answer) => answer,
        Err(_) => {
            let fallback = fallback_answer(&results);
            let citations = citations_for_text(root, &fallback)?;
            return Ok(QueryAnswer {
                answer: fallback,
                citations,
                saved_page: None,
                job: None,
            });
        }
    };

    let citations = citations_for_text(root, &answer.text)?;
    let now = now_iso();
    let mut details = answer.metadata.as_json();
    merge_json_object(
        &mut details,
        json!({
            "results_considered": results.iter().map(|result| result.path.clone()).collect::<Vec<_>>(),
        }),
    );
    let job = JobRecord {
        id: format!("query-{}", Uuid::new_v4()),
        kind: "query".to_string(),
        status: "completed".to_string(),
        created_at: now.clone(),
        updated_at: now,
        summary: request.prompt.clone(),
        source_ids: citations.iter().map(|citation| citation.source_id.clone()).collect(),
        changed_paths: Vec::new(),
        diffs: Vec::new(),
        details,
    };
    vault::save_job(root, &job)?;
    vault::append_log_entry(root, &job)?;
    index::reindex_workspace(root)?;

    Ok(QueryAnswer {
        answer: answer.text,
        citations,
        saved_page: None,
        job: Some(job),
    })
}

pub fn lint_wiki(root: &Path, request: LintRequest) -> Result<LintReport> {
    index::reindex_workspace(root)?;
    let structural_findings = collect_structural_lint_findings(root)?;
    let findings_context = if structural_findings.is_empty() {
        "No structural findings were detected.".to_string()
    } else {
        structural_findings
            .iter()
            .map(|finding| {
                format!(
                    "- severity: {}\n  code: {}\n  path: {}\n  message: {}",
                    finding.severity,
                    finding.code,
                    finding.path.clone().unwrap_or_else(|| "n/a".to_string()),
                    finding.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let agents = vault::read_document(root, "system/AGENTS.md").unwrap_or_default();
    let index_page = vault::read_document(root, "system/index.md").unwrap_or_default();
    let schema_hint = r##"{
  "summary": "short summary of the lint pass",
  "findings": [
    {
      "severity": "warning",
      "code": "broken-link",
      "message": "Broken wikilink [[foo]]",
      "path": "wiki/topics/example.md"
    }
  ]
}"##;
    let json_schema = json!({
        "type": "object",
        "required": ["summary", "findings"],
        "properties": {
            "summary": { "type": "string" },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["severity", "code", "message"],
                    "properties": {
                        "severity": { "type": "string" },
                        "code": { "type": "string" },
                        "message": { "type": "string" },
                        "path": { "type": ["string", "null"] }
                    }
                }
            }
        }
    });
    let prompt = format!(
        "Review this wiki lint context and return the most useful actionable findings.\n\nRules:\n- Preserve concrete file paths when available.\n- Keep findings concise.\n- You may add missing concept/entity coverage and weak cross-linking issues if the evidence supports them.\n- Do not propose fixes, only findings.\n\nSchema guide:\n{}\n\nIndex:\n{}\n\nExisting structural findings:\n{}",
        agents, index_page, findings_context
    );

    let (summary, findings, details) = match providers::structured_json::<LintModelResponse>(
        root,
        request.workflow_options.clone(),
        "You are a wiki linting assistant. Return only valid JSON.",
        &prompt,
        schema_hint,
        &json_schema,
    ) {
        Ok(response) => (
            response.value.summary,
            response.value.findings,
            response.metadata.as_json(),
        ),
        Err(error) => (
            format!("Linted wiki with {} findings", structural_findings.len()),
            structural_findings.clone(),
            json!({
                "provider_id": "fallback",
                "provider_kind": "fallback",
                "provider_mode": normalize_workflow(root, &request.workflow_options)?.provider_mode,
                "execution_mode": normalize_workflow(root, &request.workflow_options)?.execution_mode,
                "provider_error": error.to_string(),
                "fallback": "structural-lint"
            }),
        ),
    };

    let now = now_iso();
    let mut job = JobRecord {
        id: format!("lint-{}", Uuid::new_v4()),
        kind: "lint".to_string(),
        status: "completed".to_string(),
        created_at: now.clone(),
        updated_at: now,
        summary,
        source_ids: Vec::new(),
        changed_paths: Vec::new(),
        diffs: Vec::new(),
        details,
    };
    merge_json_object(&mut job.details, json!({ "finding_count": findings.len() }));
    vault::save_job(root, &job)?;
    vault::append_log_entry(root, &job)?;
    index::reindex_workspace(root)?;

    Ok(LintReport { findings, job })
}

fn build_source_sections(root: &Path, sources: &[crate::models::SourceRecord]) -> Result<Vec<String>> {
    let mut sections = Vec::new();
    for source in sources {
        let content = vault::read_document(root, &source.canonical_path)
            .with_context(|| format!("failed to read {}", source.canonical_path))?;
        sections.push(format!(
            "## Source {}\nsource_id: {}\npath: {}\ntitle: {}\n\n{}",
            source.title,
            source.source_id,
            source.canonical_path,
            source.title,
            truncate_for_model(&content, 7000)
        ));
    }
    Ok(sections)
}

fn related_pages_for_sources(root: &Path, sources: &[crate::models::SourceRecord]) -> Result<String> {
    let query_seed = sources
        .iter()
        .map(|source| source.title.clone())
        .collect::<Vec<_>>()
        .join(" ");
    let related_pages = index::search_documents(root, &query_seed)?
        .into_iter()
        .filter(|result| result.scope == "wiki" || result.scope == "system")
        .take(8)
        .collect::<Vec<_>>();
    build_related_context(root, &related_pages)
}

fn collect_structural_lint_findings(root: &Path) -> Result<Vec<LintFinding>> {
    let conn = Connection::open(vault::manifest_db_path(root))?;
    let mut findings = Vec::new();

    let mut broken_links = conn.prepare(
        "SELECT from_path, raw_target
         FROM links
         WHERE from_path LIKE 'wiki/%' AND resolved_path IS NULL",
    )?;
    for row in broken_links.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
        let (from_path, raw_target) = row?;
        findings.push(LintFinding {
            severity: "warning".to_string(),
            code: "broken-link".to_string(),
            message: format!("Broken wikilink `[[{}]]`", raw_target),
            path: Some(from_path),
        });
    }

    let mut weak_links = conn.prepare(
        "SELECT d.path, d.content
         FROM documents d
         LEFT JOIN links l ON l.from_path = d.path
         WHERE d.scope = 'wiki'
         GROUP BY d.path
         HAVING COUNT(l.raw_target) = 0",
    )?;
    for row in weak_links.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
        let (path, _content) = row?;
        findings.push(LintFinding {
            severity: "info".to_string(),
            code: "weak-cross-linking".to_string(),
            message: "Page has no outgoing wikilinks.".to_string(),
            path: Some(path),
        });
    }

    let mut orphan_pages = conn.prepare(
        "SELECT d.path
         FROM documents d
         LEFT JOIN links inbound ON inbound.resolved_path = d.path
         LEFT JOIN links outbound ON outbound.from_path = d.path
         WHERE d.scope = 'wiki'
         GROUP BY d.path
         HAVING COUNT(inbound.from_path) = 0 AND COUNT(outbound.raw_target) = 0",
    )?;
    for row in orphan_pages.query_map([], |row| row.get::<_, String>(0))? {
        findings.push(LintFinding {
            severity: "warning".to_string(),
            code: "orphan-page".to_string(),
            message: "Page has no incoming or outgoing wiki links.".to_string(),
            path: Some(row?),
        });
    }

    let mut uncited = conn.prepare(
        "SELECT path, content
         FROM documents
         WHERE scope = 'wiki'",
    )?;
    for row in uncited.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))? {
        let (path, content) = row?;
        if has_uncited_claims(&content) {
            findings.push(LintFinding {
                severity: "warning".to_string(),
                code: "uncited-claims".to_string(),
                message: "Page contains narrative lines without any raw-source citations.".to_string(),
                path: Some(path),
            });
        }
    }

    let stale_sql = "
        SELECT d.path, d.modified_at, s.updated_at
        FROM documents d
        JOIN citations c ON c.from_path = d.path
        JOIN source_records s ON s.source_id = c.source_id
        WHERE d.scope = 'wiki'
        GROUP BY d.path
        HAVING MAX(s.updated_at) > d.modified_at
    ";
    let mut stale_stmt = conn.prepare(stale_sql)?;
    for row in stale_stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (path, page_time, source_time) = row?;
        findings.push(LintFinding {
            severity: "info".to_string(),
            code: "stale-source-window".to_string(),
            message: format!(
                "Cited raw source changed after the wiki page ({source_time} > {page_time})."
            ),
            path: Some(path),
        });
    }

    Ok(findings)
}

fn execute_direct_edit<F>(
    root: &Path,
    kind: &str,
    source_ids: Vec<String>,
    workflow_options: WorkflowOptions,
    run: F,
) -> Result<JobRecord>
where
    F: FnOnce(&Path, WorkflowOptions) -> Result<providers::TextExecutionResult>,
{
    let before = capture_repo_snapshot(root)?;
    let result = match run(root, workflow_options.clone()) {
        Ok(result) => result,
        Err(error) => {
            let after = capture_repo_snapshot(root).unwrap_or_default();
            let _ = restore_snapshot(root, &before, &after);
            return Err(error);
        }
    };
    let after = capture_repo_snapshot(root)?;
    let changed_paths = changed_paths(&before, &after);
    let disallowed_paths = changed_paths
        .iter()
        .filter(|path| !is_allowed_direct_edit_path(path))
        .cloned()
        .collect::<Vec<_>>();

    if !disallowed_paths.is_empty() {
        restore_snapshot(root, &before, &after)?;
        let now = now_iso();
        let mut details = result.metadata.as_json();
        merge_json_object(
            &mut details,
            json!({
                "disallowed_paths": disallowed_paths,
                "guarded_write_result": "reverted"
            }),
        );
        let job = JobRecord {
            id: format!("{kind}-{}", Uuid::new_v4()),
            kind: kind.to_string(),
            status: "failed".to_string(),
            created_at: now.clone(),
            updated_at: now,
            summary: format!(
                "Aborted {kind} run because the agent attempted to edit protected files."
            ),
            source_ids,
            changed_paths: Vec::new(),
            diffs: Vec::new(),
            details,
        };
        vault::save_job(root, &job)?;
        vault::append_log_entry(root, &job)?;
        index::reindex_workspace(root)?;
        return Ok(job);
    }

    let diffs = changed_paths
        .iter()
        .filter(|path| is_allowed_direct_edit_path(path))
        .map(|path| {
            build_diff_entry(
                path,
                before
                    .get(path)
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
                after
                    .get(path)
                    .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
            )
        })
        .collect::<Vec<_>>();
    let changed_paths = diffs.iter().map(|diff| diff.path.clone()).collect::<Vec<_>>();

    let now = now_iso();
    let mut details = result.metadata.as_json();
    merge_json_object(&mut details, json!({ "guarded_write_result": "applied" }));
    let job = JobRecord {
        id: format!("{kind}-{}", Uuid::new_v4()),
        kind: kind.to_string(),
        status: "applied".to_string(),
        created_at: now.clone(),
        updated_at: now,
        summary: if result.text.trim().is_empty() {
            format!("Applied {kind} changes")
        } else {
            result.text.trim().to_string()
        },
        source_ids,
        changed_paths,
        diffs,
        details,
    };
    vault::save_job(root, &job)?;
    vault::refresh_system_index(root)?;
    vault::append_log_entry(root, &job)?;
    index::reindex_workspace(root)?;
    Ok(job)
}

fn capture_repo_snapshot(root: &Path) -> Result<HashMap<String, Vec<u8>>> {
    let mut snapshot = HashMap::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        let path = entry.path();
        if should_skip_snapshot_path(root, path) {
            if entry.file_type().is_dir() {
                continue;
            }
            continue;
        }
        if entry.file_type().is_file() {
            let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
            snapshot.insert(relative, fs::read(path)?);
        }
    }
    Ok(snapshot)
}

fn should_skip_snapshot_path(root: &Path, path: &Path) -> bool {
    if path == root {
        return false;
    }
    let relative = match path.strip_prefix(root) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => return true,
    };
    matches!(
        relative.as_str(),
        ".git"
            | ".wiki"
            | "node_modules"
            | "dist"
            | "src-tauri/target"
    ) || relative.starts_with(".git/")
        || relative.starts_with(".wiki/")
        || relative.starts_with("node_modules/")
        || relative.starts_with("dist/")
        || relative.starts_with("src-tauri/target/")
}

fn restore_snapshot(
    root: &Path,
    before: &HashMap<String, Vec<u8>>,
    after: &HashMap<String, Vec<u8>>,
) -> Result<()> {
    let all_paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for path in all_paths {
        let absolute = root.join(&path);
        match before.get(&path) {
            Some(bytes) => {
                ensure_parent_dir(&absolute)?;
                fs::write(&absolute, bytes)?;
            }
            None => {
                if absolute.exists() {
                    fs::remove_file(&absolute)?;
                }
            }
        }
    }
    Ok(())
}

fn changed_paths(
    before: &HashMap<String, Vec<u8>>,
    after: &HashMap<String, Vec<u8>>,
) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .collect()
}

fn is_allowed_direct_edit_path(path: &str) -> bool {
    path.starts_with("wiki/") || path == "system/index.md" || path == "system/log.md"
}

fn build_diff_entry(path: &str, before: Option<String>, after: Option<String>) -> DiffEntry {
    let diff = TextDiff::from_lines(before.as_deref().unwrap_or(""), after.as_deref().unwrap_or(""))
        .unified_diff()
        .header("before", "after")
        .to_string();
    DiffEntry {
        path: path.to_string(),
        before,
        after,
        diff,
    }
}

fn analysis_target_path(prompt: &str) -> String {
    let file_name = slugify(prompt);
    format!(
        "wiki/analyses/{}.md",
        if file_name.is_empty() { "analysis" } else { &file_name }
    )
}

fn markdown_body_without_heading(content: &str) -> String {
    let mut lines = content.lines();
    let first = lines.next().unwrap_or_default();
    if first.starts_with("# ") {
        let rest = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        if rest.is_empty() {
            first.trim_start_matches("# ").trim().to_string()
        } else {
            rest
        }
    } else {
        content.trim().to_string()
    }
}

fn build_related_context(root: &Path, results: &[SearchResult]) -> Result<String> {
    if results.is_empty() {
        return Ok("No related pages.".to_string());
    }

    let mut blocks = Vec::new();
    for result in results {
        let content = vault::read_document(root, &result.path).unwrap_or_else(|_| result.excerpt.clone());
        blocks.push(format!(
            "### {}\npath: {}\nsource_id: {}\n\n{}",
            result.title,
            result.path,
            result.source_id.clone().unwrap_or_default(),
            truncate_for_model(&content, 2500)
        ));
    }
    Ok(blocks.join("\n\n"))
}

fn truncate_for_model(content: &str, limit: usize) -> String {
    if content.len() <= limit {
        content.to_string()
    } else {
        format!("{}…", &content[..limit])
    }
}

fn citations_for_text(root: &Path, text: &str) -> Result<Vec<CitationReference>> {
    let regex = Regex::new(r"\[@([A-Za-z0-9:_\-]+)\]")?;
    let unique = regex
        .captures_iter(text)
        .filter_map(|captures| captures.get(1).map(|capture| capture.as_str().to_string()))
        .collect::<HashSet<_>>();

    let ids = unique.into_iter().collect::<Vec<_>>();
    let records = index::source_records_by_ids(root, &ids)?;
    Ok(records
        .into_iter()
        .map(|record| CitationReference {
            source_id: record.source_id,
            title: record.title,
            path: record.canonical_path,
            origin: record.origin,
            status: record.status,
        })
        .collect())
}

fn fallback_answer(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "No indexed pages matched the query, and no model provider is configured.".to_string();
    }

    let mut lines = vec![
        "A model provider is not configured, so this is a search-based fallback.".to_string(),
        String::new(),
    ];
    for result in results.iter().take(6) {
        lines.push(format!(
            "- {} (`{}`): {}",
            result.title, result.path, result.excerpt
        ));
    }
    lines.join("\n")
}

fn has_uncited_claims(content: &str) -> bool {
    let mut narrative_lines = 0usize;
    let mut cited_lines = 0usize;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("```") {
            continue;
        }
        if trimmed.chars().filter(|ch| ch.is_alphabetic()).count() < 10 {
            continue;
        }
        narrative_lines += 1;
        if trimmed.contains("[@") {
            cited_lines += 1;
        }
    }

    narrative_lines >= 3 && cited_lines == 0
}

fn normalize_workflow(root: &Path, options: &WorkflowOptions) -> Result<NormalizedWorkflow> {
    let settings = vault::load_settings(root)?;
    Ok(NormalizedWorkflow {
        provider_mode: match options
            .provider_mode
            .as_deref()
            .unwrap_or(settings.provider_mode.as_str())
        {
            "api" => "api".to_string(),
            _ => "subscription".to_string(),
        },
        execution_mode: match options
            .execution_mode
            .as_deref()
            .unwrap_or(settings.default_execution_mode.as_str())
        {
            "direct-edit" => "direct-edit".to_string(),
            _ => "review-first".to_string(),
        },
    })
}

fn merge_json_object(target: &mut Value, incoming: Value) {
    let Some(target_object) = target.as_object_mut() else {
        *target = incoming;
        return;
    };
    if let Value::Object(incoming_object) = incoming {
        for (key, value) in incoming_object {
            target_object.insert(key, value);
        }
    }
}

#[derive(Debug)]
struct NormalizedWorkflow {
    provider_mode: String,
    execution_mode: String,
}
