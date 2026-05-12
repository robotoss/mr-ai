//! Background job worker pool.
//!
//! Workers poll the Postgres `jobs` table via `claim_next` (SKIP LOCKED),
//! dispatch each row to a `JobHandler` registered for its `kind`, and route
//! the outcome back into the queue (`complete`, `fail` with backoff, or
//! `dead` after exhausting attempts).
//!
//! Pool topology:
//! - One async task per worker slot. Each owns a `worker_id` (`worker-NN`).
//! - Slots share a single `Registry` (`Arc`'d HashMap of handlers).
//! - Failed handlers reschedule with exponential backoff capped at 5 minutes.
//! - A boot-time SIGTERM-style cancellation token cleanly drains slots.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Duration as ChronoDuration;
use persistence::repos::jobs::{self, ClaimedJob};
use serde_json::Value;
use sqlx::PgPool;
use thiserror::Error;
use tokio::sync::Notify;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

pub mod handlers;
pub mod ports;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error(transparent)]
    Persistence(#[from] persistence::PersistenceError),
    #[error("handler '{kind}' rejected payload: {msg}")]
    BadPayload { kind: String, msg: String },
    #[error("handler '{0}' failed: {1}")]
    Handler(
        String,
        #[source] Box<dyn std::error::Error + Send + Sync>,
    ),
}

pub type WorkerResult<T> = std::result::Result<T, WorkerError>;

/// Implemented by every job kind. Stateless objects, registered once and
/// shared across worker tasks.
#[async_trait]
pub trait JobHandler: Send + Sync {
    /// Match value compared against `jobs.kind`.
    fn kind(&self) -> &'static str;
    async fn handle(&self, payload: Value) -> WorkerResult<()>;
}

/// Registry of `kind` → handler. Cheap to clone (uses `Arc` internally).
#[derive(Clone, Default)]
pub struct Registry(Arc<HashMap<&'static str, Arc<dyn JobHandler>>>);

impl Registry {
    pub fn builder() -> RegistryBuilder {
        RegistryBuilder::default()
    }
    pub fn get(&self, kind: &str) -> Option<Arc<dyn JobHandler>> {
        self.0.get(kind).cloned()
    }
    pub fn kinds(&self) -> Vec<&'static str> {
        self.0.keys().copied().collect()
    }
}

#[derive(Default)]
pub struct RegistryBuilder {
    inner: HashMap<&'static str, Arc<dyn JobHandler>>,
}

impl RegistryBuilder {
    pub fn register<H: JobHandler + 'static>(mut self, handler: H) -> Self {
        let kind = handler.kind();
        self.inner.insert(kind, Arc::new(handler));
        self
    }
    pub fn build(self) -> Registry {
        Registry(Arc::new(self.inner))
    }
}

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub pool_size: usize,
    pub poll_idle: Duration,
    pub max_backoff: ChronoDuration,
    pub initial_backoff: ChronoDuration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            pool_size: 4,
            poll_idle: Duration::from_millis(500),
            max_backoff: ChronoDuration::seconds(300),
            initial_backoff: ChronoDuration::seconds(2),
        }
    }
}

impl WorkerConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(s) = std::env::var("WORKER_POOL_SIZE") {
            if let Ok(n) = s.parse::<usize>() {
                if n > 0 {
                    cfg.pool_size = n;
                }
            }
        }
        if let Ok(s) = std::env::var("WORKER_POLL_INTERVAL_MS") {
            if let Ok(n) = s.parse::<u64>() {
                cfg.poll_idle = Duration::from_millis(n);
            }
        }
        cfg
    }
}

/// Pool handle. Drop or call `shutdown()` to drain workers.
pub struct WorkerPool {
    handles: Vec<tokio::task::JoinHandle<()>>,
    cancel: Arc<Notify>,
}

impl WorkerPool {
    /// Trigger a graceful shutdown and await all worker tasks.
    pub async fn shutdown(self) {
        self.cancel.notify_waiters();
        for h in self.handles {
            let _ = h.await;
        }
    }
}

/// Spawn a pool of workers. Returns immediately; tasks are detached but the
/// returned handle can join them later.
pub fn spawn_pool(pool: PgPool, registry: Registry, cfg: WorkerConfig) -> WorkerPool {
    let cancel = Arc::new(Notify::new());
    let mut handles = Vec::with_capacity(cfg.pool_size);

    info!(
        target = "worker",
        pool_size = cfg.pool_size,
        kinds = ?registry.kinds(),
        "starting worker pool"
    );

    for slot in 0..cfg.pool_size {
        let pool = pool.clone();
        let registry = registry.clone();
        let cfg = cfg.clone();
        let cancel = cancel.clone();
        let worker_id = format!("worker-{slot:02}");
        handles.push(tokio::spawn(async move {
            run_slot(pool, registry, cfg, worker_id, cancel).await;
        }));
    }

    WorkerPool { handles, cancel }
}

async fn run_slot(
    pool: PgPool,
    registry: Registry,
    cfg: WorkerConfig,
    worker_id: String,
    cancel: Arc<Notify>,
) {
    info!(target = "worker", id = %worker_id, "slot started");
    loop {
        tokio::select! {
            _ = cancel.notified() => {
                info!(target = "worker", id = %worker_id, "slot received shutdown");
                break;
            }
            res = jobs::claim_next(&pool, &worker_id) => {
                match res {
                    Ok(Some(job)) => {
                        process_one(&pool, &registry, &cfg, &worker_id, job).await;
                    }
                    Ok(None) => {
                        sleep(cfg.poll_idle).await;
                    }
                    Err(err) => {
                        error!(target = "worker", id = %worker_id, error = %err, "claim_next failed");
                        sleep(cfg.poll_idle * 4).await;
                    }
                }
            }
        }
    }
    info!(target = "worker", id = %worker_id, "slot exited");
}

async fn process_one(
    pool: &PgPool,
    registry: &Registry,
    cfg: &WorkerConfig,
    worker_id: &str,
    job: ClaimedJob,
) {
    // Pull the upstream webhook event_id off the payload (when present) so
    // a single correlation key threads webhook → queue → handler logs.
    let event_id = job
        .payload
        .get("event_id")
        .and_then(|v| v.as_str())
        .unwrap_or("-")
        .to_owned();
    let project_id = job
        .project_id
        .map(|p| p.to_string())
        .unwrap_or_else(|| "-".to_owned());
    let span = tracing::info_span!(
        "job",
        worker = worker_id,
        job_id = %job.id,
        kind = %job.kind,
        attempt = job.attempt,
        event_id = %event_id,
        project_id = %project_id
    );
    let _enter = span.enter();
    // Re-parent under the webhook handler's trace (when OTLP is on
    // and the webhook injected `traceparent` into the payload). Noop
    // when either side is unconfigured.
    observability::set_parent_from_payload(&job.payload);
    debug!("dispatch");

    let Some(handler) = registry.get(&job.kind) else {
        warn!("no handler registered; escalating job to dead");
        // Force-kill by spoofing attempt = max so `fail()` transitions to
        // `dead` instead of rescheduling.
        let dead = ClaimedJob {
            attempt: job.max_attempts,
            ..job.clone()
        };
        let _ = jobs::fail(
            pool,
            &dead,
            &format!("no handler for kind '{}'", job.kind),
            cfg.max_backoff,
        )
        .await;
        observability::counter!(
            observability::metrics::JOBS_DONE_TOTAL,
            "kind" => job.kind.clone(),
            "outcome" => "dead",
        )
        .increment(1);
        return;
    };

    let started = std::time::Instant::now();
    let outcome = handler.handle(job.payload.clone()).await;
    let elapsed_secs = started.elapsed().as_secs_f64();

    observability::histogram!(
        observability::metrics::JOB_DURATION_SECONDS,
        "kind" => job.kind.clone()
    )
    .record(elapsed_secs);

    match outcome {
        Ok(()) => {
            if let Err(err) = jobs::complete(pool, job.id).await {
                error!(error = %err, "complete() failed");
            } else {
                debug!("done");
            }
            observability::counter!(
                observability::metrics::JOBS_DONE_TOTAL,
                "kind" => job.kind.clone(),
                "outcome" => "ok",
            )
            .increment(1);
        }
        Err(err) => {
            let backoff = compute_backoff(cfg, job.attempt);
            warn!(error = %err, retry_in_ms = backoff.num_milliseconds(), "handler failed");
            // `attempt` is 1-indexed and `claim_next` already incremented it,
            // so `attempt >= max_attempts` means this was the final retry.
            let outcome_label = if job.attempt >= job.max_attempts {
                "dead"
            } else {
                "fail"
            };
            if let Err(err) = jobs::fail(pool, &job, &err.to_string(), backoff).await {
                error!(error = %err, "fail() failed");
            }
            observability::counter!(
                observability::metrics::JOBS_DONE_TOTAL,
                "kind" => job.kind.clone(),
                "outcome" => outcome_label,
            )
            .increment(1);
        }
    }
}

fn compute_backoff(cfg: &WorkerConfig, attempt: i32) -> ChronoDuration {
    // attempt is 1-indexed (already incremented in claim_next).
    let n = attempt.max(1) as u32;
    let mut secs = cfg.initial_backoff.num_seconds().max(1) as u64;
    for _ in 1..n {
        secs = secs.saturating_mul(2);
    }
    let cap = cfg.max_backoff.num_seconds().max(1) as u64;
    let secs = secs.min(cap);
    ChronoDuration::seconds(secs as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_with_cap() {
        let cfg = WorkerConfig {
            pool_size: 1,
            poll_idle: Duration::from_millis(1),
            initial_backoff: ChronoDuration::seconds(2),
            max_backoff: ChronoDuration::seconds(60),
        };
        assert_eq!(compute_backoff(&cfg, 1).num_seconds(), 2);
        assert_eq!(compute_backoff(&cfg, 2).num_seconds(), 4);
        assert_eq!(compute_backoff(&cfg, 3).num_seconds(), 8);
        assert_eq!(compute_backoff(&cfg, 4).num_seconds(), 16);
        assert_eq!(compute_backoff(&cfg, 5).num_seconds(), 32);
        assert_eq!(compute_backoff(&cfg, 6).num_seconds(), 60); // capped
        assert_eq!(compute_backoff(&cfg, 50).num_seconds(), 60); // still capped
    }

    #[derive(Default)]
    struct CountingHandler {
        kind: &'static str,
    }
    #[async_trait]
    impl JobHandler for CountingHandler {
        fn kind(&self) -> &'static str {
            self.kind
        }
        async fn handle(&self, _payload: Value) -> WorkerResult<()> {
            Ok(())
        }
    }

    #[test]
    fn registry_resolves_handler_by_kind() {
        let reg = Registry::builder()
            .register(CountingHandler { kind: "AAA" })
            .register(CountingHandler { kind: "BBB" })
            .build();
        assert!(reg.get("AAA").is_some());
        assert!(reg.get("BBB").is_some());
        assert!(reg.get("ZZZ").is_none());
        let mut kinds = reg.kinds();
        kinds.sort();
        assert_eq!(kinds, vec!["AAA", "BBB"]);
    }
}
