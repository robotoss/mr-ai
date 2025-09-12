//! Error types and validation helpers for `ai-llm-service`.
//!
//! This module defines a focused set of configuration errors and small,
//! reusable helpers for reading/validating environment variables.
//!
//! All error messages include the suffix `[AI LLM Service]` so that logs
//! and bubbled-up errors can be easily attributed to this library.

use thiserror::Error;

/// Convenient result alias for config and runtime operations.
pub type Result<T> = std::result::Result<T, ConfigError>;

/// Error enum for environment/config-driven setup.
///
/// Keep this focused: only errors that realistically happen at config
/// load/validation time. Extend conservatively as needs arise.
///
/// All variants include `[AI LLM Service]` in their messages.
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

/* ------------------------ Env helpers ------------------------ */

/// Fetches a required, non-empty environment variable.
///
/// # Errors
/// Returns [`ConfigError::MissingVar`] if the variable is absent or empty.
pub fn must_env(name: &'static str) -> Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(ConfigError::MissingVar(name)),
    }
}

/// Parses an optional `u32` from env (`Ok(None)` if unset/empty).
///
/// # Errors
/// Returns [`ConfigError::InvalidNumber`] if the variable is set but not a valid `u32`.
pub fn env_opt_u32(name: &'static str) -> Result<Option<u32>> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => {
            v.parse::<u32>()
                .map(Some)
                .map_err(|_| ConfigError::InvalidNumber {
                    var: name,
                    reason: "expected u32",
                })
        }
        _ => Ok(None),
    }
}

/* --------------------- Validation helpers --------------------- */

/// Validates that an HTTP endpoint starts with `http://` or `https://`.
///
/// # Errors
/// Returns [`ConfigError::InvalidFormat`] when the string does not start
/// with a valid HTTP scheme.
pub fn validate_http_endpoint(var: &'static str, value: &str) -> Result<()> {
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(ConfigError::InvalidFormat {
            var,
            reason: "must start with http:// or https://",
        })
    }
}

/// Validates that a floating-point value lies within an inclusive range.
///
/// Useful for parameters like `temperature` (e.g., `0.0..=2.0`) or
/// `top_p` (`0.0..=1.0`).
///
/// # Parameters
/// - `field`: Logical field name (e.g., `"top_p"`).
/// - `value`: The value to validate.
/// - `min`: Inclusive lower bound.
/// - `max`: Inclusive upper bound.
///
/// # Errors
/// Returns [`ConfigError::OutOfRange`] if `value` is outside `[min, max]`.
pub fn validate_range_f32(field: &'static str, value: f32, min: f32, max: f32) -> Result<()> {
    if value.is_finite() && value >= min && value <= max {
        Ok(())
    } else {
        Err(ConfigError::OutOfRange {
            field,
            detail: "expected value in inclusive range",
        })
    }
}

/// Ensures that a list of stop sequences is non-empty and contains no empty strings.
///
/// # Errors
/// Returns [`ConfigError::InvalidStopSequence`] if any element is an empty string.
pub fn validate_stop_sequences<S>(stops: &[S]) -> Result<()>
where
    S: AsRef<str>,
{
    if stops.iter().any(|s| s.as_ref().is_empty()) {
        Err(ConfigError::InvalidStopSequence)
    } else {
        Ok(())
    }
}
