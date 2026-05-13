//! Audit log writes + retention helper. The middleware on the admin
//! router calls [`insert`] per request; the scheduled cleanup task in
//! `api::start` calls [`delete_expired`] once per day.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::Result;

/// One audit row. Mirrors the `audit_log` table 1:1; field naming
/// stays in snake_case so the row can be projected back via the
/// `FromRow` macro if we ever need to read it (currently insert-only).
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub request_id: String,
    pub route: String,
    pub method: String,
    pub status: i16,
    pub latency_ms: i32,
    pub payload_size: Option<i32>,
    pub payload_sha256: Option<String>,
    pub token_hash: Option<String>,
    pub project_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Insert one row. Caller spawns this on a separate task so the HTTP
/// response is not blocked by audit-write latency.
pub async fn insert(pool: &PgPool, entry: &AuditEntry) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit_log (\
             request_id, route, method, status, latency_ms, \
             payload_size, payload_sha256, token_hash, project_id, \
             created_at\
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(&entry.request_id)
    .bind(&entry.route)
    .bind(&entry.method)
    .bind(entry.status)
    .bind(entry.latency_ms)
    .bind(entry.payload_size)
    .bind(entry.payload_sha256.as_deref())
    .bind(entry.token_hash.as_deref())
    .bind(entry.project_id)
    .bind(entry.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete rows older than `cutoff`. Returns the number of deleted
/// rows (useful for ops dashboards). Run from the scheduled task in
/// `api::start`.
pub async fn delete_expired(pool: &PgPool, cutoff: DateTime<Utc>) -> Result<u64> {
    let res = sqlx::query("DELETE FROM audit_log WHERE created_at < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}
