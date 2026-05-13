//! Postgres-backed [`observability::AuditPort`]: bridges the
//! observability audit middleware to `persistence::repos::audit`.
//! Write failures are logged via `tracing::warn` but never bubbled —
//! audit is best-effort and must never break the response path.

use async_trait::async_trait;
use observability::{AuditEntry, AuditPort};
use persistence::repos::audit as audit_repo;
use sqlx::PgPool;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct PgAuditPort {
    pool: PgPool,
}

impl PgAuditPort {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuditPort for PgAuditPort {
    async fn record(&self, entry: AuditEntry) {
        let row = audit_repo::AuditEntry {
            request_id: entry.request_id,
            route: entry.route,
            method: entry.method,
            status: entry.status as i16,
            latency_ms: entry.latency_ms as i32,
            payload_size: entry.payload_size.map(|n| n as i32),
            payload_sha256: entry.payload_sha256,
            token_hash: entry.token_hash,
            project_id: entry.project_id,
            created_at: entry.created_at,
        };
        if let Err(err) = audit_repo::insert(&self.pool, &row).await {
            warn!(
                target = "audit",
                error = %err,
                "audit_log insert failed; row dropped"
            );
        }
    }
}
