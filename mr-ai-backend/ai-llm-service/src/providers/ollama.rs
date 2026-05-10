//! Ollama provider — `/api/chat` for completion, `/api/embeddings` for vectors.
//!
//! Token usage is taken directly from Ollama's response (`prompt_eval_count`
//! and `eval_count`). Embeddings are produced sequentially since native Ollama
//! takes a single `prompt` per call.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Client;
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

/// Concrete Ollama provider client.
#[derive(Debug)]
pub struct OllamaProvider {
    client: Client,
    cfg: ProviderConfig,
    url_chat: String,
    url_embeddings: String,
    url_tags: String,
}

impl OllamaProvider {
    pub fn new(cfg: ProviderConfig) -> Result<Self, GatewayError> {
        if cfg.provider != ProviderKind::Ollama {
            return Err(
                ProviderError::new(Provider::Ollama, ProviderErrorKind::InvalidProvider).into(),
            );
        }
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
        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(60));
        let client = Client::builder().timeout(timeout).build()?;
        let base = endpoint.trim_end_matches('/').to_string();

        info!(
            provider = %ProviderKind::Ollama,
            model = %cfg.model,
            endpoint = %cfg.endpoint,
            timeout_secs = cfg.timeout_secs.unwrap_or(60),
            "OllamaProvider initialised"
        );

        Ok(Self {
            client,
            url_chat: format!("{base}/api/chat"),
            url_embeddings: format!("{base}/api/embeddings"),
            url_tags: format!("{base}/api/tags"),
            cfg,
        })
    }
}

/* --------------------------------------------------------------------- */
/* LlmProvider                                                           */
/* --------------------------------------------------------------------- */

#[async_trait]
impl LlmProvider for OllamaProvider {
    async fn complete(&self, req: UnifiedRequest) -> Result<UnifiedResponse, GatewayError> {
        let started = Instant::now();
        let body = ChatRequest::from_unified(&self.cfg, &req);

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
                "Ollama /api/chat returned non-success status"
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

        let parsed: ChatResponse = resp.json().await.map_err(|e| {
            ProviderError::new(
                Provider::Ollama,
                ProviderErrorKind::Decode(format!(
                    "serde error: {e}; expected non-streaming /api/chat response"
                )),
            )
        })?;

        let content = parsed
            .message
            .map(|m| m.content)
            .ok_or_else(|| ProviderError::new(Provider::Ollama, ProviderErrorKind::EmptyChoices))?;

        let usage = TokenUsage::new(
            parsed.prompt_eval_count.unwrap_or(0),
            parsed.eval_count.unwrap_or(0),
        );

        Ok(UnifiedResponse {
            content,
            usage,
            cost: Default::default(), // filled in by gateway after lookup
            model: self.cfg.model.clone(),
            provider: ProviderKind::Ollama,
            latency_ms: started.elapsed().as_millis() as u64,
            request_id: req.request_id,
        })
    }

    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        check_tags(
            &self.client,
            &self.url_tags,
            &self.cfg.model,
            &self.cfg.endpoint,
        )
        .await
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Ollama
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
impl EmbeddingProvider for OllamaProvider {
    async fn embed_batch(
        &self,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, GatewayError> {
        let started = Instant::now();
        let mut out = Vec::with_capacity(req.inputs.len());
        let mut total_prompt: u32 = 0;

        for input in &req.inputs {
            let body = EmbeddingHttpRequest {
                model: &self.cfg.model,
                prompt: input,
            };

            debug!(
                request_id = %req.request_id,
                model = %self.cfg.model,
                endpoint = %self.cfg.endpoint,
                input_len = input.len(),
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

            let parsed: EmbeddingHttpResponse = resp.json().await.map_err(|e| {
                ProviderError::new(
                    Provider::Ollama,
                    ProviderErrorKind::Decode(format!(
                        "serde error: {e}; expected `embedding` array"
                    )),
                )
            })?;

            total_prompt = total_prompt.saturating_add(parsed.prompt_eval_count.unwrap_or(0));
            out.push(parsed.embedding);
        }

        Ok(EmbeddingResponse {
            vectors: out,
            usage: TokenUsage::new(total_prompt, 0),
            cost: Default::default(),
            model: self.cfg.model.clone(),
            provider: ProviderKind::Ollama,
            latency_ms: started.elapsed().as_millis() as u64,
            request_id: req.request_id,
        })
    }

    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        check_tags(
            &self.client,
            &self.url_tags,
            &self.cfg.model,
            &self.cfg.endpoint,
        )
        .await
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Ollama
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

async fn check_tags(
    client: &Client,
    url_tags: &str,
    model: &str,
    endpoint: &str,
) -> Result<HealthInfo, GatewayError> {
    let started = Instant::now();
    let resp = client.get(url_tags).send().await?;
    let latency_ms = started.elapsed().as_millis() as u64;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let snippet = make_snippet(&text);
        return Err(GatewayError::Health(HealthError::HttpStatus(HttpError {
            status,
            url: url_tags.to_string(),
            snippet,
        })));
    }

    #[derive(Deserialize)]
    struct Tag {
        name: String,
    }
    #[derive(Deserialize)]
    struct Tags {
        models: Option<Vec<Tag>>,
    }

    match resp.json::<Tags>().await {
        Ok(tags) => {
            if let Some(models) = tags.models {
                let exists = models.iter().any(|m| m.name == model);
                if exists {
                    Ok(HealthInfo::ok(
                        format!("Ollama is healthy at {endpoint}; model `{model}` available"),
                        latency_ms,
                    ))
                } else {
                    Ok(HealthInfo::fail(
                        format!(
                            "Ollama is up at {endpoint}, but model `{model}` not found in /api/tags"
                        ),
                        latency_ms,
                    ))
                }
            } else {
                Ok(HealthInfo::ok(
                    format!("Ollama is healthy at {endpoint}; tags response missing `models`"),
                    latency_ms,
                ))
            }
        }
        Err(e) => Ok(HealthInfo::ok(
            format!("Ollama is reachable at {endpoint}; failed to decode /api/tags: {e}"),
            latency_ms,
        )),
    }
}

/* --------------------------------------------------------------------- */
/* HTTP DTOs                                                             */
/* --------------------------------------------------------------------- */

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessageOut<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<ChatOptions>,
}

impl<'a> ChatRequest<'a> {
    fn from_unified(cfg: &'a ProviderConfig, req: &'a UnifiedRequest) -> Self {
        let messages = req
            .messages
            .iter()
            .map(|m| ChatMessageOut {
                role: role_str(m.role),
                content: &m.content,
            })
            .collect();
        let options = ChatOptions {
            temperature: req.temperature.or(cfg.temperature),
            top_p: req.top_p.or(cfg.top_p),
            num_predict: req.max_tokens.or(cfg.max_tokens),
            seed: req.seed,
            stop: if req.stop.is_empty() {
                None
            } else {
                Some(req.stop.clone())
            },
        };
        Self {
            model: &cfg.model,
            messages,
            stream: false,
            options: Some(options),
        }
    }
}

#[derive(Debug, Serialize)]
struct ChatMessageOut<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Default, Serialize)]
struct ChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: Option<ChatMessageIn>,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ChatMessageIn {
    #[allow(dead_code)]
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct EmbeddingHttpRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Debug, Deserialize)]
struct EmbeddingHttpResponse {
    embedding: Vec<f32>,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
}

fn role_str(role: Role) -> &'static str {
    role.as_str()
}
