//! Error types for `ai-llm-service` configuration and runtime.

use thiserror::Error;

/// Convenient result alias for config and runtime operations.
pub type Result<T> = std::result::Result<T, ConfigError>;

/// Minimal error enum for environment/config-driven setup.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Required environment variable is missing or empty.
    #[error("missing required environment variable: {0}")]
    MissingVar(&'static str),

    /// A number failed to parse (like ports, limits, timeouts).
    #[error("invalid number in {var}: {reason}")]
    InvalidNumber {
        /// Variable name (e.g., `LLM_MAX_TOKENS`, `OLLAMA_PORT`).
        var: &'static str,
        /// Human-readable reason (e.g., `expected u32`).
        reason: &'static str,
    },

    /// Unsupported provider in `LLM_KIND`.
    #[error("unsupported provider: {0}")]
    UnsupportedProvider(String),
}

/// Fetches a required, non-empty environment variable.
pub fn must_env(name: &'static str) -> Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(ConfigError::MissingVar(name)),
    }
}

/// Parses an optional `u32` from env (`Ok(None)` if unset/empty).
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
