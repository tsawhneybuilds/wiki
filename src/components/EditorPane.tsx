import { autocompletion, type CompletionContext } from "@codemirror/autocomplete";
import { markdown } from "@codemirror/lang-markdown";
import type { ReactNode } from "react";
import CodeMirror from "@uiw/react-codemirror";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { CitationReference, DocumentDetails } from "../types";

type EditorPaneProps = {
  document?: DocumentDetails | null;
  draftContent: string;
  linkTargets: string[];
  onDraftChange: (value: string) => void;
  onNavigate: (path: string) => void;
};

export function EditorPane({
  document,
  draftContent,
  linkTargets,
  onDraftChange,
  onNavigate,
}: EditorPaneProps) {
  if (!document) {
    return (
      <section className="editor-pane empty-pane">
        <p className="eyebrow">Editor</p>
        <h2>Select a page to begin.</h2>
      </section>
    );
  }

  const citationMap = Object.fromEntries(
    document.citations.map((citation) => [citation.source_id, citation]),
  );

  return (
    <section className="editor-pane">
      <header className="pane-header">
        <div>
          <p className="eyebrow">{document.scope}</p>
          <h2>{document.title}</h2>
        </div>
        <div className="path-badge">
          <span>{document.path}</span>
          <strong>{document.writable ? "wiki page" : "read-only"}</strong>
        </div>
      </header>

      <div className="editor-grid">
        <div className="editor-column">
          <div className="editor-surface">
            <CodeMirror
              basicSetup={{
                lineNumbers: true,
                highlightActiveLine: true,
                foldGutter: false,
              }}
              editable={document.writable}
              extensions={[
                markdown(),
                autocompletion({
                  override: [
                    (context) => completeWikiLink(context, linkTargets),
                  ],
                }),
              ]}
              height="100%"
              onChange={onDraftChange}
              value={draftContent}
            />
          </div>
        </div>

        <div className="preview-column">
          <div className="preview-surface markdown-body">
            <ReactMarkdown
              components={{
                a: ({ children, href }) => (
                  <LinkRenderer
                    citationMap={citationMap}
                    href={href}
                    onNavigate={onNavigate}
                  >
                    {children}
                  </LinkRenderer>
                ),
              }}
              remarkPlugins={[remarkGfm]}
            >
              {decorateMarkdown(draftContent)}
            </ReactMarkdown>
          </div>
        </div>
      </div>
    </section>
  );
}

function completeWikiLink(context: CompletionContext, linkTargets: string[]) {
  const match = context.matchBefore(/\[\[[^\]]*/);
  if (!match) {
    return null;
  }

  const typed = match.text.replace("[[", "").toLowerCase();
  const options = linkTargets
    .filter((target) => target.toLowerCase().includes(typed))
    .slice(0, 20)
    .map((target) => ({
      label: target,
      type: "keyword",
      apply: `[[${target}]]`,
    }));

  return {
    from: match.from,
    options,
  };
}

function decorateMarkdown(markdownText: string) {
  return markdownText
    .replace(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g, (_, target, label) => {
      const text = label || target;
      return `[${text}](wikilink:${target})`;
    })
    .replace(/\[@([A-Za-z0-9:_-]+)\]/g, (_, sourceId) => {
      return `[${sourceId}](source:${sourceId})`;
    });
}

function LinkRenderer({
  children,
  href,
  onNavigate,
  citationMap,
}: {
  children: ReactNode;
  href?: string;
  onNavigate: (path: string) => void;
  citationMap: Record<string, CitationReference>;
}) {
  if (!href) {
    return <span>{children}</span>;
  }

  if (href.startsWith("wikilink:")) {
    const target = href.replace("wikilink:", "");
    return (
      <button
        className="inline-link"
        onClick={() => onNavigate(target)}
        type="button"
      >
        {children}
      </button>
    );
  }

  if (href.startsWith("source:")) {
    const sourceId = href.replace("source:", "");
    const target = citationMap[sourceId]?.path;
    return (
      <button
        className="inline-link"
        onClick={() => target && onNavigate(target)}
        type="button"
      >
        {children}
      </button>
    );
  }

  return (
    <a href={href} rel="noreferrer" target="_blank">
      {children}
    </a>
  );
}
