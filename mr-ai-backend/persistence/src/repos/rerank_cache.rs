//! Postgres-backed cache for LLM rerank results. Sprint 4a-1.
//!
//! `/retrieve?rerank=true` consults the cache before invoking the
//! Smart-tier rerank LLM call; a hit returns the stored hits, a miss
//! runs the rerank and writes the row before responding.
//!
//! Hits are stored as JSONB so `ScoredHit` can grow new fields
//! without a migration.

use chrono::{DateTime, Utc};
use domain::ProjectId;
use serde_json::Value;
use sqlx::PgPool;

use crate::Result;

/// One cached rerank result. `cache_key` is sha256 hex of the
/// canonicalised input set (see `compute_cache_key` in the api crate).
#[derive(Debug, Clone)]
pub struct CachedEntry {
    pub cache_key: String,
    pub hits_json: Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Look up a fresh cache row by key. Returns `Ok(None)` when the row
/// doesn't exist OR has expired — both cases trigger a recompute on
/// the caller side.
pub async fn lookup(pool: &PgPool, cache_key: &str) -> Result<Option<CachedEntry>> {
    let row: Option<(String, Value, DateTime<Utc>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT cache_key, hits_json, created_at, expires_at \
         FROM rerank_cache \
         WHERE cache_key = $1 AND expires_at > now()",
    )
    .bind(cache_key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(cache_key, hits_json, created_at, expires_at)| CachedEntry {
        cache_key,
        hits_json,
        created_at,
        expires_at,
    }))
}

/// Insert or refresh a cache row. ON CONFLICT updates `hits_json` and
/// pushes `expires_at` forward — a second writer for the same key
/// "wins" and keeps the cache warm.
///
/// `project_id` is stored explicitly (sprint C2) so the row's tenant
/// scope is queryable for ops dashboards and visible to the RLS
/// policy. The `cache_key` already embeds `project_id` in its hash,
/// so the column is denormalised but stable.
pub async fn upsert(
    pool: &PgPool,
    cache_key: &str,
    project_id: ProjectId,
    hits_json: &Value,
    ttl_hours: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO rerank_cache (cache_key, project_id, hits_json, created_at, expires_at) \
         VALUES ($1, $2, $3, now(), now() + make_interval(hours => $4)) \
         ON CONFLICT (cache_key) DO UPDATE SET \
             hits_json = EXCLUDED.hits_json, \
             created_at = EXCLUDED.created_at, \
             expires_at = EXCLUDED.expires_at",
    )
    .bind(cache_key)
    .bind(project_id.as_uuid())
    .bind(hits_json)
    .bind(ttl_hours)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete rows whose `expires_at < cutoff`. Returns the number of
/// removed rows for ops dashboards. Called by the background cleanup
/// task spawned in `api::start`.
pub async fn delete_expired(pool: &PgPool, cutoff: DateTime<Utc>) -> Result<u64> {
    let res = sqlx::query("DELETE FROM rerank_cache WHERE expires_at < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
