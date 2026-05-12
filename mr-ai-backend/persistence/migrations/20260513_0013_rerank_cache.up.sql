-- LLM rerank cache. `/retrieve?rerank=true` deduplicates expensive
-- Smart-tier rerank calls within a TTL window by hashing the query
-- + project_id + repo_id + top_k + sorted chunk_id set into a stable
-- cache_key. Cache hit short-circuits the LLM call; miss runs the
-- rerank and writes the row before responding.
--
-- Hits are stored as JSONB so the schema can evolve without a
-- migration when ScoredHit grows new fields (e.g. graph hops, overlay
-- weight).

CREATE TABLE IF NOT EXISTS rerank_cache (
    cache_key   CHAR(64) PRIMARY KEY,
    hits_json   JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS rerank_cache_expires_at_idx
    ON rerank_cache(expires_at);
