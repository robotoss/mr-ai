//! Crate-wide error hierarchy for mr-reviewer.
//!
//! Goals:
//! - Single root `Error` for all public functions.
//! - Provider-aware mapping (401→Unauthorized, 429→RateLimited, 5xx→Server, etc.).
//! - No dynamic dispatch, no async-trait, ergonomic `?` via `From` impls.

use thiserror::Error;

/// Convenient alias for crate-wide results.
pub type MrResult<T> = Result<T, Error>;

/// Root error type for the mr-reviewer crate.
#[derive(Debug, Error)]
pub enum Error {
    /// Provider (GitLab/GitHub/Bitbucket) related failure.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// Cache (file I/O / JSON) failure.
    #[error(transparent)]
    Cache(#[from] CacheError),

    /// Unified diff parsing failure.
    #[error(transparent)]
    Parse(#[from] ParseError),

    /// Configuration problems (bad/missing tokens, base URL, etc.).
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// Input validation errors (bad IDs, unsupported flavors, etc.).
    #[error("validation error: {0}")]
    Validation(String),

    /// Generic catch-all error when nothing else fits.
    #[error("other error: {0}")]
    Other(String),
}

/// Detailed provider-specific error used inside the Provider layer.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// Unauthorized (HTTP 401).
    #[error("unauthorized")]
    Unauthorized,

    /// Forbidden (HTTP 403).
    #[error("forbidden")]
    Forbidden,

    /// Not found (HTTP 404).
    #[error("not found")]
    NotFound,

    /// Rate limited (HTTP 429).
    #[error("rate limited")]
    RateLimited { retry_after_secs: Option<u64> },

    /// Gateway/Server error (HTTP 5xx).
    #[error("server error: status {0}")]
    Server(u16),

    /// Other HTTP status (4xx/3xx) not covered above.
    #[error("http status error: {0}")]
    HttpStatus(u16),

    /// Timeout at transport level.
    #[error("timeout")]
    Timeout,

    /// Network/transport failure without status (DNS/connect/reset).
    #[error("network error: {0}")]
    Network(String),

    /// JSON deserialization error.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Unexpected/invalid shape of provider response.
    #[error("invalid response: {0}")]
    InvalidResponse(String),

    /// Operation not supported by provider (placeholder for TODOs).
    #[error("unsupported provider operation")]
    Unsupported,
}

/// File cache related errors.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Unified diff parser errors.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid hunk header: {0}")]
    InvalidHunkHeader(String),

    #[error("unexpected end of input")]
    UnexpectedEof,

    #[error("integer overflow")]
    Overflow,
}

/// Configuration and setup errors (base API URL, missing token, etc.).
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("missing provider token")]
    MissingToken,

    #[error("invalid base api url: {0}")]
    InvalidBaseUrl(String),
}

// ===== Conversions for `?` ergonomics =====

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Provider(ProviderError::from(e))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Cache(CacheError::Io(e))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        // We route JSON failures via Provider when they originate from HTTP payloads,
        // but at crate root it's fine to tag as Cache if not specified.
        // To keep consistent, prefer Provider::Serde in provider code; elsewhere Cache::Serde.
        Error::Cache(CacheError::Serde(e))
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return ProviderError::Timeout;
        }
        if let Some(status) = e.status() {
            let code = status.as_u16();
            return match code {
                401 => ProviderError::Unauthorized,
                403 => ProviderError::Forbidden,
                404 => ProviderError::NotFound,
                429 => ProviderError::RateLimited {
                    retry_after_secs: None,
                },
                500..=599 => ProviderError::Server(code),
                _ => ProviderError::HttpStatus(code),
            };
        }
        ProviderError::Network(e.to_string())
    }
}
