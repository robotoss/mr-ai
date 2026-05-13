-- Code graph: nodes (definitions, files, packages).

CREATE TABLE IF NOT EXISTS graph_nodes (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id         UUID NOT NULL REFERENCES project_repos(id) ON DELETE CASCADE,
    -- Stable identity within (repo_id, file). Used for upsert and to keep
    -- IDs stable across re-indexing of unchanged code.
    fqn             TEXT NOT NULL,
    kind            TEXT NOT NULL,
    file            TEXT NOT NULL,
    symbol          TEXT NOT NULL,
    language        TEXT NOT NULL,
    -- Hash of the chunk body that defined the node (S4 incremental indexer
    -- compares this to detect actual content change vs metadata-only edits).
    content_sha256  TEXT,
    span_start      INTEGER,
    span_end        INTEGER,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, fqn)
);

CREATE INDEX IF NOT EXISTS graph_nodes_repo_kind_idx
    ON graph_nodes(repo_id, kind);
CREATE INDEX IF NOT EXISTS graph_nodes_file_idx
    ON graph_nodes(repo_id, file);
CREATE INDEX IF NOT EXISTS graph_nodes_symbol_idx
    ON graph_nodes(symbol);
