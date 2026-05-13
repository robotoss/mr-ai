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
use std::sync::{Mutex, OnceLock, RwLock};

use chrono::{DateTime, Utc};
use regex::Regex;
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
    /// Versioned prompt template label (e.g. `"per_hypothesis@v1"`).
    /// Empty for callers that don't pass `prompt_id` on the request.
    /// Sprint 4c — drives A/B telemetry off the existing JSONL.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prompt_id: Option<String>,
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
        // Pre-create the file with 0600 on Unix so even an external reader on
        // the same host can't slurp it without explicit permission. The first
        // write would create it with the umask-default mode otherwise.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&path);
        }
        #[cfg(not(unix))]
        {
            let _ = OpenOptions::new().create(true).append(true).open(&path);
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

/// High-confidence secret patterns. Each entry is `(name, regex)`.
///
/// Matches are replaced with `[REDACTED:NAME]`. Patterns are anchored at
/// well-known prefixes / shapes to keep false positives near zero on real
/// code-review prompts.
fn secret_patterns() -> &'static [(&'static str, Regex)] {
    static PATTERNS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let raw: &[(&str, &str)] = &[
            // OpenAI & Anthropic style: sk-..., sk-ant-..., sk-proj-...
            ("OPENAI_OR_ANTHROPIC_KEY", r"\bsk-(?:ant-|proj-)?[A-Za-z0-9_\-]{20,}\b"),
            // AWS access key id (AKIA…, ASIA… for STS, AGPA…/AIDA… service principals).
            ("AWS_ACCESS_KEY_ID", r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASCA)[0-9A-Z]{16}\b"),
            // GitHub tokens (PAT, OAuth, server-to-server, refresh).
            ("GITHUB_TOKEN", r"\bgh[opusr]_[A-Za-z0-9]{30,}\b"),
            // GitLab personal access token.
            ("GITLAB_PAT", r"\bglpat-[A-Za-z0-9_\-]{20,}\b"),
            // Slack tokens.
            ("SLACK_TOKEN", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
            // Google API keys.
            ("GOOGLE_API_KEY", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
            // JWT (header.payload.signature, all base64url).
            (
                "JWT",
                r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\b",
            ),
            // `Authorization: Bearer …` headers.
            ("BEARER_TOKEN", r"(?i)\bbearer\s+[A-Za-z0-9._\-]{10,}\b"),
            // Private keys — match the BEGIN line; full block redacted via the
            // surrounding truncation since it'll exceed the preview budget.
            ("PRIVATE_KEY", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
        ];
        raw.iter()
            .map(|(name, p)| (*name, Regex::new(p).expect("hard-coded regex is valid")))
            .collect()
    })
}

/// Replaces high-confidence secret patterns in `s` with `[REDACTED:KIND]`.
///
/// Designed for previews persisted in the usage log: even when an operator
/// turns on `USAGE_LOG_INCLUDE_PROMPTS`, well-known credentials embedded in
/// prompts (think: someone pasted an `Authorization: Bearer …` header into a
/// review request) do not land on disk verbatim.
pub fn redact_secrets(s: &str) -> String {
    let mut out = s.to_string();
    for (name, re) in secret_patterns() {
        out = re.replace_all(&out, format!("[REDACTED:{name}]")).into_owned();
    }
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
            prompt_id: None,
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

    #[test]
    fn redact_openai_anthropic_keys() {
        let s = "use sk-proj-abcdefghijKLMNOPQRSTUVWXYZ012345 here";
        let out = redact_secrets(s);
        assert!(out.contains("[REDACTED:OPENAI_OR_ANTHROPIC_KEY]"));
        assert!(!out.contains("sk-proj-abcdef"));

        let s2 = "key=sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        assert!(redact_secrets(s2).contains("[REDACTED:OPENAI_OR_ANTHROPIC_KEY]"));
    }

    #[test]
    fn redact_aws_access_key_id() {
        let s = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE";
        assert_eq!(
            redact_secrets(s),
            "AWS_ACCESS_KEY_ID=[REDACTED:AWS_ACCESS_KEY_ID]"
        );

        // STS session credentials.
        let s2 = "creds: ASIAY34FZKBOKMUTVV7A end";
        assert!(redact_secrets(s2).contains("[REDACTED:AWS_ACCESS_KEY_ID]"));
    }

    #[test]
    fn redact_github_gitlab_slack_google() {
        assert!(
            redact_secrets("token=ghp_abcdefghijklmnopqrstuvwxyz0123456789")
                .contains("[REDACTED:GITHUB_TOKEN]")
        );
        assert!(
            redact_secrets("glpat-AAAAAAAAAAAAAAAAAAAA").contains("[REDACTED:GITLAB_PAT]")
        );
        assert!(
            redact_secrets("xoxb-1234567890-ABCDEFGHIJ").contains("[REDACTED:SLACK_TOKEN]")
        );
        // Google API keys are AIza + exactly 35 chars (39 total).
        let google_key = format!("AIza{}", "B".repeat(35));
        assert!(
            redact_secrets(&google_key).contains("[REDACTED:GOOGLE_API_KEY]"),
            "missed: {google_key}"
        );
    }

    #[test]
    fn redact_jwt_and_bearer_and_private_key() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.SflKxwRJSMeKKF2QT4fwpMeJf36";
        assert!(redact_secrets(jwt).contains("[REDACTED:JWT]"));

        let auth = "Authorization: Bearer abc.def-XYZ_0123456789";
        assert!(redact_secrets(auth).contains("[REDACTED:BEARER_TOKEN]"));

        let pk = "-----BEGIN RSA PRIVATE KEY-----\nMIIE...";
        assert!(redact_secrets(pk).contains("[REDACTED:PRIVATE_KEY]"));
    }

    #[test]
    fn redact_leaves_innocuous_text_untouched() {
        let s = "Refactor `LlmGateway::complete` to log token usage and emit info!()";
        assert_eq!(redact_secrets(s), s);
    }
}
