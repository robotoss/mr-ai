-- Tracks the last fully-indexed commit per repo. Used by the incremental
-- delta updater (S4) to compute the diff between what is in Qdrant/graph and
-- the current master HEAD.

CREATE TABLE IF NOT EXISTS index_state (
    repo_id           UUID PRIMARY KEY REFERENCES project_repos(id) ON DELETE CASCADE,
    last_indexed_sha  TEXT,
    last_indexed_at   TIMESTAMPTZ,
    last_error        TEXT
);
