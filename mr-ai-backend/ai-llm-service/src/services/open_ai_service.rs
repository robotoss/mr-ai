//! OpenAI (ChatGPT) service for text generation and embeddings.
//!
//! This module provides a minimal, synchronous (non-streaming) client for the
//! OpenAI REST API using a shared [`LlmModelConfig`] configuration.
//!
//! Supported endpoints (built from `cfg.endpoint`):
//! - `POST {endpoint}/v1/chat/completions` — chat completion (non-streaming)
//! - `POST {endpoint}/v1/embeddings`       — embeddings retrieval
//!
//! Validation performed by the constructor:
//! - `cfg.provider` must be [`LlmProvider::OpenAI`]
//! - `cfg.api_key` must be present
//! - `cfg.endpoint` must start with `http://` or `https://`
//!
//! Errors are normalized via the unified error types in `error_handler`.

use std::time::Duration;

use reqwest::header;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::{
    config::{llm_model_config::LlmModelConfig, llm_provider::LlmProvider},
    error_handler::{
        AiLlmError, HttpError, Provider, ProviderError, ProviderErrorKind, make_snippet,
    },
};

/// Thin client for the OpenAI API (ChatGPT).
///
/// Constructed from a complete [`LlmModelConfig`]. Internally keeps a
/// preconfigured `reqwest::Client` (with timeout and default headers).
///
/// High-level operations:
/// - [`OpenAiService::generate`]   — single, non-streaming chat completion
/// - [`OpenAiService::embeddings`] — single embeddings vector retrieval
pub struct OpenAiService {
    client: reqwest::Client,
    cfg: LlmModelConfig,
    url_chat: String,
    url_embeddings: String,
}

impl OpenAiService {
    /// Creates a new [`OpenAiService`] from the given config.
    ///
    /// Validates the provider, API key, and endpoint scheme. Builds an HTTP
    /// client with default headers and a configurable timeout.
    ///
    /// # Errors
    /// - [`AiLlmError::Provider`] with `InvalidProvider` if `cfg.provider` is not OpenAI
    /// - [`AiLlmError::Provider`] with `MissingApiKey` if `cfg.api_key` is `None`
    /// - [`AiLlmError::Provider`] with `InvalidEndpoint` if `cfg.endpoint` is invalid
    /// - [`AiLlmError::HttpTransport`] if the HTTP client cannot be built
    pub fn new(cfg: LlmModelConfig) -> Result<Self, AiLlmError> {
        // 1) Provider must be OpenAI.
        if cfg.provider != LlmProvider::OpenAI {
            return Err(
                ProviderError::new(Provider::OpenAI, ProviderErrorKind::InvalidProvider).into(),
            );
        }

        // 2) API key must be present.
        let api_key = cfg.api_key.clone().ok_or_else(|| {
            ProviderError::new(Provider::OpenAI, ProviderErrorKind::MissingApiKey)
        })?;

        // 3) Endpoint must use http/https (allows custom gateways if needed).
        let endpoint = cfg.endpoint.trim();
        if endpoint.is_empty()
            || !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            return Err(ProviderError::new(
                Provider::OpenAI,
                ProviderErrorKind::InvalidEndpoint(cfg.endpoint.clone()),
            )
            .into());
        }

        // 4) HTTP client: timeout + default headers.
        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(60));

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {}", api_key)).map_err(|e| {
                ProviderError::new(
                    Provider::OpenAI,
                    ProviderErrorKind::Decode(format!("invalid API key header: {e}")),
                )
            })?,
        );
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );

        // Modern reqwest enables compression via crate features; no explicit .gzip/.brotli calls.
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .default_headers(headers)
            .build()?;

        let base = endpoint.trim_end_matches('/').to_string();
        let url_chat = format!("{}/v1/chat/completions", base);
        let url_embeddings = format!("{}/v1/embeddings", base);

        Ok(Self {
            client,
            cfg,
            url_chat,
            url_embeddings,
        })
    }

    /// Performs a **non-streaming** chat completion request (`/v1/chat/completions`).
    ///
    /// The request composes a minimal `messages` array:
    /// - optional system message (if `system` is provided);
    /// - user message with `prompt`.
    ///
    /// Mapped options from config:
    /// - `model`       ← `self.cfg.model`
    /// - `temperature` ← `self.cfg.temperature`
    /// - `top_p`       ← `self.cfg.top_p`
    /// - `max_tokens`  ← `self.cfg.max_tokens`
    ///
    /// # Parameters
    /// - `prompt`: user content
    /// - `system`: optional system instruction
    ///
    /// # Errors
    /// - [`AiLlmError::Provider`] with `HttpStatus` for non-2xx responses
    /// - [`AiLlmError::HttpTransport`] for client/network failures
    /// - [`AiLlmError::Provider`] with `Decode` if the JSON cannot be parsed
    /// - [`AiLlmError::Provider`] with `EmptyChoices` if no choices are returned
    #[instrument(skip_all, fields(model = %self.cfg.model))]
    pub async fn generate(&self, prompt: &str, system: Option<&str>) -> Result<String, AiLlmError> {
        let body = ChatCompletionRequest::from_cfg(&self.cfg, prompt, system);

        debug!("POST {}", self.url_chat);
        let resp = self.client.post(&self.url_chat).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let url = self.url_chat.clone();
            let text = resp.text().await.unwrap_or_default();
            let snippet = make_snippet(&text);
            return Err(ProviderError::new(
                Provider::OpenAI,
                ProviderErrorKind::HttpStatus(HttpError {
                    status,
                    url,
                    snippet,
                }),
            )
            .into());
        }

        let out: ChatCompletionResponse = resp.json().await.map_err(|e| {
            ProviderError::new(
                Provider::OpenAI,
                ProviderErrorKind::Decode(format!(
                    "serde error: {e}; expected `choices[0].message.content`"
                )),
            )
        })?;

        let content = out
            .choices
            .into_iter()
            .find_map(|c| c.message.content)
            .ok_or_else(|| ProviderError::new(Provider::OpenAI, ProviderErrorKind::EmptyChoices))?;

        Ok(content)
    }

    /// Retrieves a single embeddings vector via `/v1/embeddings`.
    ///
    /// By default uses `self.cfg.model`. If you need a different embeddings
    /// model (e.g., `text-embedding-3-small`), configure it in `LlmModelConfig`.
    ///
    /// # Parameters
    /// - `input`: raw text to embed
    ///
    /// # Errors
    /// - [`AiLlmError::Provider`] with `HttpStatus` for non-2xx responses
    /// - [`AiLlmError::HttpTransport`] for client/network failures
    /// - [`AiLlmError::Provider`] with `Decode` if the JSON cannot be parsed
    #[instrument(skip_all, fields(model = %self.cfg.model))]
    pub async fn embeddings(&self, input: &str) -> Result<Vec<f32>, AiLlmError> {
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
            let snippet = make_snippet(&text);
            return Err(ProviderError::new(
                Provider::OpenAI,
                ProviderErrorKind::HttpStatus(HttpError {
                    status,
                    url,
                    snippet,
                }),
            )
            .into());
        }

        let out: EmbeddingsResponse = resp.json().await.map_err(|e| {
            ProviderError::new(
                Provider::OpenAI,
                ProviderErrorKind::Decode(format!(
                    "serde error: {e}; expected `data[0].embedding`"
                )),
            )
        })?;

        let first = out.data.into_iter().next().ok_or_else(|| {
            ProviderError::new(
                Provider::OpenAI,
                ProviderErrorKind::Decode("empty `data` in embeddings response".into()),
            )
        })?;

        Ok(first.embedding)
    }
}

/* ===========================================================================
HTTP payloads & options
======================================================================== */

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
    /// Builds a minimal chat request from config, user prompt, and an optional system message.
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
    /// One of: "system" | "user" | "assistant" | "tool" | ...
    role: &'a str,
    /// Plain string content. For advanced payloads OpenAI also accepts arrays of parts.
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
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
