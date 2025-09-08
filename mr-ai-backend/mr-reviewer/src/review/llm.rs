//! LLM layer with dual-model routing (fast + slow).

use std::time::Duration;
use tracing::debug;

use crate::errors::Error;

#[derive(Debug, Clone, Copy)]
pub enum LlmKind {
    Ollama,
}

/// Model configuration (per endpoint).
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub model: String,
    pub endpoint: String,
    pub max_tokens: Option<u32>,
}

/// Escalation policy knobs controlling when to call the SLOW model.
#[derive(Debug, Clone)]
pub struct EscalationPolicy {
    pub enabled: bool,
    pub max_escalations: usize,
    pub min_severity: crate::review::policy::Severity,
    pub min_confidence: f32,
    pub long_prompt_tokens: usize,
}

impl EscalationPolicy {
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

/// Config for routing between FAST and SLOW models.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub kind: LlmKind,
    pub fast: ModelConfig,
    pub slow: ModelConfig,
    pub routing: EscalationPolicy,
}

impl LlmConfig {
    pub fn from_env() -> Self {
        let endpoint =
            std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
        let slow_model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:32b".to_string());
        let fast_model =
            std::env::var("OLLAMA_MODEL_FAST_MODEL").unwrap_or_else(|_| slow_model.clone());

        let routing = EscalationPolicy::from_env();

        LlmConfig {
            kind: LlmKind::Ollama,
            fast: ModelConfig {
                model: fast_model,
                endpoint: endpoint.clone(),
                max_tokens: None,
            },
            slow: ModelConfig {
                model: slow_model,
                endpoint,
                max_tokens: None,
            },
            routing,
        }
    }
}

/// Thin Ollama client reused by router.
#[derive(Debug, Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    cfg: ModelConfig,
}

impl LlmClient {
    pub fn new(cfg: ModelConfig) -> Self {
        let http = reqwest::Client::builder()
            .http2_keep_alive_interval(Some(Duration::from_secs(20)))
            .pool_idle_timeout(Some(Duration::from_secs(90)))
            .tcp_keepalive(Some(Duration::from_secs(30)))
            .build()
            .expect("http client");
        Self { http, cfg }
    }

    /// Best-effort warmup to avoid cold starts.
    pub async fn warmup(&self) {
        let _ = self.generate_raw("ping").await;
    }

    /// Minimal `/api/generate` wrapper, returns plain text.
    pub async fn generate_raw(&self, prompt: &str) -> Result<String, Error> {
        #[derive(serde::Serialize)]
        struct Req<'a> {
            model: &'a str,
            prompt: &'a str,
            stream: bool,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            response: String,
        }

        let url = format!("{}/api/generate", self.cfg.endpoint.trim_end_matches('/'));
        debug!("llm.generate model={} url={}", self.cfg.model, url);
        let resp = self
            .http
            .post(&url)
            .json(&Req {
                model: &self.cfg.model,
                prompt,
                stream: false,
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(crate::errors::Error::Provider(
                crate::errors::ProviderError::HttpStatus(resp.status().as_u16()),
            ));
        }
        let body: Resp = resp.json().await?;
        Ok(body.response)
    }
}

/// Target kind hint for routing.
/// Keep this local (duplicated) to avoid depending on mapping module here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKindHint {
    Line,
    Range,
    Symbol,
    File,
    Global,
}

/// Routing hint supplied by the caller (step4 orchestrator).
#[derive(Debug, Clone)]
pub struct RouteHint {
    pub target_kind: TargetKindHint,
    /// Approx prompt tokens (4 chars ≈ 1 token heuristic).
    pub prompt_tokens_approx: usize,
    /// Severity estimated/parsed from FAST (or expected).
    pub severity: crate::review::policy::Severity,
    /// Confidence estimated by heuristics (0..1).
    pub confidence: f32,
    /// Already used slow escalations in this run.
    pub used_escalations: usize,
    /// Optional: range span in lines (helps detect "big range").
    pub range_span_lines: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDecision {
    Fast,
    Slow,
}

impl RouteDecision {
    pub fn is_slow(self) -> bool {
        matches!(self, RouteDecision::Slow)
    }
}

/// Router holding both fast and slow clients + policy.
#[derive(Debug, Clone)]
pub struct LlmRouter {
    pub fast: LlmClient,
    pub slow: LlmClient,
    pub policy: EscalationPolicy,
}

impl LlmRouter {
    pub fn from_config(cfg: LlmConfig) -> Self {
        let fast = LlmClient::new(cfg.fast);
        let slow = LlmClient::new(cfg.slow);
        Self {
            fast,
            slow,
            policy: cfg.routing,
        }
    }

    pub async fn generate_fast(&self, prompt: &str) -> Result<String, Error> {
        self.fast.generate_raw(prompt).await
    }
    pub async fn generate_slow(&self, prompt: &str) -> Result<String, Error> {
        self.slow.generate_raw(prompt).await
    }

    /// Decide whether to escalate *after* FAST (legacy path).
    /// Kept for compatibility; used when we already ran FAST.
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

        // Severity is a gate: if finding is below gate, we never escalate.
        let sev_gate = rank(sev) >= rank(self.policy.min_severity);

        // Signals
        let conf_low = confidence < self.policy.min_confidence;
        let too_long = prompt_tokens_approx > self.policy.long_prompt_tokens;

        sev_gate && (conf_low || too_long)
    }

    /// Decide whether to route directly to SLOW *before* running FAST.
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

fn rank(s: crate::review::policy::Severity) -> u8 {
    match s {
        crate::review::policy::Severity::High => 3,
        crate::review::policy::Severity::Medium => 2,
        crate::review::policy::Severity::Low => 1,
    }
}
