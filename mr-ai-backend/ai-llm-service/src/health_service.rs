//! Universal health service for LLM backends (Ollama, OpenAI).
//!
//! This module provides light-weight health checks for supported providers:
//! - Ollama: `GET {endpoint}/api/tags` (best-effort model existence check)
//! - OpenAI: `GET {endpoint}/v1/models` with Bearer auth (best-effort model existence check)
//!
//! The returned [`HealthStatus`] is serializable to JSON and suitable for
//! exposing a `/health` endpoint in your app. Use [`HealthService::check`]
//! for a resilient one-shot check that never fails (errors are mapped to `ok=false`),
//! or use the provider-specific `try_*` probes for strict `Result` behavior.

use std::time::{Duration, Instant};

use reqwest::{StatusCode, header};
use serde::Serialize;
use tracing::{debug, instrument};

use crate::config::llm_provider::LlmProvider;
use crate::error_handler::{AiLlmError, HealthError, Result};
use crate::llm::LlmModelConfig;

/// A serializable health snapshot for a single provider/config.
#[derive(Debug, Clone, Serialize)]
pub struct HealthStatus {
    /// Backend/provider (e.g., "Ollama", "ChatGpt").
    pub provider: String,
    /// Target endpoint base URL.
    pub endpoint: String,
    /// Optional model identifier if relevant to the check.
    pub model: Option<String>,
    /// Overall health flag.
    pub ok: bool,
    /// Measured HTTP latency in milliseconds for the main probe.
    pub latency_ms: u128,
    /// Short human-readable message with details.
    pub message: String,
}

impl HealthStatus {
    fn ok(
        provider: LlmProvider,
        endpoint: &str,
        model: Option<&str>,
        latency_ms: u128,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider: format!("{provider:?}"),
            endpoint: endpoint.to_string(),
            model: model.map(|s| s.to_string()),
            ok: true,
            latency_ms,
            message: message.into(),
        }
    }

    fn fail(
        provider: LlmProvider,
        endpoint: &str,
        model: Option<&str>,
        latency_ms: u128,
        message: impl Into<String>,
    ) -> Self {
        Self {
            provider: format!("{provider:?}"),
            endpoint: endpoint.to_string(),
            model: model.map(|s| s.to_string()),
            ok: false,
            latency_ms,
            message: message.into(),
        }
    }
}

/// A universal health checker that reuses a single HTTP client.
pub struct HealthService {
    client: reqwest::Client,
    default_timeout: Duration,
}

impl HealthService {
    /// Creates a new health service with an optional client timeout (seconds).
    pub fn new(timeout_secs: Option<u64>) -> Result<Self> {
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(10));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .build()?;
        Ok(Self {
            client,
            default_timeout: timeout,
        })
    }

    /// Checks health for a single LLM config by routing to the appropriate provider-specific probe.
    ///
    /// This method is **resilient**: it never returns an error. Any error is converted
    /// into `HealthStatus { ok: false, message: ... }` for easy consumption by a `/health` route.
    #[instrument(skip_all, fields(provider = ?cfg.provider, endpoint = %cfg.endpoint, model = %cfg.model))]
    pub async fn check(&self, cfg: &LlmModelConfig) -> HealthStatus {
        // Basic endpoint validation up-front to avoid obvious issues.
        let endpoint = cfg.endpoint.trim();
        if endpoint.is_empty()
            || !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            return HealthStatus::fail(
                cfg.provider,
                endpoint,
                Some(&cfg.model),
                0,
                "[AI LLM Service] endpoint is empty or missing http/https",
            );
        }

        // Route to provider-specific strict probes and downcast errors to a failure status.
        let start = Instant::now();
        let result = match cfg.provider {
            LlmProvider::Ollama => self.try_probe_ollama(cfg).await,
            LlmProvider::ChatGpt => self.try_probe_openai(cfg).await,
        };

        match result {
            Ok(mut status) => {
                // Ensure latency_ms is set for success path if not provided by probe.
                if status.latency_ms == 0 {
                    status.latency_ms = start.elapsed().as_millis();
                }
                status
            }
            Err(err) => Self::status_from_error(cfg, start.elapsed().as_millis(), err),
        }
    }

    /// Checks health for multiple configs and returns a vector of statuses.
    ///
    /// This function never returns an error: each failing check is converted into
    /// a `HealthStatus` with `ok=false`.
    pub async fn check_many(&self, configs: &[LlmModelConfig]) -> Vec<HealthStatus> {
        // Sequential by default for simplicity; switch to `join_all` for parallel checks if desired.
        let mut out = Vec::with_capacity(configs.len());
        for cfg in configs {
            out.push(self.check(cfg).await);
        }
        out
    }

    /// Strict probe of Ollama: may return an error on failures.
    ///
    /// Probe:
    /// - `GET {endpoint}/api/tags`
    /// - Verify 2xx
    /// - Best-effort: check that `cfg.model` exists in the returned models list
    async fn try_probe_ollama(&self, cfg: &LlmModelConfig) -> Result<HealthStatus> {
        let url = format!("{}/api/tags", cfg.endpoint.trim_end_matches('/'));
        let start = Instant::now();

        debug!("GET {}", url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(AiLlmError::from)?;

        let latency = start.elapsed().as_millis();

        if !resp.status().is_success() {
            let status = resp.status();
            let snippet = resp.text().await.unwrap_or_default();
            return Err(AiLlmError::from(HealthError::HttpStatus {
                status,
                url,
                snippet: snippet.chars().take(240).collect(),
            }));
        }

        // Minimal expected shape: { "models": [ { "name": "qwen3:14b" }, ... ] }
        #[derive(serde::Deserialize)]
        struct Tag {
            name: String,
        }
        #[derive(serde::Deserialize)]
        struct Tags {
            models: Option<Vec<Tag>>,
        }

        match resp.json::<Tags>().await {
            Ok(tags) => {
                if let Some(models) = tags.models {
                    let exists = models.iter().any(|m| m.name == cfg.model);
                    if exists {
                        Ok(HealthStatus::ok(
                            cfg.provider,
                            &cfg.endpoint,
                            Some(&cfg.model),
                            latency,
                            "Ollama is healthy; model is available",
                        ))
                    } else {
                        Ok(HealthStatus::fail(
                            cfg.provider,
                            &cfg.endpoint,
                            Some(&cfg.model),
                            latency,
                            "[AI LLM Service] Ollama is up, but model not found in /api/tags",
                        ))
                    }
                } else {
                    // If payload doesn't include models list, still consider server up.
                    Ok(HealthStatus::ok(
                        cfg.provider,
                        &cfg.endpoint,
                        Some(&cfg.model),
                        latency,
                        "Ollama is healthy; tags response without `models` field",
                    ))
                }
            }
            Err(e) => {
                // Server reachable but unexpected shape.
                Ok(HealthStatus::ok(
                    cfg.provider,
                    &cfg.endpoint,
                    Some(&cfg.model),
                    latency,
                    format!(
                        "[AI LLM Service] Ollama is reachable; failed to decode /api/tags: {e}"
                    ),
                ))
            }
        }
    }

    /// Strict probe of OpenAI: may return an error on failures.
    ///
    /// Probe:
    /// - `GET {endpoint}/v1/models` with `Authorization: Bearer <api_key>`
    /// - Verify 2xx
    /// - Best-effort: check that `cfg.model` exists in the returned list
    async fn try_probe_openai(&self, cfg: &LlmModelConfig) -> Result<HealthStatus> {
        let base = cfg.endpoint.trim_end_matches('/').to_string();
        let url = format!("{}/v1/models", base);

        let api_key = cfg.api_key.as_ref().ok_or_else(|| {
            AiLlmError::Health(HealthError::Decode(
                "[AI LLM Service] missing OpenAI API key".into(),
            ))
        })?;

        let mut headers = header::HeaderMap::new();
        let auth = header::HeaderValue::from_str(&format!("Bearer {}", api_key)).map_err(|e| {
            AiLlmError::Health(HealthError::Decode(format!(
                "[AI LLM Service] invalid API key header: {e}"
            )))
        })?;
        headers.insert(header::AUTHORIZATION, auth);

        let client = reqwest::Client::builder()
            .timeout(
                cfg.timeout_secs
                    .map(Duration::from_secs)
                    .unwrap_or(self.default_timeout),
            )
            .default_headers(headers)
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .build()
            .map_err(AiLlmError::from)?;

        let start = Instant::now();
        debug!("GET {}", url);
        let resp = client.get(&url).send().await.map_err(AiLlmError::from)?;

        let latency = start.elapsed().as_millis();

        if !resp.status().is_success() {
            let status = resp.status();
            let snippet = resp.text().await.unwrap_or_default();
            return Err(AiLlmError::Health(HealthError::HttpStatus {
                status,
                url,
                snippet: snippet.chars().take(240).collect(),
            }));
        }

        // Minimal expected shape: { "data": [ { "id": "gpt-4o-mini" }, ... ] }
        #[derive(serde::Deserialize)]
        struct ModelItem {
            id: String,
        }
        #[derive(serde::Deserialize)]
        struct Models {
            data: Vec<ModelItem>,
        }

        match resp.json::<Models>().await {
            Ok(models) => {
                let exists = models.data.iter().any(|m| m.id == cfg.model);
                if exists {
                    Ok(HealthStatus::ok(
                        cfg.provider,
                        &cfg.endpoint,
                        Some(&cfg.model),
                        latency,
                        "OpenAI is healthy; model is available",
                    ))
                } else {
                    Ok(HealthStatus::fail(
                        cfg.provider,
                        &cfg.endpoint,
                        Some(&cfg.model),
                        latency,
                        "[AI LLM Service] OpenAI is up, but model not found in /v1/models",
                    ))
                }
            }
            Err(e) => Ok(HealthStatus::ok(
                cfg.provider,
                &cfg.endpoint,
                Some(&cfg.model),
                latency,
                format!("[AI LLM Service] OpenAI is reachable; failed to decode /v1/models: {e}"),
            )),
        }
    }

    /// Converts an error into a failure `HealthStatus` with a concise message.
    fn status_from_error(cfg: &LlmModelConfig, latency_ms: u128, err: AiLlmError) -> HealthStatus {
        // The error Display already includes the "[AI LLM Service]" prefix.
        HealthStatus::fail(
            cfg.provider,
            &cfg.endpoint,
            Some(&cfg.model),
            latency_ms,
            err.to_string(),
        )
    }
}
