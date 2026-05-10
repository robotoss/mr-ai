//! Background-refreshed cache of `LlmGateway::health_all` snapshots.
//!
//! Probing the LLM gateway on every `/health/detailed` request would burn
//! credits on every k8s probe tick. Instead we run a single background
//! task that refreshes the snapshot on a configurable interval and serve
//! the cached value to every probe.
//!
//! - First refresh runs synchronously on `start()` so callers can await
//!   until the cache is warm.
//! - Subsequent refreshes are best-effort: a hung provider stalls the
//!   background task, but the cache keeps returning the previous value
//!   tagged with `last_refreshed_at` so the UI can flag staleness.

use std::sync::Arc;
use std::time::Duration;

use ai_llm_service::{HealthSnapshot, LlmGateway};
use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

/// Cached state plus refresh metadata.
#[derive(Debug, Clone, Serialize)]
pub struct LlmHealthSnapshot {
    pub healthy: bool,
    pub last_refreshed_at: Option<DateTime<Utc>>,
    pub refresh_failures: u32,
    pub items: Vec<HealthSnapshot>,
}

impl Default for LlmHealthSnapshot {
    fn default() -> Self {
        Self {
            healthy: false,
            last_refreshed_at: None,
            refresh_failures: 0,
            items: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmHealthMonitor {
    inner: Arc<RwLock<LlmHealthSnapshot>>,
}

impl LlmHealthMonitor {
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(LlmHealthSnapshot::default())),
        }
    }

    pub async fn current(&self) -> LlmHealthSnapshot {
        self.inner.read().await.clone()
    }

    /// Spawn a background refresher. Returns the join handle plus the
    /// monitor that should be cloned into AppState. The first refresh is
    /// awaited synchronously so subsequent probes never see the empty
    /// default.
    pub async fn start(
        gateway: Arc<LlmGateway>,
        interval: Duration,
    ) -> (Self, JoinHandle<()>) {
        let monitor = Self::empty();
        // Warm-up refresh before the loop spins up.
        monitor.refresh_now(&gateway).await;

        let handle_monitor = monitor.clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // First tick fires immediately; we already refreshed, so skip.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                handle_monitor.refresh_now(&gateway).await;
            }
        });
        (monitor, handle)
    }

    async fn refresh_now(&self, gateway: &Arc<LlmGateway>) {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(15), gateway.health_all()).await;
        match result {
            Ok(items) => {
                let healthy = !items.is_empty() && items.iter().all(|s| s.ok);
                let mut state = self.inner.write().await;
                state.healthy = healthy;
                state.last_refreshed_at = Some(Utc::now());
                state.items = items;
                state.refresh_failures = 0;
                debug!(
                    target = "llm_health",
                    healthy,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "snapshot refreshed"
                );
                if healthy {
                    info!(target = "llm_health", "all providers healthy");
                } else {
                    warn!(target = "llm_health", "at least one provider unhealthy");
                }
            }
            Err(_) => {
                let mut state = self.inner.write().await;
                state.refresh_failures = state.refresh_failures.saturating_add(1);
                error!(
                    target = "llm_health",
                    failures = state.refresh_failures,
                    "health probe timed out"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_monitor_reports_default_state() {
        let monitor = LlmHealthMonitor::empty();
        let snap = monitor.current().await;
        assert!(!snap.healthy);
        assert!(snap.last_refreshed_at.is_none());
        assert_eq!(snap.refresh_failures, 0);
        assert!(snap.items.is_empty());
    }

    #[tokio::test]
    async fn current_returns_clones() {
        // Two reads should not block each other or each other's data.
        let monitor = LlmHealthMonitor::empty();
        let (a, b) = tokio::join!(monitor.current(), monitor.current());
        assert_eq!(a.refresh_failures, b.refresh_failures);
    }
}
