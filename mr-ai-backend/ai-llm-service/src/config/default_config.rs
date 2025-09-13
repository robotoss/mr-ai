//! Default LLM configs loaded strictly from environment variables.
//!
//! This module provides convenience constructors for [`LlmModelConfig`],
//! grouped by provider and role. Currently only **Ollama** is supported,
//! with three roles:
//!
//! - **Slow**   → high-quality, slower model (refine/verify)
//! - **Fast**   → lower-quality, faster model (drafting)
//! - **Embedding** → embedding generator
//!
//! Future providers (e.g., ChatGPT, Anthropic) can be added here
//! under the same pattern.
//!
//! # Environment variables
//!
//! Common:
//! - `LLM_KIND` = provider kind (e.g., `ollama`, `chatgpt`)
//! - `LLM_MAX_TOKENS` = optional max tokens (u32)
//!
//! Ollama-specific:
//! - `OLLAMA_URL` or `OLLAMA_PORT` = endpoint (mandatory)
//! - `OLLAMA_MODEL`                = slow/quality model (mandatory)
//! - `OLLAMA_MODEL_FAST_MODEL` or `OLLAMA_MODEL_FAST` = fast/speed model (mandatory)
//! - `EMBEDDING_MODEL`             = embedding model (mandatory)

use crate::{
    config::{llm_model_config::LlmModelConfig, llm_provider::LlmProvider},
    error_handler::{AiLlmError, ConfigError, env_opt_u32, must_env},
};

/// Resolves the Ollama endpoint strictly from environment.
///
/// Precedence:
/// 1. `OLLAMA_URL` if present and non-empty
/// 2. `OLLAMA_PORT` → `http://localhost:{port}`
///
/// # Errors
///
/// - [`ConfigError::MissingVar`] if both are missing
/// - [`ConfigError::InvalidNumber`] if `OLLAMA_PORT` is invalid
fn ollama_endpoint() -> Result<String, AiLlmError> {
    if let Ok(url) = std::env::var("OLLAMA_URL") {
        if !url.trim().is_empty() {
            return Ok(url);
        }
    }
    if let Ok(port) = std::env::var("OLLAMA_PORT") {
        if !port.trim().is_empty() {
            let _ = port
                .parse::<u16>()
                .map_err(|_| ConfigError::InvalidNumber {
                    var: "OLLAMA_PORT",
                    reason: "expected u16 (1..=65535)",
                })?;
            return Ok(format!("http://localhost:{port}"));
        }
    }
    Err(AiLlmError::Config(ConfigError::MissingVar(
        "OLLAMA_URL or OLLAMA_PORT",
    )))
}

/// Constructs a config for the **slow/quality** Ollama model.
///
/// Typically used for high-quality refinement or verification steps.
///
/// # Env
/// - `OLLAMA_MODEL` (required)
/// - `LLM_MAX_TOKENS` (optional)
///
/// # Defaults
/// - `temperature = Some(0.2)`
/// - `timeout_secs = Some(60)`
pub fn config_ollama_slow() -> Result<LlmModelConfig, AiLlmError> {
    let endpoint = ollama_endpoint()?;
    let model = must_env("OLLAMA_MODEL")?;
    let max_tokens = env_opt_u32("LLM_MAX_TOKENS")?;

    Ok(LlmModelConfig {
        provider: LlmProvider::Ollama,
        model,
        endpoint,
        api_key: None,
        max_tokens,
        temperature: Some(0.2),
        top_p: None,
        timeout_secs: Some(600),
    })
}

/// Constructs a config for the **fast/speed** Ollama model.
///
/// Typically used for drafting steps where speed is prioritized.
///
/// # Env
/// - `OLLAMA_MODEL_FAST_MODEL` or `OLLAMA_MODEL_FAST` (required)
/// - `LLM_MAX_TOKENS` (optional)
///
/// # Defaults
/// - `temperature = Some(0.7)`
/// - `top_p = Some(0.9)`
/// - `timeout_secs = Some(45)`
pub fn config_ollama_fast() -> Result<LlmModelConfig, AiLlmError> {
    let endpoint = ollama_endpoint()?;
    let model = std::env::var("OLLAMA_MODEL_FAST_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("OLLAMA_MODEL_FAST").ok())
        .ok_or(ConfigError::MissingVar(
            "OLLAMA_MODEL_FAST_MODEL or OLLAMA_MODEL_FAST",
        ))?;
    let max_tokens = env_opt_u32("LLM_MAX_TOKENS")?;

    Ok(LlmModelConfig {
        provider: LlmProvider::Ollama,
        model,
        endpoint,
        api_key: None,
        max_tokens,
        temperature: Some(0.7),
        top_p: Some(0.9),
        timeout_secs: Some(600),
    })
}

/// Constructs a config for the **embedding** Ollama model.
///
/// Typically used for semantic search, vector databases, or RAG.
///
/// # Env
/// - `EMBEDDING_MODEL` (required)
///
/// # Defaults
/// - `temperature = Some(0.0)` (deterministic)
/// - `max_tokens = None`
/// - `timeout_secs = Some(30)`
pub fn config_ollama_embedding() -> Result<LlmModelConfig, AiLlmError> {
    let endpoint = ollama_endpoint()?;
    let model = must_env("EMBEDDING_MODEL")?;

    Ok(LlmModelConfig {
        provider: LlmProvider::Ollama,
        model,
        endpoint,
        api_key: None,
        max_tokens: None,
        temperature: Some(0.0),
        top_p: None,
        timeout_secs: Some(30),
    })
}
