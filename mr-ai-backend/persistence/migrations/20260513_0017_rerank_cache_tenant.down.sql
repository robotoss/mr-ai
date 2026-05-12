DROP POLICY IF EXISTS tenant_iso_rerank_cache ON rerank_cache;
ALTER TABLE rerank_cache DISABLE ROW LEVEL SECURITY;
DROP INDEX IF EXISTS rerank_cache_project_id_idx;
ALTER TABLE rerank_cache DROP COLUMN IF EXISTS project_id;
