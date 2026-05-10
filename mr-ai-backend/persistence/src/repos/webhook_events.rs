//! Idempotency-aware persistence for inbound webhook events.
//!
//! Every verified webhook lands here before it reaches the job queue. The
//! unique `(provider, event_id)` constraint provides retry deduplication —
//! callers should treat `RecordOutcome::Duplicate` as a no-op success.

use domain::{ProviderKind, WebhookEventId};
use serde_json::Value;
use sqlx::PgPool;

use crate::Result;

#[derive(Debug, Clone)]
pub struct WebhookRecord {
    pub provider: ProviderKind,
    /// Provider-supplied event identifier. Use `X-Gitlab-Event-UUID`,
    /// `X-GitHub-Delivery`, or the first 32 hex chars of the body hash
    /// when the provider does not supply one.
    pub event_id: String,
    pub event_kind: String,
    pub payload_hash: Vec<u8>,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Brand-new event. Caller should enqueue a job.
    Inserted,
    /// Event already seen — caller should not enqueue again.
    Duplicate,
}

#[derive(Debug, Clone)]
pub struct RecordedEvent {
    pub id: WebhookEventId,
    pub outcome: RecordOutcome,
}

/// Insert a webhook record. When `(provider, event_id)` already exists the
/// existing row's id is returned and outcome is `Duplicate`.
pub async fn record(pool: &PgPool, rec: &WebhookRecord) -> Result<RecordedEvent> {
    let new_id = WebhookEventId::new();
    let new_uuid: uuid::Uuid = new_id.into();

    let row: (uuid::Uuid, bool) = sqlx::query_as(
        "WITH ins AS ( \
             INSERT INTO webhook_events (id, provider, event_id, event_kind, payload_hash, payload) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (provider, event_id) DO NOTHING \
             RETURNING id, true AS inserted \
         ) \
         SELECT id, inserted FROM ins \
         UNION ALL \
         SELECT id, false AS inserted FROM webhook_events \
          WHERE provider = $2 AND event_id = $3 \
          AND NOT EXISTS (SELECT 1 FROM ins) \
         LIMIT 1",
    )
    .bind(new_uuid)
    .bind(rec.provider.as_str())
    .bind(&rec.event_id)
    .bind(&rec.event_kind)
    .bind(&rec.payload_hash)
    .bind(&rec.payload)
    .fetch_one(pool)
    .await?;

    let id = WebhookEventId::from_uuid(row.0);
    let outcome = if row.1 {
        RecordOutcome::Inserted
    } else {
        RecordOutcome::Duplicate
    };
    Ok(RecordedEvent { id, outcome })
}

/// Mark a previously-recorded event as enqueued (job created downstream).
pub async fn mark_enqueued(pool: &PgPool, id: WebhookEventId) -> Result<()> {
    let id_uuid: uuid::Uuid = id.into();
    sqlx::query("UPDATE webhook_events SET status = 'enqueued' WHERE id = $1")
        .bind(id_uuid)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark an event as rejected (HMAC failure, malformed payload, ...).
pub async fn mark_rejected(pool: &PgPool, id: WebhookEventId) -> Result<()> {
    let id_uuid: uuid::Uuid = id.into();
    sqlx::query("UPDATE webhook_events SET status = 'rejected' WHERE id = $1")
        .bind(id_uuid)
        .execute(pool)
        .await?;
    Ok(())
}
