//! Typed configuration for the LLM Gateway, loaded from environment variables.

pub mod log;
pub mod pricing;
pub mod provider_config;
pub mod provider_kind;

use std::collections::HashMap;
use std::path::PathBuf;

pub use log::LogConfig;
pub use pricing::{ModelPrice, PriceTable};
pub use provider_config::ProviderConfig;
pub use provider_kind::ProviderKind;

use crate::errors::{ConfigError, GatewayError, env_opt_f32, env_opt_u32, env_opt_u64, must_env};

/// Top-level gateway configuration (validated tree).
///
/// Built from environment variables via [`GatewayConfig::from_env`].
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub fast: ProviderConfig,
    pub smart: ProviderConfig,
    pub embedding: ProviderConfig,
    pub pricing_path: PathBuf,
    pub log: LogConfig,
    pub health_timeout_secs: u64,
}

impl GatewayConfig {
    /// Reads the full config tree from environment variables.
    ///
    /// Required variables:
    /// - `LLM_FAST_PROVIDER`, `LLM_FAST_MODEL`, plus the corresponding
    ///   provider's endpoint/API key environment.
    /// - `LLM_SMART_PROVIDER`, `LLM_SMART_MODEL`, ditto.
    /// - `LLM_EMBED_PROVIDER`, `LLM_EMBED_MODEL`, ditto.
    pub fn from_env() -> Result<Self, GatewayError> {
        let fast = load_provider_config("FAST")?;
        let smart = load_provider_config("SMART")?;
        let embedding = load_provider_config("EMBED")?;

        let pricing_path = std::env::var("LLM_PRICING_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("pricing.toml"));

        let log = LogConfig {
            dir: std::env::var("LOG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("logs")),
            file_prefix: std::env::var("LOG_FILE_PREFIX")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "mr-ai".to_string()),
            level: std::env::var("LOG_LEVEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "info".to_string()),
        };

        let health_timeout_secs = env_opt_u64("LLM_HEALTH_TIMEOUT_SECS")?.unwrap_or(10);

        Ok(Self {
            fast,
            smart,
            embedding,
            pricing_path,
            log,
            health_timeout_secs,
        })
    }
}

/// Loads a single tier's provider config from prefixed env vars.
///
/// For prefix `FAST` the variables read are:
/// `LLM_FAST_PROVIDER`, `LLM_FAST_MODEL`, `LLM_FAST_ENDPOINT`,
/// `LLM_FAST_API_KEY`, `LLM_FAST_MAX_TOKENS`, `LLM_FAST_TEMPERATURE`,
/// `LLM_FAST_TOP_P`, `LLM_FAST_TIMEOUT_SECS`.
///
/// For Ollama, `LLM_<TIER>_ENDPOINT` falls back to `OLLAMA_URL`.
/// For OpenAI, `LLM_<TIER>_ENDPOINT` falls back to `https://api.openai.com`
/// and `LLM_<TIER>_API_KEY` falls back to `OPENAI_API_KEY`.
fn load_provider_config(tier: &'static str) -> Result<ProviderConfig, GatewayError> {
    let provider_var: &'static str = leak(&format!("LLM_{tier}_PROVIDER"));
    let model_var: &'static str = leak(&format!("LLM_{tier}_MODEL"));

    let provider_str = must_env(provider_var)?;
    let provider = ProviderKind::from_env_str(&provider_str)
        .ok_or_else(|| ConfigError::UnsupportedProvider(provider_str.clone()))?;

    let model = must_env(model_var)?;
    if model.trim().is_empty() {
        return Err(ConfigError::EmptyModel.into());
    }

    let endpoint = resolve_endpoint(provider, tier)?;
    let api_key = resolve_api_key(provider, tier);
    let extras = resolve_extras(provider, tier);

    let max_tokens = env_opt_u32(leak(&format!("LLM_{tier}_MAX_TOKENS")))?;
    let temperature = env_opt_f32(leak(&format!("LLM_{tier}_TEMPERATURE")))?;
    let top_p = env_opt_f32(leak(&format!("LLM_{tier}_TOP_P")))?;
    let timeout_secs = env_opt_u64(leak(&format!("LLM_{tier}_TIMEOUT_SECS")))?;

    if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
        return Err(ConfigError::InvalidFormat {
            var: leak(&format!("LLM_{tier}_ENDPOINT")),
            reason: "must start with http:// or https://",
        }
        .into());
    }

    Ok(ProviderConfig {
        provider,
        model,
        endpoint,
        api_key,
        max_tokens,
        temperature,
        top_p,
        timeout_secs,
        extras,
    })
}

fn resolve_endpoint(provider: ProviderKind, tier: &'static str) -> Result<String, GatewayError> {
    let primary = format!("LLM_{tier}_ENDPOINT");
    if let Ok(v) = std::env::var(&primary) {
        if !v.trim().is_empty() {
            return Ok(v);
        }
    }
    match provider {
        ProviderKind::Ollama => Ok(std::env::var("OLLAMA_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "http://localhost:11434".to_string())),
        ProviderKind::OpenAI => Ok(std::env::var("OPENAI_BASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "https://api.openai.com".to_string())),
        ProviderKind::Bedrock => {
            let region = bedrock_region(tier);
            Ok(format!("https://bedrock-runtime.{region}.amazonaws.com"))
        }
    }
}

fn resolve_api_key(provider: ProviderKind, tier: &'static str) -> Option<String> {
    let primary = format!("LLM_{tier}_API_KEY");
    if let Ok(v) = std::env::var(&primary) {
        if !v.trim().is_empty() {
            return Some(v);
        }
    }
    match provider {
        ProviderKind::OpenAI => std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        ProviderKind::Bedrock => std::env::var("AWS_ACCESS_KEY_ID")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        ProviderKind::Ollama => None,
    }
}

fn resolve_extras(provider: ProviderKind, tier: &'static str) -> HashMap<String, String> {
    let mut extras = HashMap::new();
    if provider == ProviderKind::Bedrock {
        extras.insert("region".to_string(), bedrock_region(tier));
        if let Some(secret) = first_non_empty_env(&[
            &format!("LLM_{tier}_SECRET_KEY"),
            "AWS_SECRET_ACCESS_KEY",
        ]) {
            extras.insert("secret_key".to_string(), secret);
        }
        if let Some(token) = first_non_empty_env(&[
            &format!("LLM_{tier}_SESSION_TOKEN"),
            "AWS_SESSION_TOKEN",
        ]) {
            extras.insert("session_token".to_string(), token);
        }
    }
    extras
}

fn bedrock_region(tier: &'static str) -> String {
    first_non_empty_env(&[
        &format!("LLM_{tier}_REGION"),
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
    ])
    .unwrap_or_else(|| "us-east-1".to_string())
}

fn first_non_empty_env(names: &[&str]) -> Option<String> {
    for name in names {
        if let Ok(v) = std::env::var(name) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// Leaks a `String` into a `&'static str`. Used only at startup for env-var
/// names derived from a tier prefix; the leak is bounded and one-shot.
fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}
