-- Pointer table for secrets resolved through SecretProvider. Never stores
-- plaintext; only records *where* a given secret lives so operators can audit
-- and rotate.

CREATE TABLE IF NOT EXISTS secrets_metadata (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID REFERENCES projects(id) ON DELETE CASCADE,
    key         TEXT NOT NULL,
    backend     TEXT NOT NULL CHECK (backend IN ('env','file','vault')),
    location    TEXT NOT NULL,
    rotated_at  TIMESTAMPTZ,
    UNIQUE (project_id, key)
);
