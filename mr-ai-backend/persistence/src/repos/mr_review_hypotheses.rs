//! Per-hypothesis review outcomes. Sprint 4b.
//!
//! Written from the worker's `per_hypothesis_review` stage. Insert-
//! only from the runtime path; the `(review_id, hypothesis_id)`
//! UNIQUE constraint protects against double-insert on worker retry.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::Result;

/// One per-hypothesis result. `status` is the outcome category the
/// worker writes:
/// - `succeeded` — LLM returned schema-valid JSON,
/// - `refused` — LLM declined ("I cannot help" etc.),
/// - `timeout` — gateway timeout, fell back to heuristic stub,
/// - `json_invalid` — response wasn't parseable, fell back to stub,
/// - `heuristic` — bypass (rerank disabled, no LLM call).
#[derive(Debug, Clone)]
pub struct HypothesisRow {
    pub review_id: Uuid,
    pub hypothesis_id: String,
    pub priority: i16,
    pub tier_used: String,
    pub status: String,
    pub llm_response: Option<Value>,
    pub latency_ms: Option<i32>,
    pub cost_usd: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// Insert one row. Returns Ok(()) on success and on duplicate key
/// (already inserted by a prior attempt of the same job).
pub async fn insert(pool: &PgPool, row: &HypothesisRow) -> Result<()> {
    sqlx::query(
        "INSERT INTO mr_review_hypotheses (\
             review_id, hypothesis_id, priority, tier_used, status, \
             llm_response, latency_ms, cost_usd, created_at\
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (review_id, hypothesis_id) DO NOTHING",
    )
    .bind(row.review_id)
    .bind(&row.hypothesis_id)
    .bind(row.priority)
    .bind(&row.tier_used)
    .bind(&row.status)
    .bind(row.llm_response.as_ref())
    .bind(row.latency_ms)
    .bind(row.cost_usd)
    .bind(row.created_at)
    .execute(pool)
    .await?;
    Ok(())
}
