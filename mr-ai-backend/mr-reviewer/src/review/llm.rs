//! LLM client (enum-dispatch) without async-trait or heap objects.
//!
//! Default implementation: local Ollama (`/api/generate`).

use super::prompt::Prompt;
use crate::errors::MrResult;
use serde::{Deserialize, Serialize};

/// Config for the LLM provider.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Provider kind (only Ollama implemented here).
    pub kind: LlmKind,
    /// Model name (e.g., "qwen2.5-coder:7b-instruct", "llama3.1:8b-instruct").
    pub model: String,
    /// HTTP endpoint (e.g., "http://127.0.0.1:11434").
    pub endpoint: String,
    /// Optional max tokens or other provider-specific knobs (not used here).
    pub max_tokens: Option<u32>,
}

/// Supported providers.
#[derive(Debug, Clone, Copy)]
pub enum LlmKind {
    Ollama,
    // OpenAI, Anthropic — add later.
}

/// Thin enum client for dispatch.
pub enum LlmClient {
    Ollama(OllamaClient),
}

impl LlmClient {
    /// Construct a client based on the given provider config.
    pub fn from_config(cfg: LlmConfig) -> MrResult<Self> {
        match cfg.kind {
            LlmKind::Ollama => Ok(Self::Ollama(OllamaClient { cfg })),
        }
    }

    /// Generate a completion for the given prompt.
    pub async fn generate(&self, prompt: &Prompt) -> MrResult<String> {
        match self {
            Self::Ollama(c) => c.generate(prompt).await,
        }
    }
}

/// Concrete Ollama client.
#[derive(Debug, Clone)]
pub struct OllamaClient {
    cfg: LlmConfig,
}

#[derive(Serialize)]
struct OllamaReq<'a> {
    model: &'a str,
    prompt: String,
    stream: bool,
}

#[derive(Deserialize)]
struct OllamaResp {
    response: String,
}

impl OllamaClient {
    /// Call `POST /api/generate` with a combined system+user prompt.
    pub async fn generate(&self, p: &Prompt) -> MrResult<String> {
        let url = format!("{}/api/generate", self.cfg.endpoint.trim_end_matches('/'));
        let full_prompt = format!("{}\n\n{}", p.system, p.user);

        let req = OllamaReq {
            model: &self.cfg.model,
            prompt: full_prompt,
            stream: false,
        };

        let resp = reqwest::Client::new()
            .post(url)
            .json(&req)
            .send()
            .await?
            .error_for_status()?;

        let body: OllamaResp = resp.json().await?;
        Ok(body.response)
    }
}
