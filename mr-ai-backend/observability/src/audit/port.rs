//! `AuditPort` — the seam between the HTTP-side middleware (this
//! crate) and the persistence-side writer (whatever the host crate
//! provides). Keeps `observability` independent of `persistence`.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// One row of audit data. Mirrors the columns of `audit_log` 1:1.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub request_id: String,
    pub route: String,
    pub method: String,
    pub status: u16,
    pub latency_ms: u64,
    pub payload_size: Option<u32>,
    pub payload_sha256: Option<String>,
    pub token_hash: Option<String>,
    pub project_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Persistence-side writer. The api crate provides an `Arc<dyn AuditPort>`
/// wrapping a real Postgres pool; tests provide an in-memory recorder.
#[async_trait]
pub trait AuditPort: Send + Sync + std::fmt::Debug {
    /// Insert one row. Errors should be logged but never bubbled up
    /// because the middleware spawns this on a detached task.
    async fn record(&self, entry: AuditEntry);
}

/// Shared `Arc` alias for ergonomics at the middleware boundary.
pub type SharedAuditPort = Arc<dyn AuditPort>;
