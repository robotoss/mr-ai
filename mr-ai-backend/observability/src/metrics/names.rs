//! Canonical metric names. Keeping them as `const` strings avoids
//! typo-driven cardinality bugs (a typo in a counter name silently
//! splits a series into two).
//!
//! Cardinality budget (sprint 1):
//! - `kind` ∈ {IngestPush, IngestMr, Reindex} — 3 values
//! - `outcome` ∈ {ok, fail, dead} — 3 values
//! - `tier` ∈ {default, smart, fast} — 3 values
//! - `kind` (LLM) ∈ {embedding, completion} — 2 values
//! - `provider`, `model` — bounded by configured gateway providers
//! - `status` ∈ {published, failed, skipped} — 3 values
//! - `op` (Qdrant) ∈ {search, scroll, upsert, delete} — 4 values
//! - `provider` (webhook) ∈ {gitlab, github, bitbucket} — 3 values
//!
//! `project_id`, `route`, `repo_id` labels are intentionally absent —
//! adding them is part of 🅲 multi-tenant work.

// === Job queue ============================================================
pub const JOBS_ENQUEUED_TOTAL: &str = "jobs_enqueued_total";
pub const JOBS_DONE_TOTAL: &str = "jobs_done_total";
pub const JOB_DURATION_SECONDS: &str = "job_duration_seconds";
pub const JOBS_RUNNING: &str = "jobs_running";

// === LLM gateway ==========================================================
pub const LLM_CALLS_TOTAL: &str = "llm_calls_total";
/// Cost tracked in micro-USD because `Counter::increment` is `u64`.
/// Prometheus query: `rate(llm_cost_micro_usd_total[5m]) / 1_000_000`.
pub const LLM_COST_MICRO_USD_TOTAL: &str = "llm_cost_micro_usd_total";
pub const LLM_LATENCY_SECONDS: &str = "llm_latency_seconds";

// === Retrieval ============================================================
pub const RETRIEVE_LATENCY_SECONDS: &str = "retrieve_latency_seconds";
pub const RETRIEVE_HITS: &str = "retrieve_hits";

// === Qdrant ===============================================================
pub const QDRANT_SEARCH_LATENCY_SECONDS: &str = "qdrant_search_latency_seconds";

// === Postgres pool ========================================================
pub const PG_POOL_CONNECTIONS_ACTIVE: &str = "pg_pool_connections_active";

// === Webhooks =============================================================
pub const WEBHOOK_RECEIVED_TOTAL: &str = "webhook_received_total";

// === MR review pipeline ===================================================
pub const MR_REVIEWS_TOTAL: &str = "mr_reviews_total";
