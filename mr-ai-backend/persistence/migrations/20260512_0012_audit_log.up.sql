-- Audit log of admin / retrieve / trigger HTTP requests. Sprint 3 of
-- the observability layer.
--
-- One row per request that hits the admin_router (sees POST bodies +
-- token). Webhooks are deliberately excluded — they're already audited
-- via `webhook_events`. Body is never stored verbatim; the middleware
-- only persists size + sha256 so the table stays cheap and PII-free.

CREATE TABLE IF NOT EXISTS audit_log (
    id              BIGSERIAL PRIMARY KEY,
    request_id      TEXT        NOT NULL,
    route           TEXT        NOT NULL,
    method          TEXT        NOT NULL,
    status          SMALLINT    NOT NULL,
    latency_ms      INTEGER     NOT NULL,
    payload_size    INTEGER,
    payload_sha256  CHAR(64),
    -- First 16 chars of sha256(X-Admin-Token) so logs don't need the
    -- secret to correlate users. NULL when no token presented.
    token_hash      CHAR(16),
    -- Reserved for the 🅲 multi-tenant work; current admin_router does
    -- not stamp it.
    project_id      UUID,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS audit_log_created_at_idx ON audit_log(created_at);
CREATE INDEX IF NOT EXISTS audit_log_request_id_idx ON audit_log(request_id);
CREATE INDEX IF NOT EXISTS audit_log_project_id_idx
    ON audit_log(project_id) WHERE project_id IS NOT NULL;
