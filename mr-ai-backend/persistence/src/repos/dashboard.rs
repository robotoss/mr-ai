//! Aggregation queries powering `/health/dashboard`. The aggregations
//! are read once per refresh window (default 30s) by the background
//! monitor task; the HTTP handler reads the cached snapshot in O(1).
//!
//! All queries are cheap — a single `GROUP BY` per metric, served from
//! the existing indexes.

use sqlx::PgPool;

use crate::Result;

/// One `(kind, count)` pair per job state. Caller buckets by status.
#[derive(Debug, Clone, Default)]
pub struct JobBucket {
    pub kind: String,
    pub count: i64,
}

/// Job rollup keyed by `(status, kind)`. Returns a flat vec for the
/// caller to fold into the dashboard shape — keeps SQL minimal and
/// lets the caller decide how to group.
pub async fn job_counts(pool: &PgPool) -> Result<Vec<(String, String, i64)>> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT status, kind, count(*)::bigint \
         FROM jobs \
         GROUP BY status, kind \
         ORDER BY status, kind",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// `(status, count)` aggregate for MR reviews. Used to drive the
/// dashboard's `mr_reviews` panel.
pub async fn mr_review_counts(pool: &PgPool) -> Result<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, count(*)::bigint \
         FROM mr_reviews \
         GROUP BY status",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
