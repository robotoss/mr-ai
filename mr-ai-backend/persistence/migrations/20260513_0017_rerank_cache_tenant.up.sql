-- Multi-tenant lift (🅲) sprint C2: tenant-scope the rerank cache.
--
-- The C1-era `rerank_cache` table keyed only on a sha256 cache_key
-- that already embedded `project_id` via the input set, but the
-- table itself had no `project_id` column → no way to attach an RLS
-- policy. Add the column, RLS-policy it, and clear the cache (rows
-- can't be retroactively assigned a tenant).
--
-- Truncation is safe: rerank_cache is a cache. A miss after deploy
-- re-runs the Smart-tier LLM call and re-populates. No correctness
-- loss, just temporary cache cold-start.

TRUNCATE rerank_cache;

ALTER TABLE rerank_cache
    ADD COLUMN project_id UUID NOT NULL;

CREATE INDEX IF NOT EXISTS rerank_cache_project_id_idx
    ON rerank_cache(project_id);

ALTER TABLE rerank_cache ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_rerank_cache ON rerank_cache
    USING (project_id = current_setting('app.current_tenant', true)::uuid);
