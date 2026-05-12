//! Unified error hierarchy for the LLM Gateway crate.
//!
//! - Top-level [`GatewayError`] appends the suffix `"[LLM Gateway]"` once
//!   during `Display`.
//! - Provider-level errors are unified through [`Provider`],
//!   [`ProviderErrorKind`], [`ProviderError`].
//! - Shared HTTP error carrier [`HttpError`] (status + url + trimmed snippet).
//! - Env helpers (`must_env`, `env_opt_u32`) are kept for the typed config
//!   loader.

use std::error::Error as StdError;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use reqwest::StatusCode;
use thiserror::Error;

use crate::config::ProviderKind;
use crate::gateway::ModelTier;

/// Top-level error returned by the gateway and its providers.
#[non_exhaustive]
#[derive(Debug)]
pub enum GatewayError {
    Config(ConfigError),
    Health(HealthError),
    Provider(ProviderError),
    Pricing(PricingError),
    UnsupportedTier(ModelTier),
    ProviderNotConfigured(ModelTier),
    HttpTransport(reqwest::Error),
    Timeout(Duration),
    /// Pre-flight estimate or post-call accumulator exceeded the
    /// per-request budget. Sprint 4c (🅰).
    CostCapExceeded {
        request_id: String,
        cumulative_usd: f64,
        cap_usd: f64,
    },
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = match self {
            GatewayError::Config(e) => e.to_string(),
            GatewayError::Health(e) => e.to_string(),
            GatewayError::Provider(e) => e.to_string(),
            GatewayError::Pricing(e) => e.to_string(),
            GatewayError::UnsupportedTier(t) => format!("unsupported model tier: {t:?}"),
            GatewayError::ProviderNotConfigured(t) => {
                format!("no provider configured for tier {t:?}")
            }
            GatewayError::HttpTransport(e) => format!("transport error: {e}"),
            GatewayError::Timeout(d) => format!("operation timed out after {d:?}"),
            GatewayError::CostCapExceeded {
                request_id,
                cumulative_usd,
                cap_usd,
            } => format!(
                "cost cap exceeded: request_id={request_id} cumulative=${cumulative_usd:.6} cap=${cap_usd:.6}"
            ),
        };
        write!(f, "{base} [LLM Gateway]")
    }
}

impl StdError for GatewayError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            GatewayError::Config(e) => Some(e),
            GatewayError::Health(e) => Some(e),
            GatewayError::Provider(e) => Some(e),
            GatewayError::Pricing(e) => Some(e),
            GatewayError::HttpTransport(e) => Some(e),
            GatewayError::UnsupportedTier(_)
            | GatewayError::ProviderNotConfigured(_)
            | GatewayError::Timeout(_)
            | GatewayError::CostCapExceeded { .. } => None,
        }
    }
}

impl From<ConfigError> for GatewayError {
    fn from(e: ConfigError) -> Self {
        GatewayError::Config(e)
    }
}
impl From<HealthError> for GatewayError {
    fn from(e: HealthError) -> Self {
        GatewayError::Health(e)
    }
}
impl From<ProviderError> for GatewayError {
    fn from(e: ProviderError) -> Self {
        GatewayError::Provider(e)
    }
}
impl From<PricingError> for GatewayError {
    fn from(e: PricingError) -> Self {
        GatewayError::Pricing(e)
    }
}
impl From<reqwest::Error> for GatewayError {
    fn from(e: reqwest::Error) -> Self {
        GatewayError::HttpTransport(e)
    }
}

/* --------------------------------------------------------------------- */
/* HTTP error carrier                                                    */
/* --------------------------------------------------------------------- */

#[derive(Debug, Clone)]
pub struct HttpError {
    pub status: StatusCode,
    pub url: String,
    pub snippet: String,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HTTP {} from {}: {}", self.status, self.url, self.snippet)
    }
}

impl StdError for HttpError {}

const SNIPPET_MAX: usize = 256;

/// Trims a response body for safe inclusion in errors and logs.
pub fn make_snippet(s: &str) -> String {
    let s = s.trim();
    if s.len() > SNIPPET_MAX {
        let mut t = s[..SNIPPET_MAX].to_string();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

impl HttpError {
    pub async fn from_response(url: &str, resp: reqwest::Response) -> Self {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Self {
            status,
            url: url.to_string(),
            snippet: make_snippet(&text),
        }
    }
}

/* --------------------------------------------------------------------- */
/* Config errors                                                         */
/* --------------------------------------------------------------------- */

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing required environment variable: {0}")]
    MissingVar(&'static str),

    #[error("invalid number in {var}: {reason}")]
    InvalidNumber {
        var: &'static str,
        reason: &'static str,
    },

    #[error("unsupported provider: {0}")]
    UnsupportedProvider(String),

    #[error("invalid format in {var}: {reason}")]
    InvalidFormat {
        var: &'static str,
        reason: &'static str,
    },

    #[error("model name must not be empty")]
    EmptyModel,
}

/* --------------------------------------------------------------------- */
/* Health errors                                                         */
/* --------------------------------------------------------------------- */

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum HealthError {
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),

    #[error("{0}")]
    HttpStatus(#[from] HttpError),

    #[error("decode error: {0}")]
    Decode(String),
}

/* --------------------------------------------------------------------- */
/* Pricing errors                                                        */
/* --------------------------------------------------------------------- */

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum PricingError {
    #[error("pricing file not readable: {path}: {reason}")]
    NotReadable { path: PathBuf, reason: String },

    #[error("pricing file is not valid TOML: {0}")]
    InvalidToml(String),

    #[error("no price entry for ({provider}, {model})")]
    Missing {
        provider: ProviderKind,
        model: String,
    },
}

/* --------------------------------------------------------------------- */
/* Provider errors (unified across Ollama/OpenAI/...)                    */
/* --------------------------------------------------------------------- */

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Provider {
    Ollama,
    OpenAI,
    Bedrock,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Provider::Ollama => f.write_str("Ollama"),
            Provider::OpenAI => f.write_str("OpenAI"),
            Provider::Bedrock => f.write_str("Bedrock"),
        }
    }
}

#[derive(Debug, Error)]
pub enum ProviderErrorKind {
    #[error("invalid provider for this operation")]
    InvalidProvider,

    #[error("missing API key")]
    MissingApiKey,

    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),

    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("{0}")]
    HttpStatus(#[from] HttpError),

    #[error("decode error: {0}")]
    Decode(String),

    #[error("empty response: no content returned")]
    EmptyChoices,
}

#[derive(Debug, Error)]
#[error("{provider}: {kind}")]
pub struct ProviderError {
    pub provider: Provider,
    #[source]
    pub kind: ProviderErrorKind,
}

impl ProviderError {
    #[inline]
    pub fn new(provider: Provider, kind: ProviderErrorKind) -> Self {
        Self { provider, kind }
    }
}

/* --------------------------------------------------------------------- */
/* Env helpers                                                           */
/* --------------------------------------------------------------------- */

/// Fetches a required, non-empty environment variable.
pub fn must_env(name: &'static str) -> Result<String, GatewayError> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(ConfigError::MissingVar(name).into()),
    }
}

/// Parses an optional `u32` from env (`Ok(None)` if unset/empty).
pub fn env_opt_u32(name: &'static str) -> Result<Option<u32>, GatewayError> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v.parse::<u32>().map(Some).map_err(|_| {
            GatewayError::from(ConfigError::InvalidNumber {
                var: name,
                reason: "expected u32",
            })
        }),
        _ => Ok(None),
    }
}

/// Parses an optional `f32` from env.
pub fn env_opt_f32(name: &'static str) -> Result<Option<f32>, GatewayError> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v.parse::<f32>().map(Some).map_err(|_| {
            GatewayError::from(ConfigError::InvalidNumber {
                var: name,
                reason: "expected f32",
            })
        }),
        _ => Ok(None),
    }
}

/// Parses an optional `u64` from env.
pub fn env_opt_u64(name: &'static str) -> Result<Option<u64>, GatewayError> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v.parse::<u64>().map(Some).map_err(|_| {
            GatewayError::from(ConfigError::InvalidNumber {
                var: name,
                reason: "expected u64",
            })
        }),
        _ => Ok(None),
    }
}
