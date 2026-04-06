import type { SearchResult, TreeNode } from "../types";

type SidebarProps = {
  tree: TreeNode[];
  currentPath?: string;
  selectedSourceIds: string[];
  searchValue: string;
  searchResults: SearchResult[];
  onSearchChange: (value: string) => void;
  onSelectPath: (path: string) => void;
  onToggleSource: (sourceId: string) => void;
};

export function Sidebar({
  tree,
  currentPath,
  selectedSourceIds,
  searchValue,
  searchResults,
  onSearchChange,
  onSelectPath,
  onToggleSource,
}: SidebarProps) {
  return (
    <aside className="sidebar">
      <div className="sidebar-search">
        <p className="eyebrow">Navigator</p>
        <input
          className="search-input"
          onChange={(event) => onSearchChange(event.currentTarget.value)}
          placeholder="Search wiki, raw, system"
          value={searchValue}
        />
      </div>

      {searchValue.trim() ? (
        <section className="search-results">
          {searchResults.length ? (
            searchResults.map((result) => (
              <button
                className="search-result"
                key={result.path}
                onClick={() => onSelectPath(result.path)}
                type="button"
              >
                <strong>{result.title}</strong>
                <span>{result.path}</span>
                <p dangerouslySetInnerHTML={{ __html: result.excerpt }} />
              </button>
            ))
          ) : (
            <p className="sidebar-empty">No matches.</p>
          )}
        </section>
      ) : (
        <div className="tree-list">
          {tree.map((node) => (
            <TreeBranch
              currentPath={currentPath}
              key={node.path}
              node={node}
              onSelectPath={onSelectPath}
              onToggleSource={onToggleSource}
              selectedSourceIds={selectedSourceIds}
            />
          ))}
        </div>
      )}
    </aside>
  );
}

type TreeBranchProps = {
  node: TreeNode;
  currentPath?: string;
  selectedSourceIds: string[];
  onSelectPath: (path: string) => void;
  onToggleSource: (sourceId: string) => void;
};

function TreeBranch({
  node,
  currentPath,
  selectedSourceIds,
  onSelectPath,
  onToggleSource,
}: TreeBranchProps) {
  const isDirectory = node.kind === "directory";
  const isActive = currentPath === node.path;
  const isSelectedSource =
    node.source_id && selectedSourceIds.includes(node.source_id);

  return (
    <div className={`tree-node ${isActive ? "active" : ""}`}>
      <div className="tree-row">
        {node.source_id ? (
          <input
            checked={Boolean(isSelectedSource)}
            onChange={() => onToggleSource(node.source_id!)}
            type="checkbox"
          />
        ) : (
          <span className="tree-spacer" />
        )}

        <button
          className={`tree-label ${isDirectory ? "directory" : ""}`}
          onClick={() => (isDirectory ? undefined : onSelectPath(node.path))}
          type="button"
        >
          <span>{node.name}</span>
          {!isDirectory && node.writable ? <em>editable</em> : null}
        </button>
      </div>

      {node.children.length ? (
        <div className="tree-children">
          {node.children.map((child) => (
            <TreeBranch
              currentPath={currentPath}
              key={child.path}
              node={child}
              onSelectPath={onSelectPath}
              onToggleSource={onToggleSource}
              selectedSourceIds={selectedSourceIds}
            />
          ))}
        </div>
      ) : null}
    </div>
  );
}
