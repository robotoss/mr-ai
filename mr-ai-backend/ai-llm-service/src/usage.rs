//! Usage history & cost analytics — persistent JSONL log + in-memory aggregates.
//!
//! Goals:
//! - **Replayable history.** Every gateway call appends one line to a
//!   JSONL file (default `logs/usage.jsonl`). Lines are self-contained,
//!   schema-stable, and grep-/jq-friendly.
//! - **Live counters.** The gateway exposes
//!   [`LlmGateway::usage_snapshot`](crate::gateway::LlmGateway::usage_snapshot)
//!   that returns totals (calls, tokens, cost) plus a per-(tier, provider,
//!   model) breakdown, suitable for `/usage` HTTP response or dashboards.
//! - **Pluggable.** Persistence happens through the [`UsageRecorder`]
//!   trait, so tests can swap in a no-op or in-memory recorder, and a
//!   future SQLite/Kafka recorder fits without touching call sites.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::ProviderKind;

/* --------------------------------------------------------------------- */
/* Record types                                                          */
/* --------------------------------------------------------------------- */

/// What kind of call produced this record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageKind {
    Completion,
    Embedding,
}

/// One persisted record per gateway call. JSONL line shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub timestamp: DateTime<Utc>,
    pub request_id: String,
    pub kind: UsageKind,
    pub tier: String,
    pub provider: ProviderKind,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: f64,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<usize>,
    /// Truncated prompt (only when `USAGE_LOG_INCLUDE_PROMPTS=true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_preview: Option<String>,
    /// Truncated response (only when `USAGE_LOG_INCLUDE_PROMPTS=true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_preview: Option<String>,
}

/* --------------------------------------------------------------------- */
/* Recorder trait + concrete impls                                       */
/* --------------------------------------------------------------------- */

/// Persistence sink for [`UsageRecord`]s. Implementations must be
/// thread-safe.
pub trait UsageRecorder: Send + Sync + std::fmt::Debug {
    /// Persist a single record. Errors are intentionally swallowed by
    /// implementations and reported via `tracing::warn!` — recording
    /// failures must never break the calling request.
    fn record(&self, rec: &UsageRecord);
}

/// No-op recorder used in tests or when persistence is disabled.
#[derive(Debug, Default)]
pub struct NoopUsageRecorder;

impl UsageRecorder for NoopUsageRecorder {
    fn record(&self, _rec: &UsageRecord) {}
}

/// Append-only JSONL recorder. One line per call, fsync-on-newline by the
/// OS; no rotation (the file grows; rotate externally with `logrotate` or
/// `cron` if needed).
#[derive(Debug)]
pub struct JsonlUsageRecorder {
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl JsonlUsageRecorder {
    pub fn new(path: PathBuf) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path,
            write_lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl UsageRecorder for JsonlUsageRecorder {
    fn record(&self, rec: &UsageRecord) {
        let line = match serde_json::to_string(rec) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "failed to serialise UsageRecord");
                return;
            }
        };

        let _guard = match self.write_lock.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(), // poisoned mutex — keep writing anyway
        };

        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{line}") {
                    warn!(path = %self.path.display(), error = %e, "failed to write usage record");
                }
            }
            Err(e) => warn!(path = %self.path.display(), error = %e, "failed to open usage log"),
        }
    }
}

/* --------------------------------------------------------------------- */
/* In-memory counters                                                    */
/* --------------------------------------------------------------------- */

/// Per-(tier, provider, model) running totals.
#[derive(Debug, Default, Clone, Serialize)]
pub struct TierModelStats {
    pub calls: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd: f64,
}

/// Aggregated snapshot of all calls observed by the gateway since process
/// start. Returned by [`LlmGateway::usage_snapshot`].
#[derive(Debug, Default, Clone, Serialize)]
pub struct UsageSnapshot {
    pub total_calls: u64,
    pub total_completions: u64,
    pub total_embeddings: u64,
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    /// Map key is `"<tier>/<provider>/<model>"` for easy JSON consumption.
    pub by_tier_model: HashMap<String, TierModelStats>,
    pub since: Option<DateTime<Utc>>,
    pub last_call_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default)]
pub(crate) struct UsageCounters {
    inner: RwLock<UsageInner>,
}

#[derive(Debug, Default)]
struct UsageInner {
    snapshot: UsageSnapshot,
}

impl UsageCounters {
    pub fn new() -> Self {
        let mut inner = UsageInner::default();
        inner.snapshot.since = Some(Utc::now());
        Self {
            inner: RwLock::new(inner),
        }
    }

    pub fn observe(&self, rec: &UsageRecord) {
        let mut w = match self.inner.write() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let s = &mut w.snapshot;
        s.total_calls += 1;
        match rec.kind {
            UsageKind::Completion => s.total_completions += 1,
            UsageKind::Embedding => s.total_embeddings += 1,
        }
        s.total_prompt_tokens += rec.prompt_tokens as u64;
        s.total_completion_tokens += rec.completion_tokens as u64;
        s.total_tokens += rec.total_tokens as u64;
        s.total_cost_usd += rec.cost_usd;
        s.last_call_at = Some(rec.timestamp);

        let key = format!("{}/{}/{}", rec.tier, rec.provider, rec.model);
        let entry = s.by_tier_model.entry(key).or_default();
        entry.calls += 1;
        entry.prompt_tokens += rec.prompt_tokens as u64;
        entry.completion_tokens += rec.completion_tokens as u64;
        entry.total_tokens += rec.total_tokens as u64;
        entry.cost_usd += rec.cost_usd;
    }

    pub fn snapshot(&self) -> UsageSnapshot {
        let r = match self.inner.read() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        r.snapshot.clone()
    }
}

/* --------------------------------------------------------------------- */
/* Helpers                                                               */
/* --------------------------------------------------------------------- */

/// Truncate a string to `max_chars` Unicode chars, suffixing `…` if cut.
pub fn truncate_preview(s: &str, max_chars: usize) -> String {
    if max_chars == 0 || s.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut count = 0usize;
    for ch in s.chars() {
        if count + 1 > max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
        count += 1;
    }
    debug!(input_len = s.len(), out_len = out.len(), "preview truncated");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn rec(kind: UsageKind, tier: &str, model: &str, prompt: u32, completion: u32, cost: f64) -> UsageRecord {
        UsageRecord {
            timestamp: Utc::now(),
            request_id: "test".into(),
            kind,
            tier: tier.into(),
            provider: ProviderKind::Ollama,
            model: model.into(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cost_usd: cost,
            latency_ms: 1,
            batch_size: None,
            prompt_preview: None,
            response_preview: None,
        }
    }

    #[test]
    fn jsonl_recorder_appends_one_line_per_record() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("usage.jsonl");
        let r = JsonlUsageRecorder::new(path.clone()).unwrap();

        r.record(&rec(UsageKind::Completion, "fast", "llama3", 10, 5, 0.001));
        r.record(&rec(UsageKind::Embedding, "default", "bge-m3", 12, 0, 0.0));

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: UsageRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.kind, UsageKind::Completion);
        assert_eq!(parsed.prompt_tokens, 10);
    }

    #[test]
    fn counters_aggregate_by_tier_provider_model() {
        let c = UsageCounters::new();
        c.observe(&rec(UsageKind::Completion, "fast", "llama3", 10, 5, 0.001));
        c.observe(&rec(UsageKind::Completion, "fast", "llama3", 20, 8, 0.002));
        c.observe(&rec(UsageKind::Embedding, "default", "bge-m3", 12, 0, 0.0));

        let s = c.snapshot();
        assert_eq!(s.total_calls, 3);
        assert_eq!(s.total_completions, 2);
        assert_eq!(s.total_embeddings, 1);
        assert_eq!(s.total_prompt_tokens, 42);
        assert_eq!(s.total_completion_tokens, 13);
        assert!((s.total_cost_usd - 0.003).abs() < 1e-9);

        let fast_llama = s.by_tier_model.get("fast/ollama/llama3").unwrap();
        assert_eq!(fast_llama.calls, 2);
        assert_eq!(fast_llama.prompt_tokens, 30);

        let embed = s.by_tier_model.get("default/ollama/bge-m3").unwrap();
        assert_eq!(embed.calls, 1);
    }

    #[test]
    fn truncate_preview_cuts_at_char_boundary_with_ellipsis() {
        assert_eq!(truncate_preview("hello", 10), "hello");
        assert_eq!(truncate_preview("hello world", 5), "hello…");
        assert_eq!(truncate_preview("café", 3), "caf…");
        assert_eq!(truncate_preview("", 10), "");
        assert_eq!(truncate_preview("anything", 0), "");
    }
}
