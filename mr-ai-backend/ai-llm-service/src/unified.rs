//! Unified, provider-agnostic message schema for the LLM Gateway.
//!
//! All callers communicate with the gateway through these types; provider
//! crates translate them to and from native JSON shapes (`/api/chat`,
//! `/v1/chat/completions`, `/v1/embeddings`, ...).

use serde::{Deserialize, Serialize};

use crate::config::ProviderKind;

/// Conversation role for a single message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// A single conversation message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiedMessage {
    pub role: Role,
    pub content: String,
}

impl UnifiedMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// Provider-agnostic completion request.
///
/// Streaming is reserved for a future sprint; current providers always issue
/// non-streaming requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedRequest {
    pub messages: Vec<UnifiedMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stop: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    pub request_id: String,
}

impl UnifiedRequest {
    /// Creates a request with a single user message and a fresh request id.
    pub fn user_only(content: impl Into<String>) -> Self {
        Self {
            messages: vec![UnifiedMessage::user(content)],
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            seed: None,
            request_id: new_request_id(),
        }
    }

    /// Creates a request with an optional system message and a user message.
    pub fn with_system(system: Option<impl Into<String>>, user: impl Into<String>) -> Self {
        let mut messages = Vec::with_capacity(2);
        if let Some(s) = system {
            messages.push(UnifiedMessage::system(s));
        }
        messages.push(UnifiedMessage::user(user));
        Self {
            messages,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            seed: None,
            request_id: new_request_id(),
        }
    }
}

/// Token usage normalised across providers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt: u32,
    pub completion: u32,
    pub total: u32,
}

impl TokenUsage {
    pub fn new(prompt: u32, completion: u32) -> Self {
        Self {
            prompt,
            completion,
            total: prompt.saturating_add(completion),
        }
    }
}

/// Cost estimate in USD for a single request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub usd: f64,
}

/// Provider-agnostic completion response.
#[derive(Debug, Clone)]
pub struct UnifiedResponse {
    pub content: String,
    pub usage: TokenUsage,
    pub cost: CostEstimate,
    pub model: String,
    pub provider: ProviderKind,
    pub latency_ms: u64,
    pub request_id: String,
}

/// Embedding request: a batch of input strings to embed.
#[derive(Debug, Clone)]
pub struct EmbeddingRequest {
    pub inputs: Vec<String>,
    pub request_id: String,
}

impl EmbeddingRequest {
    pub fn new(inputs: Vec<String>) -> Self {
        Self {
            inputs,
            request_id: new_request_id(),
        }
    }
}

/// Embedding response: vectors aligned with `inputs`, plus token / cost info.
#[derive(Debug, Clone)]
pub struct EmbeddingResponse {
    pub vectors: Vec<Vec<f32>>,
    pub usage: TokenUsage,
    pub cost: CostEstimate,
    pub model: String,
    pub provider: ProviderKind,
    pub latency_ms: u64,
    pub request_id: String,
}

/// Generates a fresh UUID v4 used as a request identifier in logs.
pub fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
