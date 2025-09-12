//! Lightweight Ollama service for text generation and embeddings.
//!
//! This module implements a thin client for the local Ollama API:
//! - `POST {endpoint}/api/generate`   — synchronous text generation (`stream=false`)
//! - `POST {endpoint}/api/embeddings` — embeddings retrieval
//!
//! It uses the universal configuration [`LlmModelConfig`] and ensures
//! that the selected provider is [`LlmProvider::Ollama`].
//!
//! # Examples
//!
//! ```no_run
//! use ai_llm_service::config::llm_provider::LlmProvider;
//! use ai_llm_service::llm::LlmModelConfig;
//! use ai_llm_service::services::ollama_service::OllamaService;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cfg = LlmModelConfig {
//!     provider: LlmProvider::Ollama,
//!     model: "qwen3:14b".into(),
//!     endpoint: "http://localhost:11434".into(),
//!     api_key: None,
//!     max_tokens: Some(256),
//!     temperature: Some(0.7),
//!     top_p: Some(0.9),
//!     timeout_secs: Some(30),
//! };
//!
//! let svc = OllamaService::new(cfg)?;
//!
//! // Text generation
//! let text = svc.generate("Write a haiku about Rust.").await?;
//! println!("Generated:\n{}", text);
//!
//! // Embeddings (usually you want to use a dedicated embedding model)
//! let vec = svc.embeddings("Ferris is a friendly crab.").await?;
//! println!("Embeddings dimension = {}", vec.len());
//! # Ok(()) }
//! ```

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, instrument};

use crate::config::llm_provider::LlmProvider;
use crate::llm::LlmModelConfig;

/// Errors produced by [`OllamaService`].
#[derive(Debug, Error)]
pub enum OllamaError {
    /// The provider in the config is not Ollama.
    #[error("[AI LLM Service] invalid provider: expected Ollama, got different provider")]
    InvalidProvider,

    /// Invalid endpoint (empty or missing http/https).
    #[error("[AI LLM Service] invalid Ollama endpoint: {0}")]
    InvalidEndpoint(String),

    /// Transport/HTTP client error.
    #[error("[AI LLM Service] transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Non-successful HTTP status from upstream.
    #[error("[AI LLM Service] unexpected HTTP status {status} from {url}: {snippet}")]
    HttpStatus {
        /// Numeric HTTP status code.
        status: StatusCode,
        /// Request URL.
        url: String,
        /// Optional short snippet of the response body.
        snippet: String,
    },

    /// Unexpected/invalid JSON response.
    #[error("[AI LLM Service] failed to decode response: {0}")]
    Decode(String),
}

/// Result alias for Ollama operations.
pub type Result<T> = std::result::Result<T, OllamaError>;

/// Thin client for Ollama.
///
/// Initialized with a full [`LlmModelConfig`]. Reuses an HTTP client with
/// a configurable timeout. Provides high-level calls:
/// - [`OllamaService::generate`]   — synchronous text generation
/// - [`OllamaService::embeddings`] — embeddings retrieval
pub struct OllamaService {
    client: reqwest::Client,
    cfg: LlmModelConfig,
    url_generate: String,
    url_embeddings: String,
}

impl OllamaService {
    /// Creates a new [`OllamaService`] from the given config.
    ///
    /// # Errors
    /// - [`OllamaError::InvalidProvider`] if `cfg.provider` is not `Ollama`
    /// - [`OllamaError::InvalidEndpoint`] if `cfg.endpoint` is invalid
    /// - [`OllamaError::Transport`] if HTTP client cannot be built
    pub fn new(cfg: LlmModelConfig) -> Result<Self> {
        if cfg.provider != LlmProvider::Ollama {
            return Err(OllamaError::InvalidProvider);
        }

        let endpoint = cfg.endpoint.trim();
        if endpoint.is_empty()
            || !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            return Err(OllamaError::InvalidEndpoint(cfg.endpoint));
        }

        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(60));

        let client = reqwest::Client::builder()
            .timeout(timeout)
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .build()?;

        let base = endpoint.trim_end_matches('/').to_string();
        let url_generate = format!("{}/api/generate", base);
        let url_embeddings = format!("{}/api/embeddings", base);

        Ok(Self {
            client,
            cfg,
            url_generate,
            url_embeddings,
        })
    }

    /// Performs a **non-streaming** generation request via `/api/generate`.
    ///
    /// Mapped options:
    /// - `model`        ← `self.cfg.model`
    /// - `prompt`       ← argument
    /// - `num_predict`  ← `self.cfg.max_tokens`
    /// - `temperature`  ← `self.cfg.temperature`
    /// - `top_p`        ← `self.cfg.top_p`
    ///
    /// # Errors
    /// - [`OllamaError::HttpStatus`] for non-2xx responses
    /// - [`OllamaError::Transport`] for client errors
    /// - [`OllamaError::Decode`] if response cannot be parsed
    #[instrument(skip_all, fields(model = %self.cfg.model))]
    pub async fn generate(&self, prompt: &str) -> Result<String> {
        let body = GenerateRequest::from_cfg(&self.cfg, prompt);

        debug!("POST {}", self.url_generate);
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
            let snippet = text.chars().take(240).collect::<String>();
            return Err(OllamaError::HttpStatus {
                status,
                url,
                snippet,
            });
        }

        let out: GenerateResponse = resp.json().await.map_err(|e| {
            OllamaError::Decode(format!("serde error: {e}; ensure `stream=false` is used"))
        })?;

        Ok(out.response)
    }

    /// Retrieves embeddings via `/api/embeddings`.
    ///
    /// **Note:** Usually a dedicated embedding model is used. If you want to
    /// use a different one, create another [`OllamaService`] with the desired
    /// config.
    ///
    /// # Errors
    /// - [`OllamaError::HttpStatus`] for non-2xx responses
    /// - [`OllamaError::Transport`] for client errors
    /// - [`OllamaError::Decode`] if response cannot be parsed
    #[instrument(skip_all, fields(model = %self.cfg.model))]
    pub async fn embeddings(&self, input: &str) -> Result<Vec<f32>> {
        let body = EmbeddingsRequest {
            model: &self.cfg.model,
            input,
        };

        debug!("POST {}", self.url_embeddings);
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
            let snippet = text.chars().take(240).collect::<String>();
            return Err(OllamaError::HttpStatus {
                status,
                url,
                snippet,
            });
        }

        let out: EmbeddingsResponse = resp.json().await.map_err(|e| {
            OllamaError::Decode(format!(
                "serde error: {e}; expected `{ embedding: number[] }`"
            ))
        })?;

        Ok(out.embedding)
    }
}

/* ==========================
HTTP payloads & options
========================== */

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
    /// Builds a request from config and prompt.
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
    input: &'a str,
}

/// Response body for `/api/embeddings`.
#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    #[serde(alias = "embedding")]
    embedding: Vec<f32>,
}
