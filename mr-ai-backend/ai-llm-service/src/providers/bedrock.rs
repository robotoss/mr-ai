//! AWS Bedrock provider — `Converse` API for chat, `InvokeModel` for Titan
//! embeddings.
//!
//! Demonstrates that adding a new endpoint family is purely a matter of
//! implementing [`LlmProvider`] / [`EmbeddingProvider`] and registering the
//! constructor in [`crate::gateway`]; no other layer changes.
//!
//! ## Configuration
//!
//! `ProviderConfig` carries:
//! - `model`           — Bedrock model id, e.g. `anthropic.claude-3-5-sonnet-20241022-v2:0`
//!                       or `amazon.titan-embed-text-v2:0`.
//! - `endpoint`        — `https://bedrock-runtime.<region>.amazonaws.com`
//!                       (or a custom VPC endpoint).
//! - `api_key`         — AWS access-key-id (e.g. `AKIA...`).
//! - `extras["region"]`           — AWS region (`us-east-1`, …).
//! - `extras["secret_key"]`       — AWS secret-access-key.
//! - `extras["session_token"]`    — optional STS session token.
//!
//! Health-check is intentionally local: hitting Bedrock costs quota / money,
//! so we only verify the credentials are present and the endpoint URL is
//! well-formed. Real reachability is exercised the moment you make a real
//! request.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

use crate::config::provider_kind::ProviderKind;
use crate::config::ProviderConfig;
use crate::errors::{
    GatewayError, HttpError, Provider, ProviderError, ProviderErrorKind, make_snippet,
};
use crate::sigv4::{sign, SigV4Request};
use crate::traits::{EmbeddingProvider, HealthInfo, LlmProvider};
use crate::unified::{
    EmbeddingRequest, EmbeddingResponse, Role, TokenUsage, UnifiedRequest, UnifiedResponse,
};

const SERVICE: &str = "bedrock";

#[derive(Debug)]
pub struct BedrockProvider {
    client: Client,
    cfg: ProviderConfig,
    region: String,
    secret_key: String,
    session_token: Option<String>,
    host: String,
    base: String,
}

impl BedrockProvider {
    pub fn new(cfg: ProviderConfig) -> Result<Self, GatewayError> {
        if cfg.provider != ProviderKind::Bedrock {
            return Err(
                ProviderError::new(Provider::Bedrock, ProviderErrorKind::InvalidProvider).into(),
            );
        }

        let access_key_id = cfg.api_key.clone().ok_or_else(|| {
            ProviderError::new(Provider::Bedrock, ProviderErrorKind::MissingApiKey)
        })?;
        if access_key_id.trim().is_empty() {
            return Err(
                ProviderError::new(Provider::Bedrock, ProviderErrorKind::MissingApiKey).into(),
            );
        }

        let region = cfg
            .extras
            .get("region")
            .cloned()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                ProviderError::new(
                    Provider::Bedrock,
                    ProviderErrorKind::Decode(
                        "extras['region'] is required for AWS Bedrock".into(),
                    ),
                )
            })?;

        let secret_key = cfg
            .extras
            .get("secret_key")
            .cloned()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                ProviderError::new(
                    Provider::Bedrock,
                    ProviderErrorKind::Decode(
                        "extras['secret_key'] is required for AWS Bedrock".into(),
                    ),
                )
            })?;

        let session_token = cfg
            .extras
            .get("session_token")
            .cloned()
            .filter(|s| !s.trim().is_empty());

        let endpoint = cfg.endpoint.trim();
        if endpoint.is_empty()
            || !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            return Err(ProviderError::new(
                Provider::Bedrock,
                ProviderErrorKind::InvalidEndpoint(cfg.endpoint.clone()),
            )
            .into());
        }
        let base = endpoint.trim_end_matches('/').to_string();
        let host = host_from_url(&base).ok_or_else(|| {
            ProviderError::new(
                Provider::Bedrock,
                ProviderErrorKind::InvalidEndpoint(base.clone()),
            )
        })?;

        let timeout = cfg
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(60));
        let client = Client::builder().timeout(timeout).build()?;

        info!(
            provider = %ProviderKind::Bedrock,
            model = %cfg.model,
            endpoint = %cfg.endpoint,
            region = %region,
            timeout_secs = cfg.timeout_secs.unwrap_or(60),
            "BedrockProvider initialised"
        );

        // `cfg.api_key` participates in signing but is not retained explicitly;
        // we keep it on the cfg only.
        let _ = access_key_id;

        Ok(Self {
            client,
            cfg,
            region,
            secret_key,
            session_token,
            host,
            base,
        })
    }

    fn access_key_id(&self) -> &str {
        // Validated as Some non-empty in `new`.
        self.cfg.api_key.as_deref().unwrap_or_default()
    }

    /// Signs `body` and dispatches a POST to `path` (relative to `self.base`).
    async fn signed_post(
        &self,
        path: &str,
        body: Vec<u8>,
        request_id: &str,
    ) -> Result<reqwest::Response, GatewayError> {
        let signed = sign(&SigV4Request {
            method: "POST",
            host: &self.host,
            path,
            body: &body,
            region: &self.region,
            service: SERVICE,
            access_key_id: self.access_key_id(),
            secret_access_key: &self.secret_key,
            session_token: self.session_token.as_deref(),
            now: Utc::now(),
        });

        let url = format!("{}{}", self.base, path);
        let mut builder = self
            .client
            .post(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-amz-date", &signed.x_amz_date)
            .header("x-amz-content-sha256", &signed.x_amz_content_sha256)
            .header(header::AUTHORIZATION, &signed.authorization)
            .body(body);

        if let Some(token) = &signed.x_amz_security_token {
            builder = builder.header("x-amz-security-token", token);
        }

        debug!(
            request_id = %request_id,
            model = %self.cfg.model,
            region = %self.region,
            "POST {url}"
        );

        Ok(builder.send().await?)
    }
}

/* --------------------------------------------------------------------- */
/* LlmProvider — Bedrock Converse                                        */
/* --------------------------------------------------------------------- */

#[async_trait]
impl LlmProvider for BedrockProvider {
    async fn complete(&self, req: UnifiedRequest) -> Result<UnifiedResponse, GatewayError> {
        let started = Instant::now();
        let body = serde_json::to_vec(&ConverseRequest::from_unified(&self.cfg, &req))
            .map_err(|e| {
                ProviderError::new(
                    Provider::Bedrock,
                    ProviderErrorKind::Decode(format!("failed to encode request: {e}")),
                )
            })?;

        let path = format!("/model/{}/converse", self.cfg.model);
        let resp = self.signed_post(&path, body, &req.request_id).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let url = format!("{}{}", self.base, path);
            let text = resp.text().await.unwrap_or_default();
            let snippet = make_snippet(&text);
            error!(
                request_id = %req.request_id,
                %status,
                %url,
                %snippet,
                "Bedrock Converse returned non-success status"
            );
            return Err(ProviderError::new(
                Provider::Bedrock,
                ProviderErrorKind::HttpStatus(HttpError {
                    status,
                    url,
                    snippet,
                }),
            )
            .into());
        }

        let parsed: ConverseResponse = resp.json().await.map_err(|e| {
            ProviderError::new(
                Provider::Bedrock,
                ProviderErrorKind::Decode(format!(
                    "serde error: {e}; expected `output.message.content[].text`"
                )),
            )
        })?;

        let content = parsed
            .output
            .and_then(|o| o.message)
            .map(|m| {
                m.content
                    .into_iter()
                    .filter_map(|p| p.text)
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ProviderError::new(Provider::Bedrock, ProviderErrorKind::EmptyChoices)
            })?;

        let usage = parsed
            .usage
            .map(|u| TokenUsage {
                prompt: u.input_tokens,
                completion: u.output_tokens,
                total: u
                    .total_tokens
                    .unwrap_or_else(|| u.input_tokens.saturating_add(u.output_tokens)),
            })
            .unwrap_or_default();

        Ok(UnifiedResponse {
            content,
            usage,
            cost: Default::default(),
            model: self.cfg.model.clone(),
            provider: ProviderKind::Bedrock,
            latency_ms: started.elapsed().as_millis() as u64,
            request_id: req.request_id,
        })
    }

    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        // Local-only: avoid burning quota on a real AWS round-trip.
        Ok(HealthInfo::ok(
            format!(
                "Bedrock provider configured for region={} model={} (no live probe)",
                self.region, self.cfg.model
            ),
            0,
        ))
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Bedrock
    }
    fn model(&self) -> &str {
        &self.cfg.model
    }
    fn endpoint(&self) -> &str {
        &self.cfg.endpoint
    }
}

/* --------------------------------------------------------------------- */
/* EmbeddingProvider — Bedrock InvokeModel (Titan v2 schema)             */
/* --------------------------------------------------------------------- */

#[async_trait]
impl EmbeddingProvider for BedrockProvider {
    async fn embed_batch(
        &self,
        req: EmbeddingRequest,
    ) -> Result<EmbeddingResponse, GatewayError> {
        let started = Instant::now();
        let mut vectors = Vec::with_capacity(req.inputs.len());
        let mut total_prompt: u32 = 0;

        // Titan Embeddings v2 accepts one input per call.
        for input in &req.inputs {
            let body = serde_json::to_vec(&TitanEmbedRequest { input_text: input })
                .map_err(|e| {
                    ProviderError::new(
                        Provider::Bedrock,
                        ProviderErrorKind::Decode(format!("encode embed body: {e}")),
                    )
                })?;

            let path = format!("/model/{}/invoke", self.cfg.model);
            let resp = self.signed_post(&path, body, &req.request_id).await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let url = format!("{}{}", self.base, path);
                let text = resp.text().await.unwrap_or_default();
                let snippet = make_snippet(&text);
                error!(
                    request_id = %req.request_id,
                    %status,
                    %url,
                    %snippet,
                    "Bedrock InvokeModel (embeddings) returned non-success status"
                );
                return Err(ProviderError::new(
                    Provider::Bedrock,
                    ProviderErrorKind::HttpStatus(HttpError {
                        status,
                        url,
                        snippet,
                    }),
                )
                .into());
            }

            let parsed: TitanEmbedResponse = resp.json().await.map_err(|e| {
                ProviderError::new(
                    Provider::Bedrock,
                    ProviderErrorKind::Decode(format!("decode Titan embed: {e}")),
                )
            })?;

            total_prompt = total_prompt.saturating_add(parsed.input_text_token_count.unwrap_or(0));
            vectors.push(parsed.embedding);
        }

        Ok(EmbeddingResponse {
            vectors,
            usage: TokenUsage::new(total_prompt, 0),
            cost: Default::default(),
            model: self.cfg.model.clone(),
            provider: ProviderKind::Bedrock,
            latency_ms: started.elapsed().as_millis() as u64,
            request_id: req.request_id,
        })
    }

    async fn health_check(&self) -> Result<HealthInfo, GatewayError> {
        Ok(HealthInfo::ok(
            format!(
                "Bedrock embedding provider configured for region={} model={}",
                self.region, self.cfg.model
            ),
            0,
        ))
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Bedrock
    }
    fn model(&self) -> &str {
        &self.cfg.model
    }
    fn endpoint(&self) -> &str {
        &self.cfg.endpoint
    }
}

/* --------------------------------------------------------------------- */
/* Helpers + DTOs                                                        */
/* --------------------------------------------------------------------- */

fn host_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest)?;
    let host = after_scheme.split('/').next()?.split('?').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[derive(Debug, Serialize)]
struct ConverseRequest<'a> {
    messages: Vec<ConverseMessage<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<ConverseSystemBlock<'a>>,
    #[serde(rename = "inferenceConfig", skip_serializing_if = "Option::is_none")]
    inference_config: Option<InferenceConfig>,
}

impl<'a> ConverseRequest<'a> {
    fn from_unified(cfg: &'a ProviderConfig, req: &'a UnifiedRequest) -> Self {
        // Bedrock Converse separates system messages from the conversation.
        let mut system = Vec::new();
        let mut messages = Vec::new();
        for m in &req.messages {
            match m.role {
                Role::System => system.push(ConverseSystemBlock { text: &m.content }),
                Role::User => messages.push(ConverseMessage {
                    role: "user",
                    content: vec![ConverseContentBlock { text: &m.content }],
                }),
                Role::Assistant => messages.push(ConverseMessage {
                    role: "assistant",
                    content: vec![ConverseContentBlock { text: &m.content }],
                }),
            }
        }

        let inference_config = Some(InferenceConfig {
            max_tokens: req.max_tokens.or(cfg.max_tokens),
            temperature: req.temperature.or(cfg.temperature),
            top_p: req.top_p.or(cfg.top_p),
            stop_sequences: if req.stop.is_empty() {
                None
            } else {
                Some(req.stop.clone())
            },
        });

        Self {
            messages,
            system,
            inference_config,
        }
    }
}

#[derive(Debug, Serialize)]
struct ConverseMessage<'a> {
    role: &'a str,
    content: Vec<ConverseContentBlock<'a>>,
}

#[derive(Debug, Serialize)]
struct ConverseContentBlock<'a> {
    text: &'a str,
}

#[derive(Debug, Serialize)]
struct ConverseSystemBlock<'a> {
    text: &'a str,
}

#[derive(Debug, Default, Serialize)]
struct InferenceConfig {
    #[serde(rename = "maxTokens", skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(rename = "topP", skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(rename = "stopSequences", skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ConverseResponse {
    output: Option<ConverseOutput>,
    #[serde(default)]
    usage: Option<UsageBlock>,
}

#[derive(Debug, Deserialize)]
struct ConverseOutput {
    message: Option<ConverseMessageOut>,
}

#[derive(Debug, Deserialize)]
struct ConverseMessageOut {
    #[allow(dead_code)]
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Vec<ConverseContentBlockOut>,
}

#[derive(Debug, Deserialize)]
struct ConverseContentBlockOut {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageBlock {
    #[serde(default, rename = "inputTokens")]
    input_tokens: u32,
    #[serde(default, rename = "outputTokens")]
    output_tokens: u32,
    #[serde(default, rename = "totalTokens")]
    total_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct TitanEmbedRequest<'a> {
    #[serde(rename = "inputText")]
    input_text: &'a str,
}

#[derive(Debug, Deserialize)]
struct TitanEmbedResponse {
    embedding: Vec<f32>,
    #[serde(default, rename = "inputTextTokenCount")]
    input_text_token_count: Option<u32>,
}
