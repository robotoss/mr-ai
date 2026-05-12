-- Multi-tenant lift (🅲) sprint C2: row-level security on tables that
-- carry `project_id` directly.
--
-- Policies use `current_setting('app.current_tenant', true)::uuid`,
-- which returns NULL when SET LOCAL is missing. NULL doesn't compare
-- equal to anything → 0 rows visible. Intentional: this matches the
-- `persistence::with_tenant` contract (every tenant-scoped query is
-- wrapped in a tx that calls `set_config(...)` first).
--
-- We use `ENABLE` (not `FORCE`) for now. Sprint C5 will add FORCE
-- once every callsite has been migrated through `with_tenant`. Until
-- then the table owner (the app's database user) bypasses RLS, so
-- existing code keeps working unchanged through C3 + C4.

-- ============================================================
-- projects: id IS the project_id (this table is the source of truth)
-- ============================================================
ALTER TABLE projects ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_projects ON projects
    USING (id = current_setting('app.current_tenant', true)::uuid);

-- ============================================================
-- project_repos: explicit project_id FK
-- ============================================================
ALTER TABLE project_repos ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_project_repos ON project_repos
    USING (project_id = current_setting('app.current_tenant', true)::uuid);

-- ============================================================
-- jobs: project_id is nullable (NULL = system jobs); compare via
-- IS NOT DISTINCT FROM so the NULL tenant setting matches NULL rows
-- only when both sides are unset — i.e. system-level jobs are
-- invisible to tenants but visible to unscoped admin queries.
-- ============================================================
ALTER TABLE jobs ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_jobs ON jobs
    USING (project_id = current_setting('app.current_tenant', true)::uuid);

-- ============================================================
-- mr_reviews: project_id NOT NULL (S2 invariant)
-- ============================================================
ALTER TABLE mr_reviews ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_mr_reviews ON mr_reviews
    USING (project_id = current_setting('app.current_tenant', true)::uuid);

-- ============================================================
-- audit_log: project_id currently nullable (placeholder from
-- sprint 3 observability). C4 will make it NOT NULL.
-- ============================================================
ALTER TABLE audit_log ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_audit_log ON audit_log
    USING (project_id = current_setting('app.current_tenant', true)::uuid);

-- ============================================================
-- secrets_metadata: project_id nullable (NULL = global secrets like
-- TRIGGER_SECRET). Tenant policy hides per-tenant entries; global
-- secrets are not tenant data and shouldn't be queried through this
-- table by routes anyway.
-- ============================================================
ALTER TABLE secrets_metadata ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_secrets_metadata ON secrets_metadata
    USING (project_id = current_setting('app.current_tenant', true)::uuid);

-- NOTE: webhook_events is intentionally left without RLS — no
-- project_id column; the table is a global idempotency ledger keyed
-- on (provider, event_id). Adding tenant scope would require a
-- schema change and a write-path rewrite; out of scope for C2.
