//! LLM routing layer on top of `ai-llm-service` profiles (fast + slow + embedding).
//!
//! This module keeps only the *routing* logic (escalation policy and decisions)
//! and delegates actual generation/embedding calls to `LlmServiceProfiles`.
//!
//! - No HTTP code here.
//! - No provider-specific structs here.
//! - All inference is performed by `ai-llm-service`.
//!
//! Typical usage:
//! ```no_run
//! use std::sync::Arc;
//! use ai_llm_service::service_profiles::LlmServiceProfiles;
//! use crate::llm_router::{LlmRouter, EscalationPolicy, RouteHint, TargetKindHint};
//!
//! async fn example(router: &LlmRouter) -> Result<(), crate::errors::Error> {
//!     // Route before running FAST
//!     let decision = router.route_for(&RouteHint {
//!         target_kind: TargetKindHint::Symbol,
//!         prompt_tokens_approx: 3_000,
//!         severity: crate::review::policy::Severity::High,
//!         confidence: 0.5,
//!         used_escalations: 0,
//!         range_span_lines: Some(120),
//!     });
//!
//!     let text = match decision {
//!         crate::llm_router::RouteDecision::Fast => router.generate_fast("prompt").await?,
//!         crate::llm_router::RouteDecision::Slow => router.generate_slow("prompt").await?,
//!     };
//!     println!("{}", text);
//!     Ok(())
//! }
//! ```

use ai_llm_service::service_profiles::LlmServiceProfiles;
use std::sync::Arc;
use tracing::debug;

use crate::errors::ProviderError;

/// Routing hint target granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKindHint {
    /// Single-line hint (very localized).
    Line,
    /// Range of lines (medium locality).
    Range,
    /// Symbol-level (function/module/class) — usually harder reasoning.
    Symbol,
    /// Entire file.
    File,
    /// Global or cross-file reasoning.
    Global,
}

/// Routing hint supplied by the caller.
#[derive(Debug, Clone)]
pub struct RouteHint {
    /// Target kind (helps detect heavy reasoning upfront).
    pub target_kind: TargetKindHint,
    /// Approximate prompt tokens (heuristic: ~4 chars ≈ 1 token).
    pub prompt_tokens_approx: usize,
    /// Estimated/expected severity.
    pub severity: crate::review::policy::Severity,
    /// Estimated confidence in the current result (0..1).
    pub confidence: f32,
    /// Already used slow escalations in this run.
    pub used_escalations: usize,
    /// Optional line-span for `Range`.
    pub range_span_lines: Option<usize>,
}

/// Router decision: run with the fast or the slow profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    /// Use fast profile.
    Fast,
    /// Use slow profile (higher quality / cost).
    Slow,
}

impl RouteDecision {
    /// Returns `true` if the decision is slow.
    pub fn is_slow(self) -> bool {
        matches!(self, RouteDecision::Slow)
    }
}

/// Escalation policy controlling when to call the SLOW model.
#[derive(Debug, Clone)]
pub struct EscalationPolicy {
    /// Master switch.
    pub enabled: bool,
    /// Upper bound on number of slow escalations in a run.
    pub max_escalations: usize,
    /// Minimum severity gate required to allow escalation.
    pub min_severity: crate::review::policy::Severity,
    /// Escalate when confidence is below this threshold.
    pub min_confidence: f32,
    /// Escalate when prompt tokens exceed this threshold.
    pub long_prompt_tokens: usize,
}

impl EscalationPolicy {
    /// Loads escalation knobs from environment variables.
    ///
    /// - `REVIEW_ESCALATE_ENABLED` (default: `"true"`)
    /// - `REVIEW_ESCALATE_MAX` (default: `5`)
    /// - `REVIEW_ESCALATE_SEVERITY` (`"High"|"Medium"|"Low"`, default: `"High"`)
    /// - `REVIEW_ESCALATE_MIN_CONF` (default: `0.55`)
    /// - `REVIEW_ESCALATE_LONG_PROMPT_TOK` (default: `2500`)
    pub fn from_env() -> Self {
        let enabled =
            std::env::var("REVIEW_ESCALATE_ENABLED").unwrap_or_else(|_| "true".into()) == "true";
        let max_escalations = std::env::var("REVIEW_ESCALATE_MAX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        let min_severity = match std::env::var("REVIEW_ESCALATE_SEVERITY")
            .unwrap_or_else(|_| "High".into())
            .as_str()
        {
            "High" => crate::review::policy::Severity::High,
            "Medium" => crate::review::policy::Severity::Medium,
            _ => crate::review::policy::Severity::Low,
        };
        let min_confidence = std::env::var("REVIEW_ESCALATE_MIN_CONF")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.55);
        let long_prompt_tokens = std::env::var("REVIEW_ESCALATE_LONG_PROMPT_TOK")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);

        Self {
            enabled,
            max_escalations,
            min_severity,
            min_confidence,
            long_prompt_tokens,
        }
    }
}

/// Thin router that delegates all inference to `LlmServiceProfiles` and
/// applies an escalation policy for deciding between fast and slow runs.
#[derive(Debug, Clone)]
pub struct LlmRouter {
    /// Shared profiles service (fast/slow/embedding) from `ai-llm-service`.
    pub svc: Arc<LlmServiceProfiles>,
    /// Escalation policy knobs.
    pub policy: EscalationPolicy,
}

impl LlmRouter {
    /// Creates a new router using the provided shared profiles service.
    pub fn new(svc: Arc<LlmServiceProfiles>, policy: EscalationPolicy) -> Self {
        Self { svc, policy }
    }

    /// Generates with the **fast** profile.
    ///
    /// # Errors
    /// Maps [`AiLlmError`] into your crate's `Error` via `From`.
    pub async fn generate_fast(&self, prompt: &str) -> Result<String, crate::errors::Error> {
        debug!("router: generate_fast");
        self.svc
            .generate_fast(prompt, None)
            .await
            .map_err(|_| crate::errors::Error::Provider(ProviderError::Forbidden))
    }

    /// Generates with the **slow** profile.
    ///
    /// If slow profile is not configured, the profiles service falls back to fast.
    ///
    /// # Errors
    /// Maps [`AiLlmError`] into your crate's `Error` via `From`.
    pub async fn generate_slow(&self, prompt: &str) -> Result<String, crate::errors::Error> {
        debug!("router: generate_slow");
        self.svc
            .generate_slow(prompt, None)
            .await
            .map_err(|_| crate::errors::Error::Provider(ProviderError::Forbidden))
    }

    /// Decide whether to escalate **after** FAST (legacy path).
    ///
    /// Use this when you already ran FAST and want to decide if SLOW is needed.
    pub fn should_escalate(
        &self,
        sev: crate::review::policy::Severity,
        confidence: f32,
        prompt_tokens_approx: usize,
        used_escalations: usize,
    ) -> bool {
        if !self.policy.enabled {
            return false;
        }
        if used_escalations >= self.policy.max_escalations {
            return false;
        }

        // Severity gate: if finding is below gate, we never escalate.
        let sev_gate = rank(sev) >= rank(self.policy.min_severity);

        // Signals
        let conf_low = confidence < self.policy.min_confidence;
        let too_long = prompt_tokens_approx > self.policy.long_prompt_tokens;

        sev_gate && (conf_low || too_long)
    }

    /// Decide whether to route directly to SLOW **before** running FAST.
    ///
    /// This prevents wasteful double-inference on obviously heavy targets.
    pub fn route_for(&self, hint: &RouteHint) -> RouteDecision {
        if !self.policy.enabled {
            return RouteDecision::Fast;
        }
        if hint.used_escalations >= self.policy.max_escalations {
            return RouteDecision::Fast;
        }

        // Severity gate first.
        let sev_gate = rank(hint.severity) >= rank(self.policy.min_severity);
        if !sev_gate {
            return RouteDecision::Fast;
        }

        // Heuristics: what is "clearly heavy" upfront?
        let is_symbol = matches!(hint.target_kind, TargetKindHint::Symbol);
        let is_big_range = matches!(hint.target_kind, TargetKindHint::Range)
            && hint.range_span_lines.unwrap_or(0) >= 40; // tune as needed
        let too_long = hint.prompt_tokens_approx > self.policy.long_prompt_tokens;
        let conf_low = hint.confidence < self.policy.min_confidence;

        // Direct-to-slow when:
        //  - Symbol or big range (harder reasoning), AND
        //  - either long context or low confidence.
        if (is_symbol || is_big_range) && (too_long || conf_low) {
            return RouteDecision::Slow;
        }

        // Additional "near-threshold" guard:
        // If tokens are close to threshold and confidence only slightly below,
        // prefer SLOW to avoid double pass.
        let near_long = hint.prompt_tokens_approx > (self.policy.long_prompt_tokens * 9 / 10);
        let slightly_low_conf = hint.confidence < (self.policy.min_confidence + 0.05);
        if (is_symbol || is_big_range) && near_long && slightly_low_conf {
            return RouteDecision::Slow;
        }

        RouteDecision::Fast
    }
}

/* --------------------- helpers --------------------- */

fn rank(s: crate::review::policy::Severity) -> u8 {
    match s {
        crate::review::policy::Severity::High => 3,
        crate::review::policy::Severity::Medium => 2,
        crate::review::policy::Severity::Low => 1,
    }
}
