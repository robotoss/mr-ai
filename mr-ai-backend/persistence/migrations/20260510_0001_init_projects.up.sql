-- Project group / repo / dependency model.

CREATE TABLE IF NOT EXISTS projects (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS project_repos (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    provider        TEXT NOT NULL CHECK (provider IN ('gitlab','github','bitbucket')),
    remote_url      TEXT NOT NULL,
    default_branch  TEXT NOT NULL DEFAULT 'main',
    is_primary      BOOLEAN NOT NULL DEFAULT false,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, remote_url)
);
CREATE INDEX IF NOT EXISTS project_repos_project_id_idx ON project_repos(project_id);

CREATE TABLE IF NOT EXISTS project_dependencies (
    from_repo_id UUID NOT NULL REFERENCES project_repos(id) ON DELETE CASCADE,
    to_repo_id   UUID NOT NULL REFERENCES project_repos(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,
    PRIMARY KEY (from_repo_id, to_repo_id, kind)
);
