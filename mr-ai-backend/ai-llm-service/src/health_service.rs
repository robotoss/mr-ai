//! Universal health service for LLM backends (Ollama, OpenAI).
//!
//! This module exposes lightweight health checks for supported providers:
//! - Ollama: `GET {endpoint}/api/tags` (best-effort model existence check)
//! - OpenAI: `GET {endpoint}/v1/models` with Bearer auth (best-effort model existence check)
//!
//! The returned [`HealthStatus`] is JSON-serializable and suitable for a `/health` endpoint.
//! [`HealthService::check`] is resilient and never fails (errors mapped to `ok=false`).
//! Provider-specific probes (`try_*`) return strict `Result`.

use std::time::{Duration, Instant};

use reqwest::header;
use serde::Serialize;
use tracing::{debug, error, info, warn};

use crate::config::llm_model_config::LlmModelConfig;
use crate::config::llm_provider::LlmProvider;
use crate::error_handler::{AiLlmError, HealthError, HttpError, make_snippet};

/// A serializable health snapshot for a single provider/config.
#[derive(Debug, Clone, Serialize)]
pub struct HealthStatus {
    /// Backend/provider (e.g., "Ollama", "OpenAI").
    pub provider: String,
    /// Target endpoint base URL.
    pub endpoint: String,
    /// Optional model identifier relevant to the probe (if any).
    pub model: Option<String>,
    /// Overall health flag.
    pub ok: bool,
    /// Measured HTTP latency in milliseconds for the main probe.
    pub latency_ms: u128,
    /// Short human-readable message with details.
    pub message: String,
}

impl HealthStatus {
    #[inline]
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
            model: model.map(str::to_string),
            ok: true,
            latency_ms,
            message: message.into(),
        }
    }

    #[inline]
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
            model: model.map(str::to_string),
            ok: false,
            latency_ms,
            message: message.into(),
        }
    }
}

/// A universal health checker that reuses a single HTTP client.
///
/// The client is constructed with a default timeout. Individual probes may
/// override the timeout per request based on the provided config.
pub struct HealthService {
    client: reqwest::Client,
    default_timeout: Duration,
}

impl HealthService {
    /// Creates a new health service with an optional client timeout (seconds).
    ///
    /// The internal client is reused across all probes.
    ///
    /// # Errors
    /// Returns [`AiLlmError::HttpTransport`] if the HTTP client cannot be built.
    pub fn new(timeout_secs: Option<u64>) -> Result<Self, AiLlmError> {
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(10));
        let client = reqwest::Client::builder().timeout(timeout).build()?;

        info!(
            default_timeout_secs = timeout.as_secs(),
            "HealthService initialized"
        );

        Ok(Self {
            client,
            default_timeout: timeout,
        })
    }

    /// Checks health for a single LLM config, routing to the provider-specific probe.
    ///
    /// This method is **resilient**: it never returns an error. Any failure is converted
    /// to `HealthStatus { ok: false, message: ... }`, which is convenient for `/health`.
    pub async fn check(&self, cfg: &LlmModelConfig) -> HealthStatus {
        // Quick endpoint validation to avoid obvious issues.
        let endpoint = cfg.endpoint.trim();
        if endpoint.is_empty()
            || !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            warn!(
                provider = ?cfg.provider,
                endpoint = %cfg.endpoint,
                "invalid endpoint (empty or missing http/https)"
            );
            return HealthStatus::fail(
                cfg.provider,
                endpoint,
                Some(&cfg.model),
                0,
                "endpoint is empty or missing http/https",
            );
        }

        let start = Instant::now();
        let result = match cfg.provider {
            LlmProvider::Ollama => self.try_probe_ollama(cfg).await,
            LlmProvider::OpenAI => self.try_probe_openai(cfg).await,
        };

        match result {
            Ok(mut status) => {
                if status.latency_ms == 0 {
                    status.latency_ms = start.elapsed().as_millis();
                }
                info!(
                    provider = %status.provider,
                    endpoint = %status.endpoint,
                    model = %status.model.as_deref().unwrap_or("n/a"),
                    ok = status.ok,
                    latency_ms = status.latency_ms,
                    "health probe completed"
                );
                status
            }
            Err(err) => {
                let status =
                    Self::status_from_error(cfg, start.elapsed().as_millis(), err.to_string());
                warn!(
                    provider = %status.provider,
                    endpoint = %status.endpoint,
                    model = %status.model.as_deref().unwrap_or("n/a"),
                    latency_ms = status.latency_ms,
                    message = %status.message,
                    "health probe failed"
                );
                status
            }
        }
    }

    /// Checks health for multiple configs and returns a vector of statuses.
    ///
    /// This function never returns an error: each failing check is converted into
    /// a `HealthStatus` with `ok = false`.
    pub async fn check_many(&self, configs: &[LlmModelConfig]) -> Vec<HealthStatus> {
        debug!(count = configs.len(), "running batch health probes");
        let mut out = Vec::with_capacity(configs.len());
        for cfg in configs {
            out.push(self.check(cfg).await);
        }
        out
    }

    /// Strict Ollama probe. Returns an error on hard failures.
    ///
    /// Probe:
    /// - `GET {endpoint}/api/tags`
    /// - Ensure 2xx
    /// - Best-effort: verify `cfg.model` exists in the returned tags
    async fn try_probe_ollama(&self, cfg: &LlmModelConfig) -> Result<HealthStatus, AiLlmError> {
        let url = format!("{}/api/tags", cfg.endpoint.trim_end_matches('/'));
        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        let start = Instant::now();
        debug!(
            provider = "Ollama",
            endpoint = %cfg.endpoint,
            model = %cfg.model,
            "GET {}", url
        );

        let resp = self
            .client
            .get(&url)
            .timeout(timeout)
            .send()
            .await
            .map_err(AiLlmError::from)?;

        let latency = start.elapsed().as_millis();

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let snippet = make_snippet(&text);

            error!(
                provider = "Ollama",
                %url,
                %status,
                %snippet,
                latency_ms = latency,
                "health GET /api/tags returned non-success status"
            );

            return Err(AiLlmError::from(HealthError::HttpStatus(HttpError {
                status,
                url,
                snippet,
            })));
        }

        // Expected minimal JSON: { "models": [ { "name": "<model>" }, ... ] }
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
                            "Ollama is up, but model not found in /api/tags",
                        ))
                    }
                } else {
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
                warn!(
                    provider = "Ollama",
                    endpoint = %cfg.endpoint,
                    model = %cfg.model,
                    error = %e,
                    latency_ms = latency,
                    "failed to decode /api/tags; treating server as reachable"
                );
                Ok(HealthStatus::ok(
                    cfg.provider,
                    &cfg.endpoint,
                    Some(&cfg.model),
                    latency,
                    format!("Ollama is reachable; failed to decode /api/tags: {e}"),
                ))
            }
        }
    }

    /// Strict OpenAI probe. Returns an error on hard failures.
    ///
    /// Probe:
    /// - `GET {endpoint}/v1/models` with `Authorization: Bearer <api_key>`
    /// - Ensure 2xx
    /// - Best-effort: verify `cfg.model` exists in the returned list
    async fn try_probe_openai(&self, cfg: &LlmModelConfig) -> Result<HealthStatus, AiLlmError> {
        let base = cfg.endpoint.trim_end_matches('/').to_string();
        let url = format!("{}/v1/models", base);
        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        let api_key = cfg.api_key.as_ref().ok_or_else(|| {
            AiLlmError::Health(HealthError::Decode("missing OpenAI API key".into()))
        })?;

        let auth_header =
            header::HeaderValue::from_str(&format!("Bearer {}", api_key)).map_err(|e| {
                AiLlmError::Health(HealthError::Decode(format!("invalid API key header: {e}")))
            })?;

        let start = Instant::now();
        debug!(
            provider = "OpenAI",
            endpoint = %cfg.endpoint,
            model = %cfg.model,
            "GET {}", url
        );

        let resp = self
            .client
            .get(&url)
            .timeout(timeout)
            .header(header::AUTHORIZATION, auth_header)
            .send()
            .await
            .map_err(AiLlmError::from)?;

        let latency = start.elapsed().as_millis();

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let snippet = make_snippet(&text);

            error!(
                provider = "OpenAI",
                %url,
                %status,
                %snippet,
                latency_ms = latency,
                "health GET /v1/models returned non-success status"
            );

            return Err(AiLlmError::Health(HealthError::HttpStatus(HttpError {
                status,
                url,
                snippet,
            })));
        }

        // Expected minimal JSON: { "data": [ { "id": "<model>" }, ... ] }
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
                        "OpenAI is up, but model not found in /v1/models",
                    ))
                }
            }
            Err(e) => {
                warn!(
                    provider = "OpenAI",
                    endpoint = %cfg.endpoint,
                    model = %cfg.model,
                    error = %e,
                    latency_ms = latency,
                    "failed to decode /v1/models; treating server as reachable"
                );
                Ok(HealthStatus::ok(
                    cfg.provider,
                    &cfg.endpoint,
                    Some(&cfg.model),
                    latency,
                    format!("OpenAI is reachable; failed to decode /v1/models: {e}"),
                ))
            }
        }
    }

    /// Converts an error into a failure `HealthStatus` with a concise message.
    #[inline]
    fn status_from_error(cfg: &LlmModelConfig, latency_ms: u128, msg: String) -> HealthStatus {
        HealthStatus::fail(
            cfg.provider,
            &cfg.endpoint,
            Some(&cfg.model),
            latency_ms,
            msg,
        )
    }
}
