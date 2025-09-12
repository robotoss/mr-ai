//! Unified error handling for `ai-llm-service`.
//!
//! This module exposes a single top-level error type [`AiLlmError`] for the whole
//! library, and groups domain-specific errors in nested enums (e.g., [`ConfigError`],
//! [`HealthError`]). Small helpers for reading/validating environment variables are
//! provided and return the unified [`Result<T>`] alias.
//!
//! All messages include the suffix `[AI LLM Service]` to simplify attribution in logs.

use reqwest::StatusCode;
use std::time::Duration;
use thiserror::Error;

/* ------------------------------------------------------------------------- */
/* Public result alias                                                       */
/* ------------------------------------------------------------------------- */

/// Unified result alias for the entire crate.
pub type Result<T> = std::result::Result<T, AiLlmError>;

/* ------------------------------------------------------------------------- */
/* Top-level error                                                           */
/* ------------------------------------------------------------------------- */

/// Top-level error for the `ai-llm-service` crate.
///
/// Variants wrap domain-specific enums (config/health) and a few common cases
/// (e.g., HTTP transport, timeouts). Prefer adding new sub-enums for distinct
/// domains instead of growing this type indefinitely.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AiLlmError {
    /// Configuration/validation errors (startup/readiness).
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// Health-check/connectivity/decoding errors.
    #[error(transparent)]
    Health(#[from] HealthError),

    /// Underlying HTTP transport error (e.g., `reqwest::Error`).
    #[error("[AI LLM Service] transport error: {0}")]
    HttpTransport(#[from] reqwest::Error),

    /// Operation exceeded the configured timeout.
    #[error("[AI LLM Service] operation timed out after {0:?}")]
    Timeout(Duration),
}

/* ------------------------------------------------------------------------- */
/* Config errors                                                             */
/* ------------------------------------------------------------------------- */

/// Error enum for environment/config-driven setup.
///
/// Keep this focused: only errors that realistically happen at config
/// load/validation time.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Required environment variable is missing or empty.
    #[error("[AI LLM Service] missing required environment variable: {0}")]
    MissingVar(&'static str),

    /// A number failed to parse (like ports, limits, timeouts).
    #[error("[AI LLM Service] invalid number in {var}: {reason}")]
    InvalidNumber {
        /// Variable name (e.g., `LLM_MAX_TOKENS`, `OLLAMA_PORT`).
        var: &'static str,
        /// Human-readable reason (e.g., `expected u32`).
        reason: &'static str,
    },

    /// Unsupported provider in `LLM_KIND`.
    #[error("[AI LLM Service] unsupported provider: {0}")]
    UnsupportedProvider(String),

    /// Value had the wrong format (e.g., invalid URL).
    #[error("[AI LLM Service] invalid format in {var}: {reason}")]
    InvalidFormat {
        /// Variable name (e.g., `OLLAMA_URL`).
        var: &'static str,
        /// Explanation (e.g., `must start with http:// or https://`).
        reason: &'static str,
    },

    /// A numeric field was outside of the allowed range.
    #[error("[AI LLM Service] {field} is out of range: {detail}")]
    OutOfRange {
        /// Field name (e.g., `temperature`).
        field: &'static str,
        /// Description of the expected range (e.g., `expected 0.0..=1.0`).
        detail: &'static str,
    },

    /// Model name was empty or invalid.
    #[error("[AI LLM Service] model name must not be empty")]
    EmptyModel,

    /// Stop sequences contained one or more empty strings.
    #[error("[AI LLM Service] stop sequences must not contain empty strings")]
    InvalidStopSequence,
}

/* ------------------------------------------------------------------------- */
/* Health errors                                                             */
/* ------------------------------------------------------------------------- */

/// Error enum for provider health checks.
///
/// Used by health services to represent connectivity, protocol, and decoding
/// problems in a provider-agnostic way.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum HealthError {
    /// The config has an unexpected provider for this check.
    #[error("[AI LLM Service] invalid provider for health check")]
    InvalidProvider,

    /// The endpoint is empty or does not start with http/https.
    #[error("[AI LLM Service] invalid endpoint: {0}")]
    InvalidEndpoint(String),

    /// Upstream returned a non-successful HTTP status.
    #[error("[AI LLM Service] HTTP {status} from {url}: {snippet}")]
    HttpStatus {
        /// Numeric HTTP status code.
        status: StatusCode,
        /// Request URL.
        url: String,
        /// Optional short snippet of the response body (trimmed).
        snippet: String,
    },

    /// Response payload could not be decoded as expected.
    #[error("[AI LLM Service] decode error: {0}")]
    Decode(String),
}

/* ------------------------------------------------------------------------- */
/* Env helpers (return unified `Result<T>`)                                  */
/* ------------------------------------------------------------------------- */

/// Fetches a required, non-empty environment variable.
///
/// # Errors
/// Returns [`AiLlmError::Config`] with [`ConfigError::MissingVar`] if the
/// variable is absent or empty.
pub fn must_env(name: &'static str) -> Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(ConfigError::MissingVar(name).into()),
    }
}

/// Parses an optional `u32` from env (`Ok(None)` if unset/empty).
///
/// # Errors
/// Returns [`AiLlmError::Config`] with [`ConfigError::InvalidNumber`] if the
/// variable is set but not a valid `u32`.
pub fn env_opt_u32(name: &'static str) -> Result<Option<u32>> {
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

/* ------------------------------------------------------------------------- */
/* Validation helpers (return unified `Result<T>`)                           */
/* ------------------------------------------------------------------------- */

/// Validates that an HTTP endpoint starts with `http://` or `https://`.
///
/// # Errors
/// Returns [`AiLlmError::Config`] with [`ConfigError::InvalidFormat`] when
/// the string does not start with a valid HTTP scheme.
pub fn validate_http_endpoint(var: &'static str, value: &str) -> Result<()> {
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(ConfigError::InvalidFormat {
            var,
            reason: "must start with http:// or https://",
        }
        .into())
    }
}

/// Validates that a floating-point value lies within an inclusive range.
///
/// Useful for parameters like `temperature` (e.g., `0.0..=2.0`) or `top_p`
/// (`0.0..=1.0`).
///
/// # Parameters
/// - `field`: Logical field name (e.g., `"top_p"`).
/// - `value`: The value to validate.
/// - `min`: Inclusive lower bound.
/// - `max`: Inclusive upper bound.
///
/// # Errors
/// Returns [`AiLlmError::Config`] with [`ConfigError::OutOfRange`] if `value`
/// is outside `[min, max]`.
pub fn validate_range_f32(field: &'static str, value: f32, min: f32, max: f32) -> Result<()> {
    if value.is_finite() && value >= min && value <= max {
        Ok(())
    } else {
        Err(ConfigError::OutOfRange {
            field,
            detail: "expected value in inclusive range",
        }
        .into())
    }
}

/// Ensures that a list of stop sequences is non-empty and contains no empty strings.
///
/// # Errors
/// Returns [`AiLlmError::Config`] with [`ConfigError::InvalidStopSequence`] if
/// any element is an empty string.
pub fn validate_stop_sequences<S>(stops: &[S]) -> Result<()>
where
    S: AsRef<str>,
{
    if stops.iter().any(|s| s.as_ref().is_empty()) {
        Err(ConfigError::InvalidStopSequence.into())
    } else {
        Ok(())
    }
}
