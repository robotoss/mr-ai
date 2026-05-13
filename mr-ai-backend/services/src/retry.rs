//! Bounded exponential-backoff retry helper.
//!
//! Used by outbound calls (Git providers, LLM gateway, Qdrant) so a
//! single transient failure does not abort a job. The classifier decides
//! whether a given error is worth retrying — non-retryable errors short-
//! circuit immediately. Backoff is exponential with optional jitter and
//! capped by `max_backoff`.

use std::future::Future;
use std::time::Duration;

use tokio::time::sleep;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    /// Multiplicative jitter applied to the computed backoff. Values like
    /// `0.2` mean ±20%. Keeps stampedes from synchronising.
    pub jitter: f32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(10),
            jitter: 0.2,
        }
    }
}

impl RetryPolicy {
    pub const fn fast() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(2),
            jitter: 0.2,
        }
    }

    pub fn compute_backoff(&self, attempt: u32) -> Duration {
        let exp = 2u64.saturating_pow(attempt.saturating_sub(1));
        let base = self.initial_backoff.saturating_mul(exp as u32);
        let capped = std::cmp::min(base, self.max_backoff);
        // Pseudo-jitter without an RNG dep: derive from attempt number so
        // tests are deterministic; spreading is good-enough for our use.
        let jitter_factor = 1.0
            + (self.jitter * (((attempt as i32 % 7) - 3) as f32 / 3.0))
                .clamp(-self.jitter, self.jitter);
        let nanos = (capped.as_nanos() as f32 * jitter_factor.max(0.5)) as u128;
        Duration::from_nanos(nanos as u64)
    }
}

/// Run `op` with retries. The classifier returns `true` to retry on a
/// given error. Non-retryable errors propagate on the first failure.
///
/// Errors carry no rich type — the caller's existing `Result<T, E>` flows
/// through unchanged.
pub async fn retry_async<T, E, Fut, F, C>(
    label: &'static str,
    policy: &RetryPolicy,
    classifier: C,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    C: Fn(&E) -> bool,
    E: std::fmt::Display,
{
    let mut attempt = 1u32;
    loop {
        match op().await {
            Ok(value) => {
                if attempt > 1 {
                    debug!(target = "retry", %label, attempt, "succeeded after retry");
                }
                return Ok(value);
            }
            Err(err) => {
                if !classifier(&err) || attempt >= policy.max_attempts {
                    return Err(err);
                }
                let delay = policy.compute_backoff(attempt);
                warn!(
                    target = "retry",
                    %label,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    error = %err,
                    "retrying"
                );
                sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// Convenience: retry every error.
pub fn retry_any<E>(_err: &E) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn retries_until_success() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_clone = calls.clone();
        let result: Result<u32, &'static str> = retry_async(
            "ok-after-2",
            &RetryPolicy {
                max_attempts: 5,
                initial_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(2),
                jitter: 0.0,
            },
            retry_any,
            move || {
                let calls_inner = calls_clone.clone();
                async move {
                    let n = calls_inner.fetch_add(1, Ordering::SeqCst) + 1;
                    if n < 3 {
                        Err("transient")
                    } else {
                        Ok(n)
                    }
                }
            },
        )
        .await;
        assert_eq!(result, Ok(3));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stops_after_max_attempts() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_clone = calls.clone();
        let result: Result<(), &'static str> = retry_async(
            "always-fails",
            &RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(2),
                jitter: 0.0,
            },
            retry_any,
            move || {
                let calls_inner = calls_clone.clone();
                async move {
                    calls_inner.fetch_add(1, Ordering::SeqCst);
                    Err("nope")
                }
            },
        )
        .await;
        assert_eq!(result, Err("nope"));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn classifier_short_circuits_non_retryable() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_clone = calls.clone();
        let result: Result<(), &'static str> = retry_async(
            "fatal",
            &RetryPolicy {
                max_attempts: 5,
                initial_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(2),
                jitter: 0.0,
            },
            |e| *e != "fatal",
            move || {
                let calls_inner = calls_clone.clone();
                async move {
                    calls_inner.fetch_add(1, Ordering::SeqCst);
                    Err("fatal")
                }
            },
        )
        .await;
        assert_eq!(result, Err("fatal"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn backoff_doubles_until_cap() {
        let policy = RetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(80),
            jitter: 0.0,
        };
        assert_eq!(policy.compute_backoff(1), Duration::from_millis(10));
        assert_eq!(policy.compute_backoff(2), Duration::from_millis(20));
        assert_eq!(policy.compute_backoff(3), Duration::from_millis(40));
        assert_eq!(policy.compute_backoff(4), Duration::from_millis(80));
        assert_eq!(policy.compute_backoff(5), Duration::from_millis(80));
    }
}
