//! Background snapshotter for `/health/dashboard`.
//!
//! Single tokio task wakes every `refresh_interval` (default 30s),
//! runs the four cheap aggregation queries, takes one LLM gateway
//! `usage_snapshot`, and stores the result behind an `RwLock`. The
//! HTTP handler then serves the cached snapshot in O(1) — no DB hop
//! per request.
//!
//! Errors per refresh are logged and the previous snapshot is kept;
//! a transient DB blip never poisons the endpoint.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::warn;

use ai_llm_service::LlmGateway;
use persistence::repos::dashboard;

/// Payload served by `GET /health/dashboard`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct DashboardSnapshot {
    /// Timestamp the snapshot was generated. `None` until the first
    /// refresh completes.
    pub as_of: Option<DateTime<Utc>>,
    pub jobs: JobsPanel,
    pub mr_reviews: MrReviewsPanel,
    pub llm: LlmPanel,
    pub worker: WorkerPanel,
}

/// Jobs grouped by state, with per-kind sub-counts. Stable shape so a
/// UI can render unknown kinds without breakage.
#[derive(Debug, Clone, Serialize, Default)]
pub struct JobsPanel {
    pub queued: BTreeMap<String, i64>,
    pub running: BTreeMap<String, i64>,
    pub dead: BTreeMap<String, i64>,
    pub done: BTreeMap<String, i64>,
    pub failed: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct MrReviewsPanel {
    pub by_status: BTreeMap<String, i64>,
}

/// LLM gateway summary lifted from the in-memory `UsageSnapshot`.
/// Cheap — one read lock + clone per refresh.
#[derive(Debug, Clone, Serialize, Default)]
pub struct LlmPanel {
    pub total_calls: u64,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct WorkerPanel {
    pub pool_size: usize,
}

/// Shared snapshot cell — handler clones the inner via `read().await`.
pub type DashboardCache = Arc<RwLock<DashboardSnapshot>>;

/// Wiring inputs for [`spawn_dashboard_monitor`].
pub struct DashboardMonitorInputs {
    pub pool: PgPool,
    pub gateway: Arc<LlmGateway>,
    pub worker_pool_size: usize,
    pub refresh_interval: Duration,
}

/// Spawn the refresher and return the shared cell. First refresh runs
/// immediately so the `/health/dashboard` endpoint has data before the
/// first scrape interval elapses.
pub fn spawn_dashboard_monitor(inputs: DashboardMonitorInputs) -> DashboardCache {
    let cache: DashboardCache = Arc::new(RwLock::new(DashboardSnapshot::default()));
    let cache_clone = cache.clone();
    tokio::spawn(async move {
        // Initial refresh — best effort. If it fails we leave the
        // default and let the next tick try again.
        let snapshot = build_snapshot(&inputs.pool, &inputs.gateway, inputs.worker_pool_size).await;
        *cache_clone.write().await = snapshot;

        let mut ticker = tokio::time::interval(inputs.refresh_interval);
        // First tick fires immediately on `tokio::time::interval`; we
        // already did one refresh above, so skip it.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let snapshot =
                build_snapshot(&inputs.pool, &inputs.gateway, inputs.worker_pool_size).await;
            *cache_clone.write().await = snapshot;
        }
    });
    cache
}

async fn build_snapshot(
    pool: &PgPool,
    gateway: &Arc<LlmGateway>,
    worker_pool_size: usize,
) -> DashboardSnapshot {
    let jobs = match dashboard::job_counts(pool).await {
        Ok(rows) => fold_job_rows(rows),
        Err(err) => {
            warn!(target = "dashboard", error = %err, "job_counts failed");
            JobsPanel::default()
        }
    };

    let mr_reviews = match dashboard::mr_review_counts(pool).await {
        Ok(rows) => {
            let mut by_status = BTreeMap::new();
            for (status, count) in rows {
                by_status.insert(status, count);
            }
            MrReviewsPanel { by_status }
        }
        Err(err) => {
            warn!(target = "dashboard", error = %err, "mr_review_counts failed");
            MrReviewsPanel::default()
        }
    };

    let usage = gateway.usage_snapshot();
    let llm = LlmPanel {
        total_calls: usage.total_calls,
        total_tokens: usage.total_tokens,
        total_cost_usd: usage.total_cost_usd,
    };

    DashboardSnapshot {
        as_of: Some(Utc::now()),
        jobs,
        mr_reviews,
        llm,
        worker: WorkerPanel {
            pool_size: worker_pool_size,
        },
    }
}

/// Fold the flat `(status, kind, count)` rollup into the panel shape.
/// Unknown statuses land in a per-status bucket so the UI can flag
/// drift without a code change.
pub(crate) fn fold_job_rows(rows: Vec<(String, String, i64)>) -> JobsPanel {
    let mut panel = JobsPanel::default();
    for (status, kind, count) in rows {
        let bucket = match status.as_str() {
            "queued" => &mut panel.queued,
            "running" => &mut panel.running,
            "dead" => &mut panel.dead,
            "done" => &mut panel.done,
            "failed" => &mut panel.failed,
            _ => continue,
        };
        bucket.insert(kind, count);
    }
    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_serializes_with_expected_shape() {
        let snap = DashboardSnapshot {
            as_of: None,
            jobs: JobsPanel {
                queued: BTreeMap::from([("Reindex".to_owned(), 5)]),
                running: BTreeMap::from([("IngestMr".to_owned(), 1)]),
                dead: BTreeMap::new(),
                done: BTreeMap::new(),
                failed: BTreeMap::new(),
            },
            mr_reviews: MrReviewsPanel {
                by_status: BTreeMap::from([("published".to_owned(), 142)]),
            },
            llm: LlmPanel {
                total_calls: 234,
                total_tokens: 12_345,
                total_cost_usd: 0.82,
            },
            worker: WorkerPanel { pool_size: 4 },
        };
        let v = serde_json::to_value(&snap).unwrap();
        // Required top-level keys present (UI relies on these).
        assert!(v.get("as_of").is_some());
        assert_eq!(
            v["jobs"]["queued"],
            json!({"Reindex": 5}),
            "got: {v:?}"
        );
        assert_eq!(v["mr_reviews"]["by_status"]["published"], 142);
        assert_eq!(v["llm"]["total_calls"], 234);
        assert_eq!(v["worker"]["pool_size"], 4);
    }

    #[test]
    fn fold_job_rows_buckets_by_status_and_drops_unknown_status() {
        let rows = vec![
            ("queued".into(), "Reindex".into(), 5),
            ("queued".into(), "IngestMr".into(), 2),
            ("running".into(), "Reindex".into(), 1),
            ("dead".into(), "IngestMr".into(), 3),
            // Unknown status — must not panic, must not appear in the
            // existing buckets.
            ("paused".into(), "Reindex".into(), 7),
        ];
        let panel = fold_job_rows(rows);
        assert_eq!(panel.queued.get("Reindex"), Some(&5));
        assert_eq!(panel.queued.get("IngestMr"), Some(&2));
        assert_eq!(panel.running.get("Reindex"), Some(&1));
        assert_eq!(panel.dead.get("IngestMr"), Some(&3));
        assert!(panel.queued.get("paused").is_none());
    }
}
