# AGENTS

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
- `system/index.md` is generated from the current `wiki/` tree and refreshed after writes.
- `system/log.md` is append-only and records imports, ingest runs, query saves, and lint passes.
- Raw files are read-only in the UI.
