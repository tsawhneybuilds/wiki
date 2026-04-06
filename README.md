# Tanush Wiki

macOS-first Tauri app for building an LLM-maintained markdown wiki on top of immutable raw imports from Notion and Apple Notes.

## Architecture

- `raw/`: imported source material. Notion exports land in `raw/notion/`; Apple Notes land in `raw/apple-notes/`; extracted note assets land in `raw/assets/`.
- `wiki/`: editable markdown knowledge base.
- `system/`: schema and operational docs. `AGENTS.md` defines citation and page conventions; `index.md` is generated; `log.md` is append-only.
- `.wiki/`: derived and operational metadata, including `settings.json`, `jobs/`, `sources/`, and the rebuildable SQLite manifest `manifest.db`.
- `src/`: React/TypeScript desktop UI.
- `src-tauri/`: Rust backend, importers, indexing, provider adapters, and Tauri config.

## Core Features

- Three-pane desktop UI with source/wiki tree, split markdown editor/preview, and backlinks/citation/job inspector.
- Notion ZIP import from official `Markdown & CSV` exports with idempotent source records.
- Apple Notes import via JXA automation, markdown conversion, extracted embedded assets, and HTML fallback preservation.
- SQLite FTS index for search, backlinks, link graph edges, citations, and job history.
- Provider-agnostic AI layer with OpenAI, Anthropic, and generic OpenAI-compatible endpoints, with API keys stored in macOS Keychain.
- Review-first ingest flow: model proposes page patches, the app shows diffs, and writes happen only after approval.

## Development

```bash
npm install
npm run build
npm run tauri build -- --debug
```

Useful commands:

- `npm run dev`: Vite frontend only.
- `npm run tauri dev`: run the full desktop app in development.
- `npm run tauri build -- --debug`: build the native app bundle.
- `cd src-tauri && cargo check`: Rust-only verification.

## Verification

Implemented and verified locally with:

- `npm run build`
- `cd src-tauri && cargo check`
- `npm run tauri build -- --debug`

The Tauri build produced:

- `/Users/tanushsawhney/Documents/tanushwiki/src-tauri/target/debug/tanushwiki`
- `/Users/tanushsawhney/Documents/tanushwiki/src-tauri/target/debug/bundle/macos/Tanush Wiki.app`

The DMG packaging script started but was manually stopped after the `.app` bundle was already produced.
