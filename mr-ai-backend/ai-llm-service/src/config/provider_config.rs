//! Per-provider configuration (model, endpoint, sampling, timeout, extras).

use std::collections::HashMap;

use crate::config::provider_kind::ProviderKind;

/// Configuration required to construct a provider client for one tier.
///
/// `extras` carries provider-specific knobs that don't belong on the flat
/// struct (e.g., AWS region, AWS secret key, AWS session token, Cohere
/// `input_type`, Anthropic API version). The Open/Closed extension point
/// for new providers — they can read whatever keys they need without forcing
/// the base struct to change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderConfig {
    pub provider: ProviderKind,
    pub model: String,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub timeout_secs: Option<u64>,
    pub extras: HashMap<String, String>,
}

impl Default for ProviderKind {
    fn default() -> Self {
        ProviderKind::Ollama
    }
}
