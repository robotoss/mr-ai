//! LLM layer with dual-model routing (fast + slow) for step 4.
//!
//! - `LlmClient` provides minimal Ollama /api/generate wrapper.
//! - `LlmRouter` decides whether to keep fast draft or escalate to slow model.
//! - `LlmConfig::from_env()` reads both legacy keys (OLLAMA_MODEL/URL)
//!   and the optional fast model key OLLAMA_MODEL_FAST_MODEL.
//!
//! Design goals:
//! - No async-trait, no boxed trait objects.
//! - Keep-alive client to avoid reconnect churn.
//! - Best-effort warmup to reduce first-token latency.

use std::time::Duration;

use reqwest::Client;
use tracing::debug;

#[derive(Debug, Clone, Copy)]
pub enum LlmKind {
    /// Only Ollama is implemented.
    Ollama,
}

/// Model configuration (per endpoint).
#[derive(Debug, Clone)]
pub struct ModelConfig {
    /// Model name (e.g., "qwen3:14b", "qwen3:32b").
    pub model: String,
    /// HTTP endpoint (e.g., "http://127.0.0.1:11434").
    pub endpoint: String,
    /// Optional max tokens (reserved for future).
    pub max_tokens: Option<u32>,
}

/// Escalation policy knobs controlling when to call the SLOW model.
#[derive(Debug, Clone)]
pub struct EscalationPolicy {
    pub enabled: bool,
    pub max_escalations: usize,
    pub min_severity: String, // "Low" | "Medium" | "High"
    pub min_confidence: f32,  // 0..1
    pub long_prompt_tokens: usize,
    pub group_simple_targets: bool,
}

impl EscalationPolicy {
    pub fn from_env() -> Self {
        let enabled =
            std::env::var("REVIEW_ESCALATE_ENABLED").unwrap_or_else(|_| "true".into()) == "true";
        let max_escalations = std::env::var("REVIEW_ESCALATE_MAX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
        let min_severity =
            std::env::var("REVIEW_ESCALATE_SEVERITY").unwrap_or_else(|_| "High".into());
        let min_confidence = std::env::var("REVIEW_ESCALATE_MIN_CONF")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.55);
        let long_prompt_tokens = std::env::var("REVIEW_ESCALATE_LONG_PROMPT_TOK")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        let group_simple_targets = std::env::var("REVIEW_GROUP_SIMPLE_TARGETS")
            .unwrap_or_else(|_| "true".into())
            == "true";

        Self {
            enabled,
            max_escalations,
            min_severity,
            min_confidence,
            long_prompt_tokens,
            group_simple_targets,
        }
    }
}

/// Config for routing between FAST and SLOW models.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub kind: LlmKind,
    /// Fast model for mass drafting (speed).
    pub fast: ModelConfig,
    /// Slow model for selective refine (quality).
    pub slow: ModelConfig,
    /// Escalation knobs.
    pub routing: EscalationPolicy,
}

impl LlmConfig {
    /// Build config from environment without breaking legacy keys.
    ///
    /// Env:
    /// - OLLAMA_URL (required)
    /// - OLLAMA_MODEL (slow/default, required)
    /// - OLLAMA_MODEL_FAST_MODEL (optional → fast==slow if missing)
    /// - REVIEW_* (optional)
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
    http: Client,
    cfg: ModelConfig,
}

impl LlmClient {
    pub fn new(cfg: ModelConfig) -> Self {
        let http = Client::builder()
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
    pub async fn generate_raw(&self, prompt: &str) -> Result<String, crate::errors::Error> {
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

    /// Decide whether to escalate to SLOW model based on severity/confidence/length/budget.
    pub fn should_escalate(
        &self,
        severity: &str,
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
        let sev_ok = severity_ge(severity, &self.policy.min_severity);
        let conf_low = confidence < self.policy.min_confidence;
        let too_long = prompt_tokens_approx > self.policy.long_prompt_tokens;
        sev_ok || conf_low || too_long
    }

    pub async fn generate_fast(&self, prompt: &str) -> Result<String, crate::errors::Error> {
        self.fast.generate_raw(prompt).await
    }
    pub async fn generate_slow(&self, prompt: &str) -> Result<String, crate::errors::Error> {
        self.slow.generate_raw(prompt).await
    }
}

/// Compare severities ("Low" < "Medium" < "High").
fn severity_ge(a: &str, b: &str) -> bool {
    fn rank(s: &str) -> u8 {
        match s {
            "High" => 3,
            "Medium" => 2,
            "Low" => 1,
            _ => 0,
        }
    }
    rank(a) >= rank(b)
}
