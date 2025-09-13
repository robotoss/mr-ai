//! Lightweight Ollama service for text generation and embeddings.
//!
//! This module provides a minimal, non-streaming client for a local or remote
//! Ollama instance. Endpoints are derived from `LlmModelConfig::endpoint`:
//! - `POST {endpoint}/api/generate`   — text generation (with `stream=false`)
//! - `POST {endpoint}/api/embeddings` — embeddings retrieval
//!
//! Validation performed by the constructor:
//! - `cfg.provider` must be [`LlmProvider::Ollama`]
//! - `cfg.endpoint` must start with `http://` or `https://`
//!
//! Errors are normalized via the unified error types from your `error_handler`
//! (e.g., `ProviderErrorKind::{InvalidProvider, InvalidEndpoint, HttpStatus, Decode}`).

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use crate::{
    config::{llm_model_config::LlmModelConfig, llm_provider::LlmProvider},
    error_handler::{
        AiLlmError, HttpError, Provider, ProviderError, ProviderErrorKind, make_snippet,
    },
};

/// Thin client for the Ollama API.
///
/// Constructed from a complete [`LlmModelConfig`]. Internally keeps a
/// preconfigured `reqwest::Client` (with timeout). Provides two high-level calls:
/// - [`OllamaService::generate`]   — single, non-streaming text generation
/// - [`OllamaService::embeddings`] — single embeddings vector retrieval
#[derive(Debug)]
pub struct OllamaService {
    client: reqwest::Client,
    cfg: LlmModelConfig,
    url_generate: String,
    url_embeddings: String,
}

impl OllamaService {
    /// Creates a new [`OllamaService`] from the given config.
    ///
    /// Validates the provider and endpoint scheme, then builds an HTTP client
    /// with a configurable timeout.
    ///
    /// # Errors
    /// - [`AiLlmError::Provider`] with `InvalidProvider` if `cfg.provider` is not Ollama
    /// - [`AiLlmError::Provider`] with `InvalidEndpoint` if `cfg.endpoint` is invalid
    /// - [`AiLlmError::HttpTransport`] if the HTTP client cannot be built
    pub fn new(cfg: LlmModelConfig) -> Result<Self, AiLlmError> {
        // 1) Provider must be Ollama.
        if cfg.provider != LlmProvider::Ollama {
            return Err(
                ProviderError::new(Provider::Ollama, ProviderErrorKind::InvalidProvider).into(),
            );
        }

        // 2) Endpoint must use http/https (local and remote hosts supported).
        let endpoint = cfg.endpoint.trim();
        if endpoint.is_empty()
            || !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            return Err(ProviderError::new(
                Provider::Ollama,
                ProviderErrorKind::InvalidEndpoint(cfg.endpoint.clone()),
            )
            .into());
        }

        // 3) HTTP client: timeout only; compression is enabled via crate features.
        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(60));

        let client = reqwest::Client::builder().timeout(timeout).build()?;

        let base = endpoint.trim_end_matches('/').to_string();
        let url_generate = format!("{}/api/generate", base);
        let url_embeddings = format!("{}/api/embeddings", base);

        info!(
            provider = ?cfg.provider,
            model = %cfg.model,
            endpoint = %cfg.endpoint,
            timeout_secs = cfg.timeout_secs.unwrap_or(60),
            "OllamaService initialized"
        );

        Ok(Self {
            client,
            cfg,
            url_generate,
            url_embeddings,
        })
    }

    /// Performs a **non-streaming** generation request via `/api/generate`.
    ///
    /// The request maps config to Ollama fields as follows:
    /// - `model`        ← `self.cfg.model`
    /// - `prompt`       ← `prompt` argument
    /// - `options`:
    ///   - `num_predict`  ← `self.cfg.max_tokens`
    ///   - `temperature`  ← `self.cfg.temperature`
    ///   - `top_p`        ← `self.cfg.top_p`
    /// - `stream` is forced to `false` (this method is synchronous)
    ///
    /// # Parameters
    /// - `prompt`: user content to generate from
    ///
    /// # Errors
    /// - [`AiLlmError::Provider`] with `HttpStatus` for non-2xx responses
    /// - [`AiLlmError::HttpTransport`] for client/network failures
    /// - [`AiLlmError::Provider`] with `Decode` if the JSON cannot be parsed
    pub async fn generate(&self, prompt: &str) -> Result<String, AiLlmError> {
        let started = Instant::now();
        let body = GenerateRequest::from_cfg(&self.cfg, prompt);

        debug!(
            model = %self.cfg.model,
            endpoint = %self.cfg.endpoint,
            prompt_len = prompt.len(),
            "POST {}", self.url_generate
        );

        let resp = self
            .client
            .post(&self.url_generate)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let url = self.url_generate.clone();
            let text = resp.text().await.unwrap_or_default();
            let snippet = make_snippet(&text);

            error!(
                %status,
                %url,
                %snippet,
                model = %self.cfg.model,
                endpoint = %self.cfg.endpoint,
                latency_ms = started.elapsed().as_millis(),
                "Ollama /api/generate returned non-success status"
            );

            return Err(ProviderError::new(
                Provider::Ollama,
                ProviderErrorKind::HttpStatus(HttpError {
                    status,
                    url,
                    snippet,
                }),
            )
            .into());
        }

        let out: GenerateResponse = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                error!(
                    error = %e,
                    model = %self.cfg.model,
                    endpoint = %self.cfg.endpoint,
                    latency_ms = started.elapsed().as_millis(),
                    "failed to decode /api/generate response"
                );
                return Err(ProviderError::new(
                    Provider::Ollama,
                    ProviderErrorKind::Decode(format!(
                        "serde error: {e}; ensure `stream=false` is used"
                    )),
                )
                .into());
            }
        };

        info!(
            model = %self.cfg.model,
            endpoint = %self.cfg.endpoint,
            latency_ms = started.elapsed().as_millis(),
            "generation completed"
        );
        Ok(out.response)
    }

    /// Retrieves a single embeddings vector via `/api/embeddings`.
    ///
    /// By default uses `self.cfg.model`. If you need a dedicated embeddings model,
    /// configure it in `LlmModelConfig` for the service instance that performs
    /// embeddings.
    ///
    /// # Parameters
    /// - `input`: raw text to embed
    ///
    /// # Errors
    /// - [`AiLlmError::Provider`] with `HttpStatus` for non-2xx responses
    /// - [`AiLlmError::HttpTransport`] for client/network failures
    /// - [`AiLlmError::Provider`] with `Decode` if the JSON cannot be parsed
    pub async fn embeddings(&self, prompt: &str) -> Result<Vec<f32>, AiLlmError> {
        let started = Instant::now();
        let body = EmbeddingsRequest {
            model: &self.cfg.model,
            prompt,
        };

        debug!(
            model = %self.cfg.model,
            endpoint = %self.cfg.endpoint,
            input_len = prompt.len(),
            "POST {}", self.url_embeddings
        );

        let resp = self
            .client
            .post(&self.url_embeddings)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let url = self.url_embeddings.clone();
            let text = resp.text().await.unwrap_or_default();
            let snippet = make_snippet(&text);

            error!(
                %status,
                %url,
                %snippet,
                model = %self.cfg.model,
                endpoint = %self.cfg.endpoint,
                latency_ms = started.elapsed().as_millis(),
                "Ollama /api/embeddings returned non-success status"
            );

            return Err(ProviderError::new(
                Provider::Ollama,
                ProviderErrorKind::HttpStatus(HttpError {
                    status,
                    url,
                    snippet,
                }),
            )
            .into());
        }

        let out: EmbeddingsResponse = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                error!(
                    error = %e,
                    model = %self.cfg.model,
                    endpoint = %self.cfg.endpoint,
                    latency_ms = started.elapsed().as_millis(),
                    "failed to decode /api/embeddings response"
                );
                return Err(ProviderError::new(
                    Provider::Ollama,
                    ProviderErrorKind::Decode(format!(
                        "serde error: {e}; expected `embedding` array"
                    )),
                )
                .into());
            }
        };

        info!(
            model = %self.cfg.model,
            endpoint = %self.cfg.endpoint,
            latency_ms = started.elapsed().as_millis(),
            "embeddings completed"
        );
        Ok(out.embedding)
    }
}

/* ===========================================================================
HTTP payloads & options
======================================================================== */

/// Request body for `/api/generate` (non-streaming).
#[derive(Debug, Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    #[serde(default)]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<GenerateOptions>,
}

impl<'a> GenerateRequest<'a> {
    /// Builds a request from config and prompt (forces `stream=false`).
    fn from_cfg(cfg: &'a LlmModelConfig, prompt: &'a str) -> Self {
        let options = GenerateOptions {
            temperature: cfg.temperature,
            top_p: cfg.top_p,
            num_predict: cfg.max_tokens,
        };

        Self {
            model: &cfg.model,
            prompt,
            stream: false,
            options: Some(options),
        }
    }
}

/// Subset of Ollama `options`.
///
/// Extend this struct as needed (top_k, stop sequences, penalties, etc.).
#[derive(Debug, Default, Serialize)]
struct GenerateOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
}

/// Response body for `/api/generate`.
///
/// Minimal shape: the generated text is in `response`.
#[derive(Debug, Deserialize)]
struct GenerateResponse {
    response: String,
}

/// Request body for `/api/embeddings`.
#[derive(Debug, Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

/// Response body for `/api/embeddings`.
///
/// Ollama commonly returns `{ "embedding": [f32; N] }`.
#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    #[serde(alias = "embedding")]
    embedding: Vec<f32>,
}
