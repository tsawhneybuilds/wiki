import type { CitationReference, DocumentDetails, JobRecord, LinkReference } from "../types";

type InspectorPaneProps = {
  document?: DocumentDetails | null;
  jobs: JobRecord[];
  onNavigate: (path: string) => void;
  onOpenJob: (job: JobRecord) => void;
};

export function InspectorPane({
  document,
  jobs,
  onNavigate,
  onOpenJob,
}: InspectorPaneProps) {
  const currentJob = jobs.find((job) => job.status === "pending_review");

  return (
    <aside className="inspector">
      <section className="inspector-card">
        <div className="inspector-header">
          <p className="eyebrow">Context</p>
          <h3>Backlinks & citations</h3>
        </div>
        {document ? (
          <>
            <LinkList
              links={document.backlinks}
              onNavigate={onNavigate}
              title="Backlinks"
            />
            <LinkList
              links={document.outgoing_links}
              onNavigate={onNavigate}
              title="Outgoing links"
            />
            <CitationList citations={document.citations} onNavigate={onNavigate} />
            <GraphPreview document={document} onNavigate={onNavigate} />
          </>
        ) : (
          <p className="inspector-empty">Open a document to inspect it.</p>
        )}
      </section>

      <section className="inspector-card">
        <div className="inspector-header">
          <p className="eyebrow">Jobs</p>
          <h3>Recent runs</h3>
        </div>
        {currentJob ? (
          <button
            className="job-highlight"
            onClick={() => onOpenJob(currentJob)}
            type="button"
          >
            <strong>{currentJob.summary}</strong>
            <span>{currentJob.kind}</span>
            <em>{currentJob.status}</em>
          </button>
        ) : null}
        <div className="job-list">
          {jobs.map((job) => (
            <button
              className="job-item"
              key={job.id}
              onClick={() => onOpenJob(job)}
              type="button"
            >
              <strong>{job.kind}</strong>
              <span>{job.summary}</span>
              <em>{job.status}</em>
            </button>
          ))}
        </div>
      </section>
    </aside>
  );
}

function LinkList({
  title,
  links,
  onNavigate,
}: {
  title: string;
  links: LinkReference[];
  onNavigate: (path: string) => void;
}) {
  return (
    <div className="inspector-block">
      <h4>{title}</h4>
      {links.length ? (
        links.map((link) => (
          <button
            className="plain-link"
            key={`${title}-${link.path}`}
            onClick={() => onNavigate(link.path)}
            type="button"
          >
            <strong>{link.title}</strong>
            <span>{link.path}</span>
          </button>
        ))
      ) : (
        <p className="inspector-empty">None.</p>
      )}
    </div>
  );
}

function CitationList({
  citations,
  onNavigate,
}: {
  citations: CitationReference[];
  onNavigate: (path: string) => void;
}) {
  return (
    <div className="inspector-block">
      <h4>Citations</h4>
      {citations.length ? (
        citations.map((citation) => (
          <button
            className="plain-link"
            key={citation.source_id}
            onClick={() => onNavigate(citation.path)}
            type="button"
          >
            <strong>{citation.title}</strong>
            <span>{citation.source_id}</span>
          </button>
        ))
      ) : (
        <p className="inspector-empty">No raw-source citations yet.</p>
      )}
    </div>
  );
}

function GraphPreview({
  document,
  onNavigate,
}: {
  document: DocumentDetails;
  onNavigate: (path: string) => void;
}) {
  const nodes = [
    { label: document.title, path: document.path, x: 120, y: 92, center: true },
    ...document.backlinks.slice(0, 4).map((link, index) => {
      const angle = (Math.PI * (index + 1)) / 3;
      return {
        label: link.title,
        path: link.path,
        x: 120 - Math.cos(angle) * 90,
        y: 92 - Math.sin(angle) * 66,
        center: false,
      };
    }),
    ...document.outgoing_links.slice(0, 4).map((link, index) => {
      const angle = (Math.PI * (index + 1)) / 3;
      return {
        label: link.title,
        path: link.path,
        x: 120 + Math.cos(angle) * 90,
        y: 92 + Math.sin(angle) * 66,
        center: false,
      };
    }),
  ];

  return (
    <div className="inspector-block">
      <h4>Graph</h4>
      <svg className="graph-preview" viewBox="0 0 240 184">
        {nodes
          .filter((node) => !node.center)
          .map((node) => (
            <line
              key={`edge-${node.path}`}
              stroke="rgba(34, 76, 118, 0.28)"
              strokeWidth="1.5"
              x1={120}
              x2={node.x}
              y1={92}
              y2={node.y}
            />
          ))}
        {nodes.map((node) => (
          <g key={node.path}>
            <circle
              className={node.center ? "graph-node center" : "graph-node"}
              cx={node.x}
              cy={node.y}
              onClick={() => onNavigate(node.path)}
              r={node.center ? 18 : 12}
            />
            <text x={node.x} y={node.y + 28}>
              {node.label.slice(0, 16)}
            </text>
          </g>
        ))}
      </svg>
    </div>
  );
}
