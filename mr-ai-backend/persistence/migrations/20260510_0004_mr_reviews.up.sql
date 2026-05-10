-- Per-MR review state and the bundle that fed it.

CREATE TABLE IF NOT EXISTS mr_reviews (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    primary_repo_id UUID NOT NULL REFERENCES project_repos(id) ON DELETE CASCADE,
    mr_iid          TEXT NOT NULL,
    status          TEXT NOT NULL
        CHECK (status IN ('pending','running','published','failed')),
    bundle          JSONB NOT NULL,
    started_at      TIMESTAMPTZ,
    finished_at     TIMESTAMPTZ,
    UNIQUE (primary_repo_id, mr_iid)
);

CREATE INDEX IF NOT EXISTS mr_reviews_project_status_idx
    ON mr_reviews(project_id, status);
