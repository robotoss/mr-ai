-- Background job queue. Workers claim rows via SELECT ... FOR UPDATE SKIP LOCKED.

CREATE TABLE IF NOT EXISTS jobs (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id    UUID REFERENCES projects(id) ON DELETE SET NULL,
    kind          TEXT NOT NULL,
    payload       JSONB NOT NULL,
    status        TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued','running','done','failed','dead')),
    attempt       INTEGER NOT NULL DEFAULT 0,
    max_attempts  INTEGER NOT NULL DEFAULT 5,
    run_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_at     TIMESTAMPTZ,
    locked_by     TEXT,
    last_error    TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at   TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS jobs_pickup_idx
    ON jobs(status, run_at)
    WHERE status = 'queued';

CREATE INDEX IF NOT EXISTS jobs_project_id_idx
    ON jobs(project_id);
