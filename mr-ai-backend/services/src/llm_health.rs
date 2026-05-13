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
use tokio::sync::{Notify, RwLock};
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

    /// Spawn a background refresher and return both the monitor (clone into
    /// `AppState`) and a `LlmHealthSupervisor` that must be drained on
    /// graceful shutdown to avoid leaking the background task. The first
    /// refresh is awaited synchronously so subsequent probes never see the
    /// empty default.
    pub async fn start(
        gateway: Arc<LlmGateway>,
        interval: Duration,
    ) -> (Self, LlmHealthSupervisor) {
        let monitor = Self::empty();
        monitor.refresh_now(&gateway).await;

        let cancel = Arc::new(Notify::new());
        let cancel_for_task = cancel.clone();
        let handle_monitor = monitor.clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // First tick fires immediately; we already refreshed, so skip.
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = cancel_for_task.notified() => {
                        debug!(target = "llm_health", "supervisor received shutdown");
                        break;
                    }
                    _ = ticker.tick() => {
                        handle_monitor.refresh_now(&gateway).await;
                    }
                }
            }
        });
        (
            monitor,
            LlmHealthSupervisor {
                handle: Some(handle),
                cancel,
            },
        )
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
                    let failing: Vec<String> = state
                        .items
                        .iter()
                        .filter(|s| !s.ok)
                        .map(|s| format!("{:?}/{}: {}", s.role, s.model, s.message))
                        .collect();
                    warn!(
                        target = "llm_health",
                        failing = ?failing,
                        "at least one provider unhealthy"
                    );
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

/// Owns the background refresh task. Drop or `shutdown()` to drain it —
/// dropping without shutdown still aborts the task (`JoinHandle::abort`)
/// so the monitor never outlives the host process, but explicit
/// `shutdown().await` is the clean path that lets the in-flight refresh
/// finish first.
pub struct LlmHealthSupervisor {
    handle: Option<JoinHandle<()>>,
    cancel: Arc<Notify>,
}

impl LlmHealthSupervisor {
    pub async fn shutdown(mut self) {
        self.cancel.notify_waiters();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for LlmHealthSupervisor {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
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
