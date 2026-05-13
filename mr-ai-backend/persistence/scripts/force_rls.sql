-- =============================================================
-- Sprint C5 of 🅲 — FORCE row level security.
-- =============================================================
--
-- THIS IS NOT A MIGRATION. The file lives in `persistence/scripts/`
-- on purpose so `sqlx::migrate!` doesn't pick it up.
--
-- ENABLE (set in migrations 0015-0017) leaves the table owner role
-- on the bypass path. The app's database user is currently the table
-- owner, so the existing pool-based callsites keep working: queries
-- without `SET LOCAL app.current_tenant` see all rows. RLS protects
-- against bugs in code that's been migrated to `with_tenant`, plus
-- it protects against a future non-owner role used by, say, BI tools.
--
-- FORCE removes the bypass — every connection (including the owner's)
-- must `SET LOCAL app.current_tenant` before reading/writing. The app
-- isn't there yet: most callsites still use the raw pool. Applying
-- FORCE before they're migrated breaks the entire runtime — `SELECT *
-- FROM mr_reviews` from any handler returns 0 rows, INSERTs fail
-- because no row satisfies the policy check.
--
-- Prerequisites before running this script:
-- 1. Every mutating repo function in `persistence/src/repos/*` takes
--    a `&mut Transaction` rather than `&PgPool`.
-- 2. Every callsite in `api/src/routes/`, `api/src/middleware_layer/`,
--    and `worker/src/handlers/` wraps the call in
--    `persistence::with_tenant(pool, &scope, |tx| ...)`.
-- 3. CI passes the testcontainer integration tests that already
--    exercise the FORCE path on a per-test basis (see
--    `api/tests/multi_tenant_integration.rs`).
-- 4. Operators have a backup window — switching FORCE is a no-data-
--    loss change but a misbehaving query will start returning empty
--    results loudly.
--
-- How to run:
--   psql "$DATABASE_URL" -f persistence/scripts/force_rls.sql
--
-- How to revert (if a callsite was missed):
--   psql "$DATABASE_URL" -c "ALTER TABLE <table> NO FORCE ROW LEVEL SECURITY;"
-- =============================================================

ALTER TABLE projects             FORCE ROW LEVEL SECURITY;
ALTER TABLE project_repos        FORCE ROW LEVEL SECURITY;
ALTER TABLE project_dependencies FORCE ROW LEVEL SECURITY;
ALTER TABLE jobs                 FORCE ROW LEVEL SECURITY;
ALTER TABLE mr_reviews           FORCE ROW LEVEL SECURITY;
ALTER TABLE audit_log            FORCE ROW LEVEL SECURITY;
ALTER TABLE secrets_metadata     FORCE ROW LEVEL SECURITY;
ALTER TABLE index_state          FORCE ROW LEVEL SECURITY;
ALTER TABLE graph_nodes          FORCE ROW LEVEL SECURITY;
ALTER TABLE graph_edges          FORCE ROW LEVEL SECURITY;
ALTER TABLE mr_review_hypotheses FORCE ROW LEVEL SECURITY;
ALTER TABLE rerank_cache         FORCE ROW LEVEL SECURITY;
