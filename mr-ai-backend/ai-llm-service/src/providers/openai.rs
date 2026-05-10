//! OpenAI provider — `/v1/chat/completions` and `/v1/embeddings`.
//!
//! Token usage and cost are taken directly from `usage{}` in the response.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use crate::config::provider_kind::ProviderKind;
use crate::config::ProviderConfig;
use crate::errors::{
    GatewayError, HealthError, HttpError, Provider, ProviderError, ProviderErrorKind, make_snippet,
};
use crate::traits::{EmbeddingProvider, HealthInfo, LlmProvider};
use crate::unified::{
    EmbeddingRequest, EmbeddingResponse, Role, TokenUsage, UnifiedRequest, UnifiedResponse,
};

/// Concrete OpenAI provider client.
#[derive(Debug)]
pub struct OpenAiProvider {
    client: Client,
    cfg: ProviderConfig,
    url_chat: String,
    url_embeddings: String,
    url_models: String,
}

impl OpenAiProvider {
    pub fn new(cfg: ProviderConfig) -> Result<Self, GatewayError> {
        if cfg.provider != ProviderKind::OpenAI {
            return Err(
                ProviderError::new(Provider::OpenAI, ProviderErrorKind::InvalidProvider).into(),
            );
        }
        let api_key = cfg.api_key.clone().ok_or_else(|| {
            ProviderError::new(Provider::OpenAI, ProviderErrorKind::MissingApiKey)
        })?;

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

        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(60));

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|e| {
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

        let client = Client::builder()
            .timeout(timeout)
            .default_headers(headers)
            .build()?;

        let base = endpoint.trim_end_matches('/').to_string();

        info!(
            provider = %ProviderKind::OpenAI,
            model = %cfg.model,
            endpoint = %cfg.endpoint,
            timeout_secs = cfg.timeout_secs.unwrap_or(60),
            "OpenAiProvider initialised"
        );

        Ok(Self {
            client,
            url_chat: format!("{base}/v1/chat/completions"),
            url_embeddings: format!("{base}/v1/embeddings"),
            url_models: format!("{base}/v1/models"),
            cfg,
        })
    }
}

/* --------------------------------------------------------------------- */
/* LlmProvider                                                           */
/* --------------------------------------------------------------------- */

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, req: UnifiedRequest) -> Result<UnifiedResponse, GatewayError> {
        let started = Instant::now();
        let body = ChatCompletionRequest::from_unified(&self.cfg, &req);

        debug!(
            request_id = %req.request_id,
            model = %self.cfg.model,
            endpoint = %self.cfg.endpoint,
            messages = req.messages.len(),
            "POST {}", self.url_chat
        );

        let resp = self
            .client
            .post(&self.url_chat)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let url = self.url_chat.clone();
            let text = resp.text().await.unwrap_or_default();
            let snippet = make_snippet(&text);
            error!(
                request_id = %req.request_id,
                %status,
                %url,
                %snippet,
                "OpenAI /v1/chat/completions returned non-success status"
            );
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

        let parsed: ChatCompletionResponse = resp.json().await.map_err(|e| {
            ProviderError::new(
                Provider::OpenAI,
                ProviderErrorKind::Decode(format!(
                    "serde error: {e}; expected `choices[0].message.content` and `usage`"
                )),
            )
        })?;

        let content = parsed
            .choices
            .into_iter()
            .find_map(|c| c.message.content)
            .ok_or_else(|| {
                ProviderError::new(Provider::OpenAI, ProviderErrorKind::EmptyChoices)
            })?;

        let usage = parsed
            .usage
            .map(|u| TokenUsage {
                prompt: u.prompt_tokens,
                completion: u.completion_tokens,
                total: u
                    .total_tokens
                    .unwrap_or_else(|| u.prompt_tokens.saturating_add(u.completion_tokens)),
            })
            .unwrap_or_default();

        Ok(UnifiedResponse {
            content,
            usage,
            cost: Default::default(),
            model: self.cfg.model.clone(),
            provider: ProviderKind::OpenAI,
            latency_ms: started.elapsed().as_millis() as u64,
            request_id: req.request_id,
        })
    }

    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        check_models(&self.client, &self.url_models, &self.cfg.model, &self.cfg.endpoint).await
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        &self.cfg.model
    }
    fn endpoint(&self) -> &str {
        &self.cfg.endpoint
    }
}

/* --------------------------------------------------------------------- */
/* EmbeddingProvider                                                     */
/* --------------------------------------------------------------------- */

#[async_trait]
impl EmbeddingProvider for OpenAiProvider {
    async fn embed_batch(
        &self,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, GatewayError> {
        let started = Instant::now();
        let body = EmbeddingHttpRequest {
            model: &self.cfg.model,
            input: &req.inputs,
        };

        debug!(
            request_id = %req.request_id,
            model = %self.cfg.model,
            endpoint = %self.cfg.endpoint,
            batch_size = req.inputs.len(),
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
                request_id = %req.request_id,
                %status,
                %url,
                %snippet,
                "OpenAI /v1/embeddings returned non-success status"
            );
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

        let parsed: EmbeddingHttpResponse = resp.json().await.map_err(|e| {
            ProviderError::new(
                Provider::OpenAI,
                ProviderErrorKind::Decode(format!(
                    "serde error: {e}; expected `data[].embedding` and `usage`"
                )),
            )
        })?;

        let mut by_index: Vec<(usize, Vec<f32>)> = parsed
            .data
            .into_iter()
            .map(|d| (d.index, d.embedding))
            .collect();
        by_index.sort_by_key(|(i, _)| *i);
        let vectors = by_index.into_iter().map(|(_, v)| v).collect::<Vec<_>>();

        let usage = parsed
            .usage
            .map(|u| TokenUsage {
                prompt: u.prompt_tokens,
                completion: 0,
                total: u.total_tokens.unwrap_or(u.prompt_tokens),
            })
            .unwrap_or_default();

        Ok(EmbeddingResponse {
            vectors,
            usage,
            cost: Default::default(),
            model: self.cfg.model.clone(),
            provider: ProviderKind::OpenAI,
            latency_ms: started.elapsed().as_millis() as u64,
            request_id: req.request_id,
        })
    }

    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        check_models(&self.client, &self.url_models, &self.cfg.model, &self.cfg.endpoint).await
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        &self.cfg.model
    }
    fn endpoint(&self) -> &str {
        &self.cfg.endpoint
    }
}

/* --------------------------------------------------------------------- */
/* Helpers                                                               */
/* --------------------------------------------------------------------- */

async fn check_models(
    client: &Client,
    url_models: &str,
    model: &str,
    endpoint: &str,
) -> Result<HealthInfo, GatewayError> {
    let started = Instant::now();
    let resp = client.get(url_models).send().await?;
    let latency_ms = started.elapsed().as_millis() as u64;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let snippet = make_snippet(&text);
        return Err(GatewayError::Health(HealthError::HttpStatus(HttpError {
            status,
            url: url_models.to_string(),
            snippet,
        })));
    }

    #[derive(Deserialize)]
    struct ModelItem {
        id: String,
    }
    #[derive(Deserialize)]
    struct Models {
        data: Vec<ModelItem>,
    }

    match resp.json::<Models>().await {
        Ok(m) => {
            let exists = m.data.iter().any(|x| x.id == model);
            if exists {
                Ok(HealthInfo::ok(
                    format!("OpenAI is healthy at {endpoint}; model `{model}` available"),
                    latency_ms,
                ))
            } else {
                Ok(HealthInfo::fail(
                    format!(
                        "OpenAI is up at {endpoint}, but model `{model}` not in /v1/models"
                    ),
                    latency_ms,
                ))
            }
        }
        Err(e) => Ok(HealthInfo::ok(
            format!("OpenAI is reachable at {endpoint}; failed to decode /v1/models: {e}"),
            latency_ms,
        )),
    }
}

/* --------------------------------------------------------------------- */
/* HTTP DTOs                                                             */
/* --------------------------------------------------------------------- */

#[derive(Debug, Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessageOut<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    stop: Vec<String>,
}

impl<'a> ChatCompletionRequest<'a> {
    fn from_unified(cfg: &'a ProviderConfig, req: &'a UnifiedRequest) -> Self {
        let messages = req
            .messages
            .iter()
            .map(|m| ChatMessageOut {
                role: role_str(m.role),
                content: &m.content,
            })
            .collect();
        Self {
            model: &cfg.model,
            messages,
            temperature: req.temperature.or(cfg.temperature),
            top_p: req.top_p.or(cfg.top_p),
            max_tokens: req.max_tokens.or(cfg.max_tokens),
            seed: req.seed,
            stop: req.stop.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ChatMessageOut<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<UsageBlock>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessageIn,
}

#[derive(Debug, Deserialize)]
struct ChatMessageIn {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageBlock {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct EmbeddingHttpRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Debug, Deserialize)]
struct EmbeddingHttpResponse {
    data: Vec<EmbeddingItem>,
    #[serde(default)]
    usage: Option<UsageBlock>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

fn role_str(role: Role) -> &'static str {
    role.as_str()
}
