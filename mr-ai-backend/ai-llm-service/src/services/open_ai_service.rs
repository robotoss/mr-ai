//! OpenAI (ChatGPT) service for text generation and embeddings.
//!
//! This module implements a thin client for the OpenAI API using
//! the universal [`LlmModelConfig`].
//!
//! Supported operations:
//! - `POST {endpoint}/v1/chat/completions` — synchronous chat completion (non-streaming)
//! - `POST {endpoint}/v1/embeddings`       — embeddings retrieval
//!
//! It validates that `cfg.provider == LlmProvider::ChatGpt` and that an API key
//! is provided. The `endpoint` must start with `https://` or `http://` to allow
//! custom/self-hosted gateways (use with caution).

use std::time::Duration;

use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, instrument};

use crate::config::llm_provider::LlmProvider;
use crate::llm::LlmModelConfig;

/// Errors produced by [`OpenAiService`].
#[derive(Debug, Error)]
pub enum OpenAiError {
    /// The provider in the config is not ChatGpt.
    #[error("[AI LLM Service] invalid provider: expected ChatGpt, got different provider")]
    InvalidProvider,

    /// API key is missing in the config.
    #[error("[AI LLM Service] missing OpenAI API key in LlmModelConfig::api_key")]
    MissingApiKey,

    /// Invalid endpoint (empty or missing http/https).
    #[error("[AI LLM Service] invalid OpenAI endpoint: {0}")]
    InvalidEndpoint(String),

    /// Transport/HTTP client error.
    #[error("[AI LLM Service] transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Non-successful HTTP status from upstream.
    #[error("[AI LLM Service] unexpected HTTP status {status} from {url}: {snippet}")]
    HttpStatus {
        status: StatusCode,
        url: String,
        snippet: String,
    },

    /// Unexpected/invalid JSON response.
    #[error("[AI LLM Service] failed to decode response: {0}")]
    Decode(String),

    /// The completion returned no choices.
    #[error("[AI LLM Service] empty response: no choices returned")]
    EmptyChoices,
}

/// Result alias for OpenAI operations.
pub type Result<T> = std::result::Result<T, OpenAiError>;

/// Thin client for the OpenAI API (ChatGPT).
///
/// Initialized with a full [`LlmModelConfig`]. Reuses an HTTP client with
/// a configurable timeout. Provides high-level calls:
/// - [`OpenAiService::generate`]   — synchronous chat completion
/// - [`OpenAiService::embeddings`] — embeddings retrieval
pub struct OpenAiService {
    client: reqwest::Client,
    cfg: LlmModelConfig,
    url_chat: String,
    url_embeddings: String,
    api_key: String,
}

impl OpenAiService {
    /// Creates a new [`OpenAiService`] from the given config.
    ///
    /// # Errors
    /// - [`OpenAiError::InvalidProvider`] if `cfg.provider` is not `ChatGpt`
    /// - [`OpenAiError::MissingApiKey`] if `cfg.api_key` is `None`
    /// - [`OpenAiError::InvalidEndpoint`] if `cfg.endpoint` is invalid
    /// - [`OpenAiError::Transport`] if the HTTP client cannot be built
    pub fn new(cfg: LlmModelConfig) -> Result<Self> {
        if cfg.provider != LlmProvider::ChatGpt {
            return Err(OpenAiError::InvalidProvider);
        }
        let api_key = cfg.api_key.clone().ok_or(OpenAiError::MissingApiKey)?;

        let endpoint = cfg.endpoint.trim();
        if endpoint.is_empty()
            || !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            return Err(OpenAiError::InvalidEndpoint(cfg.endpoint));
        }

        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(60));

        // Prepare default headers (Authorization, Content-Type).
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", api_key))
                .map_err(|e| OpenAiError::Decode(format!("invalid API key header: {e}")))?,
        );
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );

        let client = reqwest::Client::builder()
            .timeout(timeout)
            .default_headers(headers)
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .build()?;

        let base = endpoint.trim_end_matches('/').to_string();
        let url_chat = format!("{}/v1/chat/completions", base);
        let url_embeddings = format!("{}/v1/embeddings", base);

        Ok(Self {
            client,
            cfg,
            url_chat,
            url_embeddings,
            api_key,
        })
    }

    /// Performs a **non-streaming** chat completion via `/v1/chat/completions`.
    ///
    /// This call sends a minimal message array:
    /// - System role (optional, if provided via `system` argument)
    /// - User role (the `prompt` argument)
    ///
    /// Mapped options:
    /// - `model`       ← `self.cfg.model`
    /// - `temperature` ← `self.cfg.temperature`
    /// - `top_p`       ← `self.cfg.top_p`
    /// - `max_tokens`  ← `self.cfg.max_tokens`
    ///
    /// # Parameters
    /// - `prompt`: user content
    /// - `system`: optional system instruction (if `None`, omitted)
    ///
    /// # Errors
    /// - [`OpenAiError::HttpStatus`] for non-2xx responses
    /// - [`OpenAiError::Transport`] for client errors
    /// - [`OpenAiError::Decode`] if response cannot be parsed
    /// - [`OpenAiError::EmptyChoices`] if no choices are returned
    #[instrument(skip_all, fields(model = %self.cfg.model))]
    pub async fn generate(&self, prompt: &str, system: Option<&str>) -> Result<String> {
        let body = ChatCompletionRequest::from_cfg(&self.cfg, prompt, system);

        debug!("POST {}", self.url_chat);
        let resp = self.client.post(&self.url_chat).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let url = self.url_chat.clone();
            let text = resp.text().await.unwrap_or_default();
            let snippet = text.chars().take(240).collect::<String>();
            return Err(OpenAiError::HttpStatus {
                status,
                url,
                snippet,
            });
        }

        let out: ChatCompletionResponse = resp.json().await.map_err(|e| {
            OpenAiError::Decode(format!(
                "serde error: {e}; expected `choices[0].message.content`"
            ))
        })?;

        let content = out
            .choices
            .into_iter()
            .find_map(|c| c.message.content)
            .ok_or(OpenAiError::EmptyChoices)?;

        Ok(content)
    }

    /// Retrieves embeddings via `/v1/embeddings`.
    ///
    /// **Note:** In most setups, embeddings use a dedicated model (e.g., `text-embedding-3-small`).
    /// If you want to use a different one, create another [`OpenAiService`] with the desired
    /// configuration or extend this method to accept a model override.
    ///
    /// # Errors
    /// - [`OpenAiError::HttpStatus`] for non-2xx responses
    /// - [`OpenAiError::Transport`] for client errors
    /// - [`OpenAiError::Decode`] if response cannot be parsed
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
            return Err(OpenAiError::HttpStatus {
                status,
                url,
                snippet,
            });
        }

        let out: EmbeddingsResponse = resp.json().await.map_err(|e| {
            OpenAiError::Decode(format!("serde error: {e}; expected `data[0].embedding`"))
        })?;

        let first = out
            .data
            .into_iter()
            .next()
            .ok_or_else(|| OpenAiError::Decode("empty `data` in embeddings response".into()))?;

        Ok(first.embedding)
    }
}

/* ==========================
HTTP payloads & options
========================== */

/// Minimal request body for `/v1/chat/completions` (non-streaming).
#[derive(Debug, Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

impl<'a> ChatCompletionRequest<'a> {
    /// Builds a request from config, user prompt, and an optional system message.
    fn from_cfg(cfg: &'a LlmModelConfig, prompt: &'a str, system: Option<&'a str>) -> Self {
        let mut messages = Vec::with_capacity(2);
        if let Some(sys) = system {
            messages.push(ChatMessage {
                role: "system",
                content: Some(sys),
            });
        }
        messages.push(ChatMessage {
            role: "user",
            content: Some(prompt),
        });

        Self {
            model: &cfg.model,
            messages,
            temperature: cfg.temperature,
            top_p: cfg.top_p,
            max_tokens: cfg.max_tokens,
        }
    }
}

/// Chat message for the OpenAI API.
#[derive(Debug, Serialize)]
struct ChatMessage<'a> {
    role: &'a str, // "system" | "user" | "assistant" | "tool" ...
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>, // Simple string content; for complex payloads use arrays.
}

/// Minimal response for `/v1/chat/completions`.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessageOut,
}

#[derive(Debug, Deserialize)]
struct ChatMessageOut {
    content: Option<String>,
}

/// Request body for `/v1/embeddings`.
#[derive(Debug, Serialize)]
struct EmbeddingsRequest<'a> {
    model: &'a str,
    input: &'a str,
}

/// Response body for `/v1/embeddings`.
#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
}
