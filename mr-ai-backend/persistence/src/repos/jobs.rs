//! Postgres-backed background job queue.
//!
//! Workers claim a row with `SELECT ... FOR UPDATE SKIP LOCKED`. Each job has
//! `attempt` / `max_attempts` and a `run_at` timestamp the worker uses to
//! enforce exponential-backoff retries. Failed jobs whose attempts hit
//! `max_attempts` transition to the `dead` status.

use chrono::{DateTime, Utc};
use domain::{JobId, ProjectId};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};

use crate::Result;

#[derive(Debug, Clone)]
pub struct ClaimedJob {
    pub id: JobId,
    pub project_id: Option<ProjectId>,
    pub kind: String,
    pub payload: Value,
    pub attempt: i32,
    pub max_attempts: i32,
}

#[derive(Debug, Clone, Default)]
pub struct EnqueueOptions {
    pub project_id: Option<ProjectId>,
    pub max_attempts: Option<i32>,
    pub run_at: Option<DateTime<Utc>>,
}

/// Insert a new job. Returns the assigned ID. Idempotency is the caller's
/// responsibility (typically achieved by checking `webhook_events.event_id`
/// upstream).
pub async fn enqueue(
    pool: &PgPool,
    kind: &str,
    payload: &Value,
    opts: EnqueueOptions,
) -> Result<JobId> {
    let mut tx = pool.begin().await?;
    let id = enqueue_in_tx(&mut tx, kind, payload, opts).await?;
    tx.commit().await?;
    Ok(id)
}

/// Transaction-aware variant — bundle inside a `webhook_events::record_in_tx`
/// + `mark_enqueued_in_tx` window so retried deliveries cannot end up with
/// a "received" row but no job to drive them.
pub async fn enqueue_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    payload: &Value,
    opts: EnqueueOptions,
) -> Result<JobId> {
    let id = JobId::new();
    let id_uuid: uuid::Uuid = id.into();
    let project_uuid: Option<uuid::Uuid> = opts.project_id.map(|p| p.into());
    let max_attempts = opts.max_attempts.unwrap_or(5);
    let run_at = opts.run_at.unwrap_or_else(Utc::now);

    sqlx::query(
        "INSERT INTO jobs (id, project_id, kind, payload, max_attempts, run_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id_uuid)
    .bind(project_uuid)
    .bind(kind)
    .bind(payload)
    .bind(max_attempts)
    .bind(run_at)
    .execute(&mut **tx)
    .await?;

    observability::counter!(
        observability::metrics::JOBS_ENQUEUED_TOTAL,
        "kind" => kind.to_owned()
    )
    .increment(1);

    Ok(id)
}

/// Claim the next due job for the supplied worker. Locks the row for the
/// duration of the surrounding transaction so concurrent workers cannot
/// double-pick. Returns `None` when nothing is due.
pub async fn claim_next(pool: &PgPool, worker_id: &str) -> Result<Option<ClaimedJob>> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;

    let row: Option<(uuid::Uuid, Option<uuid::Uuid>, String, Value, i32, i32)> = sqlx::query_as(
        "WITH next AS ( \
             SELECT id FROM jobs \
              WHERE status = 'queued' AND run_at <= now() \
              ORDER BY run_at \
              FOR UPDATE SKIP LOCKED \
              LIMIT 1 \
         ) \
         UPDATE jobs j \
            SET status = 'running', \
                locked_at = now(), \
                locked_by = $1, \
                attempt = j.attempt + 1 \
          FROM next \
          WHERE j.id = next.id \
          RETURNING j.id, j.project_id, j.kind, j.payload, j.attempt, j.max_attempts",
    )
    .bind(worker_id)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(row.map(
        |(id, project_id, kind, payload, attempt, max_attempts)| ClaimedJob {
            id: JobId::from_uuid(id),
            project_id: project_id.map(ProjectId::from_uuid),
            kind,
            payload,
            attempt,
            max_attempts,
        },
    ))
}

/// Mark a successfully processed job as `done`.
pub async fn complete(pool: &PgPool, id: JobId) -> Result<()> {
    let id_uuid: uuid::Uuid = id.into();
    sqlx::query(
        "UPDATE jobs SET status = 'done', finished_at = now(), last_error = NULL \
         WHERE id = $1",
    )
    .bind(id_uuid)
    .execute(pool)
    .await?;
    Ok(())
}

/// Schedule a retry: bumps `run_at` to `now() + delay`, resets the lock.
/// When `attempt + 1 >= max_attempts` the job is marked `dead` instead.
pub async fn fail(
    pool: &PgPool,
    job: &ClaimedJob,
    error: &str,
    retry_after: chrono::Duration,
) -> Result<()> {
    let id_uuid: uuid::Uuid = job.id.into();
    let next_attempt = job.attempt; // already incremented in claim_next
    if next_attempt >= job.max_attempts {
        sqlx::query(
            "UPDATE jobs SET status = 'dead', finished_at = now(), \
                              locked_at = NULL, locked_by = NULL, \
                              last_error = $2 \
             WHERE id = $1",
        )
        .bind(id_uuid)
        .bind(error)
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            "UPDATE jobs SET status = 'queued', \
                              locked_at = NULL, locked_by = NULL, \
                              run_at = now() + $2::interval, \
                              last_error = $3 \
             WHERE id = $1",
        )
        .bind(id_uuid)
        .bind(format!("{} milliseconds", retry_after.num_milliseconds()))
        .bind(error)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Diagnostic: count jobs by status. Cheap (uses partial index for `queued`).
pub async fn counts_by_status(pool: &PgPool) -> Result<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*)::bigint FROM jobs GROUP BY status")
            .fetch_all(pool)
            .await?;
    Ok(rows)
}
