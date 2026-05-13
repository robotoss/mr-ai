//! `mr_reviews` table: per-MR review state and the bundle that fed it.
//!
//! Lifecycle: `pending` (just enqueued) → `running` (worker picked it up)
//! → `published` / `failed`. Re-runs of the same MR upsert the same row
//! keyed by `(primary_repo_id, mr_iid)`.

use chrono::{DateTime, Utc};
use domain::{MrId, ProjectId, RepoId};
use serde_json::Value;
use sqlx::PgPool;

use crate::Result;

#[derive(Debug, Clone)]
pub struct MrReviewSummary {
    pub id: uuid::Uuid,
    pub project_id: ProjectId,
    pub primary_repo_id: RepoId,
    pub mr_iid: MrId,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// Upsert a row in `pending` status with the latest bundle. Idempotent —
/// re-runs reset `started_at` / `finished_at` so observers can tell a new
/// attempt has begun.
pub async fn upsert_pending(
    pool: &PgPool,
    project_id: ProjectId,
    primary_repo_id: RepoId,
    mr_iid: &MrId,
    bundle: &Value,
) -> Result<uuid::Uuid> {
    let project_uuid: uuid::Uuid = project_id.into();
    let repo_uuid: uuid::Uuid = primary_repo_id.into();

    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO mr_reviews (project_id, primary_repo_id, mr_iid, status, bundle) \
         VALUES ($1, $2, $3, 'pending', $4) \
         ON CONFLICT (primary_repo_id, mr_iid) DO UPDATE SET \
             status = 'pending', \
             bundle = EXCLUDED.bundle, \
             started_at = NULL, \
             finished_at = NULL \
         RETURNING id",
    )
    .bind(project_uuid)
    .bind(repo_uuid)
    .bind(mr_iid.as_ref())
    .bind(bundle)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

/// Mark a review as in-flight.
pub async fn mark_running(pool: &PgPool, id: uuid::Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE mr_reviews SET status = 'running', started_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace the bundle and mark the review as finished. Use `published`
/// when the LLM review actually shipped, `failed` for hard errors.
pub async fn finish(
    pool: &PgPool,
    id: uuid::Uuid,
    status: &str,
    bundle: &Value,
) -> Result<()> {
    sqlx::query(
        "UPDATE mr_reviews \
            SET status = $2, finished_at = now(), bundle = $3 \
          WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(bundle)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record a hard failure without updating the bundle.
pub async fn mark_failed(pool: &PgPool, id: uuid::Uuid, error: &str) -> Result<()> {
    sqlx::query(
        "UPDATE mr_reviews \
            SET status = 'failed', finished_at = now(), \
                bundle = jsonb_set(coalesce(bundle, '{}'::jsonb), '{error}', to_jsonb($2::text)) \
          WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(())
}
