//! Per-repo indexing watermark.
//!
//! Single row per `RepoId` in the `index_state` table. The S4 incremental
//! delta updater reads `last_indexed_sha`, computes the diff against the
//! latest master HEAD, and re-indexes only what changed. Failures are
//! recorded in `last_error` for ops visibility.

use chrono::{DateTime, Utc};
use domain::RepoId;
use sqlx::PgPool;

use crate::Result;

#[derive(Debug, Clone)]
pub struct IndexWatermark {
    pub repo_id: RepoId,
    pub last_indexed_sha: Option<String>,
    pub last_indexed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

pub async fn get(pool: &PgPool, repo_id: RepoId) -> Result<Option<IndexWatermark>> {
    let id_uuid: uuid::Uuid = repo_id.into();
    let row: Option<(uuid::Uuid, Option<String>, Option<DateTime<Utc>>, Option<String>)> =
        sqlx::query_as(
            "SELECT repo_id, last_indexed_sha, last_indexed_at, last_error \
               FROM index_state WHERE repo_id = $1",
        )
        .bind(id_uuid)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(
        |(repo_id, last_indexed_sha, last_indexed_at, last_error)| IndexWatermark {
            repo_id: RepoId::from_uuid(repo_id),
            last_indexed_sha,
            last_indexed_at,
            last_error,
        },
    ))
}

/// Mark a repo as indexed at `sha`. Clears any previous error.
pub async fn mark_indexed(pool: &PgPool, repo_id: RepoId, sha: &str) -> Result<()> {
    let id_uuid: uuid::Uuid = repo_id.into();
    sqlx::query(
        "INSERT INTO index_state (repo_id, last_indexed_sha, last_indexed_at, last_error) \
         VALUES ($1, $2, now(), NULL) \
         ON CONFLICT (repo_id) DO UPDATE SET \
             last_indexed_sha = EXCLUDED.last_indexed_sha, \
             last_indexed_at  = now(), \
             last_error       = NULL",
    )
    .bind(id_uuid)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record an indexing failure without advancing `last_indexed_sha`.
pub async fn record_error(pool: &PgPool, repo_id: RepoId, err: &str) -> Result<()> {
    let id_uuid: uuid::Uuid = repo_id.into();
    sqlx::query(
        "INSERT INTO index_state (repo_id, last_error) VALUES ($1, $2) \
         ON CONFLICT (repo_id) DO UPDATE SET last_error = EXCLUDED.last_error",
    )
    .bind(id_uuid)
    .bind(err)
    .execute(pool)
    .await?;
    Ok(())
}
