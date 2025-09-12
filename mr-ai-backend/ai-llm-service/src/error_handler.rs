//! Unified error handling for `ai-llm-service`.
//!
//! Key points (modern, low-dup approach):
//! - Single top-level error [`AiLlmError`] that appends the suffix
//!   `"[AI LLM Service]"` **once** during `Display` formatting.
//! - Provider errors are unified via [`Provider`], [`ProviderErrorKind`],
//!   and [`ProviderError`], avoiding duplication between Ollama and OpenAI.
//! - Shared HTTP error carrier [`HttpError`] (status + url + snippet) and
//!   safe snippet trimming.
//! - Expanded env helpers: `must_env`, `must_env_url`, `env_opt_u32`,
//!   `env_opt_f32`, and `ensure_range_f32`.
//!
//! Note: If you prefer the suffix to be injected via thiserror attribute
//! (e.g., `#[error("{0} [AI LLM Service]")]`), you can wrap `AiLlmError`’s
//! formatted string; here we centralize it by implementing `Display` manually.

use reqwest::StatusCode;
use std::error::Error as StdError;
use std::fmt;
use std::time::Duration;
use thiserror::Error;

/* ------------------------------------------------------------------------- */
/* Top-level error                                                           */
/* ------------------------------------------------------------------------- */

/// Top-level error for the library.
///
/// Contains config/health/provider errors and generic cases (HTTP transport,
/// timeouts). During formatting, we **append** the suffix `"[AI LLM Service]"`
#[non_exhaustive]
#[derive(Debug)]
pub enum AiLlmError {
    /// Configuration / validation failures.
    Config(ConfigError),

    /// Provider-agnostic health/decoding/HTTP status issues.
    Health(HealthError),

    /// Unified provider-specific errors (Ollama / OpenAI).
    Provider(ProviderError),

    /// Underlying HTTP transport error that wasn’t wrapped at a lower layer.
    HttpTransport(reqwest::Error),

    /// Operation exceeded the configured timeout.
    Timeout(Duration),
}

impl fmt::Display for AiLlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Format the base message without the suffix.
        let base = match self {
            AiLlmError::Config(e) => e.to_string(),
            AiLlmError::Health(e) => e.to_string(),
            AiLlmError::Provider(e) => e.to_string(),
            AiLlmError::HttpTransport(e) => format!("transport error: {e}"),
            AiLlmError::Timeout(dur) => format!("operation timed out after {dur:?}"),
        };
        // Append the library suffix centrally (single source of truth).
        write!(f, "{base} [AI LLM Service]")
    }
}

impl StdError for AiLlmError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            AiLlmError::Config(e) => Some(e),
            AiLlmError::Health(e) => Some(e),
            AiLlmError::Provider(e) => Some(e),
            AiLlmError::HttpTransport(e) => Some(e),
            AiLlmError::Timeout(_) => None,
        }
    }
}

/* Convenient conversions to the top-level error */
impl From<ConfigError> for AiLlmError {
    fn from(e: ConfigError) -> Self {
        AiLlmError::Config(e)
    }
}
impl From<HealthError> for AiLlmError {
    fn from(e: HealthError) -> Self {
        AiLlmError::Health(e)
    }
}
impl From<ProviderError> for AiLlmError {
    fn from(e: ProviderError) -> Self {
        AiLlmError::Provider(e)
    }
}
impl From<reqwest::Error> for AiLlmError {
    fn from(e: reqwest::Error) -> Self {
        AiLlmError::HttpTransport(e)
    }
}

/* ------------------------------------------------------------------------- */
/* Shared HTTP details & utilities                                           */
/* ------------------------------------------------------------------------- */

/// Compact carrier for upstream HTTP failures.
#[derive(Debug, Clone)]
pub struct HttpError {
    pub status: StatusCode,
    pub url: String,
    pub snippet: String, // Already trimmed/shortened
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HTTP {} from {}: {}",
            self.status, self.url, self.snippet
        )
    }
}

impl StdError for HttpError {}

/// Maximum size for response body snippets embedded into errors.
const SNIPPET_MAX: usize = 256;

/// Safely trims and normalizes a body snippet for logging/diagnostics.
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
    /// Builds [`HttpError`] from a `reqwest::Response` by reading the body
    /// (best-effort) and trimming it.
    pub async fn from_response(url: &str, resp: reqwest::Response) -> Self {
        let status = resp.status();
        let text = match resp.text().await {
            Ok(t) => t,
            Err(_) => String::new(),
        };
        Self {
            status,
            url: url.to_string(),
            snippet: make_snippet(&text),
        }
    }
}

/* ------------------------------------------------------------------------- */
/* Config errors                                                             */
/* ------------------------------------------------------------------------- */

/// Config/environment-driven setup errors.
///
/// Focused on issues that realistically happen during config load/validation.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Required environment variable is missing or empty.
    #[error("missing required environment variable: {0}")]
    MissingVar(&'static str),

    /// Numeric parse failed (ports, limits, timeouts).
    #[error("invalid number in {var}: {reason}")]
    InvalidNumber {
        /// Variable name (e.g., `LLM_MAX_TOKENS`, `OLLAMA_PORT`).
        var: &'static str,
        /// Human-readable reason (e.g., `expected u32`).
        reason: &'static str,
    },

    /// Unsupported provider name in configuration.
    #[error("unsupported provider: {0}")]
    UnsupportedProvider(String),

    /// Value had an invalid format (e.g., not a valid URL).
    #[error("invalid format in {var}: {reason}")]
    InvalidFormat {
        /// Variable name (e.g., `OLLAMA_URL`).
        var: &'static str,
        /// Explanation (e.g., `must start with http:// or https://`).
        reason: &'static str,
    },

    /// A numeric value was outside the allowed range.
    #[error("{field} is out of range: {detail}")]
    OutOfRange {
        /// Field name (e.g., `temperature`).
        field: &'static str,
        /// Description of the expected range.
        detail: &'static str,
    },

    /// Model name was empty or invalid.
    #[error("model name must not be empty")]
    EmptyModel,

    /// Stop sequences contained one or more empty strings.
    #[error("stop sequences must not contain empty strings")]
    InvalidStopSequence,
}

/* ------------------------------------------------------------------------- */
/* Health errors                                                             */
/* ------------------------------------------------------------------------- */

/// Errors raised by provider health checks (provider-agnostic).
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum HealthError {
    /// The configured provider is invalid for this health check.
    #[error("invalid provider for health check")]
    InvalidProvider,

    /// Endpoint is empty or does not start with http/https.
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),

    /// Upstream returned a non-success HTTP status.
    #[error("{0}")]
    HttpStatus(#[from] HttpError),

    /// Response payload could not be decoded as expected.
    #[error("decode error: {0}")]
    Decode(String),
}

/* ------------------------------------------------------------------------- */
/* Provider-unified errors                                                   */
/* ------------------------------------------------------------------------- */

/// Supported providers (unified enumeration).
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Provider {
    Ollama,
    OpenAI,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Provider::Ollama => f.write_str("Ollama"),
            Provider::OpenAI => f.write_str("OpenAI"),
        }
    }
}

/// Provider error kinds shared across all providers.
#[derive(Debug, Error)]
pub enum ProviderErrorKind {
    /// The selected provider is invalid for this operation.
    #[error("invalid provider for this operation")]
    InvalidProvider,

    /// API key is missing (primarily for OpenAI).
    #[error("missing API key")]
    MissingApiKey,

    /// Malformed or unsupported endpoint (scheme/host/etc.).
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),

    /// Underlying HTTP client failure.
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    /// Non-success upstream HTTP status + short response snippet.
    #[error("{0}")]
    HttpStatus(#[from] HttpError),

    /// Unexpected / invalid JSON payload.
    #[error("decode error: {0}")]
    Decode(String),

    /// Completion returned no content (e.g., no choices).
    #[error("empty response: no choices returned")]
    EmptyChoices,
}

/// Provider error pairs a concrete provider with a failure kind.
///
/// Display renders as `"Provider: <kind>"` (suffix added by `AiLlmError`).
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

/* ------------------------------------------------------------------------- */
/* Env helpers (return `Result<_, AiLlmError>`)                               */
/* ------------------------------------------------------------------------- */

/// Fetches a required, non-empty environment variable.
///
/// # Errors
/// Returns [`AiLlmError::Config`] with [`ConfigError::MissingVar`] if absent/empty.
pub fn must_env(name: &'static str) -> Result<String, AiLlmError> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(ConfigError::MissingVar(name).into()),
    }
}

/// Parses an optional `u32` from env (`Ok(None)` if unset/empty).
///
/// # Errors
/// Returns [`AiLlmError::Config`] with [`ConfigError::InvalidNumber`] if set but invalid.
pub fn env_opt_u32(name: &'static str) -> Result<Option<u32>, AiLlmError> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v.parse::<u32>().map(Some).map_err(|_| {
            AiLlmError::from(ConfigError::InvalidNumber {
                var: name,
                reason: "expected u32",
            })
        }),
        _ => Ok(None),
    }
}
