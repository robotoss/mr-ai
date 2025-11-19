//! Crate-wide error hierarchy for git-context-engine.

use thiserror::Error;

/// Convenient alias for crate-wide results.
pub type GitContextEngineResult<T> = Result<T, GitContextEngineError>;

/// Root error type for the git-context-engine crate.
#[derive(Debug, Error)]
pub enum GitContextEngineError {
    /// Provider (GitLab/GitHub/Bitbucket) related failure.
    #[error(transparent)]
    Provider(#[from] GitContextEngineProviderError),

    /// Cache (file I/O / JSON) failure.
    #[error(transparent)]
    Cache(#[from] GitContextEngineCacheError),

    /// Unified diff parsing failure.
    #[error(transparent)]
    DiffParse(#[from] GitContextEngineDiffParseError),

    /// Configuration problems (bad/missing tokens, base URL, etc.).
    #[error(transparent)]
    Config(#[from] GitContextEngineConfigError),

    /// Input validation errors (bad IDs, unsupported formats, etc.).
    #[error("validation error: {0}")]
    Validation(String),

    /// Generic catch-all error when nothing else fits.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Provider-specific error used inside the provider layer.
#[derive(Debug, Error)]
pub enum GitContextEngineProviderError {
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
    RateLimited {
        /// Optional `Retry-After` hint in seconds when available.
        retry_after_secs: Option<u64>,
    },

    /// Gateway / server error (HTTP 5xx).
    #[error("server error: status {0}")]
    Server(u16),

    /// Other HTTP status (non-2xx) not covered by specific variants.
    #[error("http status error: status {0}")]
    HttpStatus(u16),

    /// Timeout at transport level.
    #[error("timeout")]
    Timeout,

    /// Network/transport failure without HTTP status (DNS/connect/reset).
    #[error("network error: {0}")]
    Network(String),

    /// Unexpected/invalid shape of provider response.
    #[error("invalid provider response: {0}")]
    InvalidResponse(String),

    /// Operation is not yet implemented or not supported by this provider.
    #[error("unsupported provider operation")]
    Unsupported,
}

/// File cache related errors.
#[derive(Debug, Error)]
pub enum GitContextEngineCacheError {
    /// I/O error while reading or writing cache files.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error in cache payloads.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Unified diff parser errors.
#[derive(Debug, Error)]
pub enum GitContextEngineDiffParseError {
    /// Hunk header could not be parsed or had invalid counters.
    #[error("invalid hunk header: {0}")]
    InvalidHunkHeader(String),

    /// Unexpected end of input while parsing a diff.
    #[error("unexpected end of input")]
    UnexpectedEof,

    /// Integer overflow while computing line ranges.
    #[error("integer overflow")]
    Overflow,
}

/// Configuration and setup errors (base API URL, missing token, etc.).
#[derive(Debug, Error)]
pub enum GitContextEngineConfigError {
    /// Missing required provider access token.
    #[error("missing provider token")]
    MissingToken,

    /// Invalid base API URL.
    #[error("invalid base api url: {0}")]
    InvalidBaseUrl(String),
}

// ===== Conversions for `?` ergonomics at the crate root =====

impl From<reqwest::Error> for GitContextEngineError {
    fn from(e: reqwest::Error) -> Self {
        GitContextEngineError::Provider(GitContextEngineProviderError::from(e))
    }
}

impl From<std::io::Error> for GitContextEngineError {
    fn from(e: std::io::Error) -> Self {
        GitContextEngineError::Cache(GitContextEngineCacheError::Io(e))
    }
}

impl From<serde_json::Error> for GitContextEngineError {
    fn from(e: serde_json::Error) -> Self {
        GitContextEngineError::Cache(GitContextEngineCacheError::Serde(e))
    }
}

// ===== Mapping from reqwest::Error into GitContextEngineProviderError =====

impl From<reqwest::Error> for GitContextEngineProviderError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return GitContextEngineProviderError::Timeout;
        }

        if let Some(status) = e.status() {
            let code = status.as_u16();
            return match code {
                401 => GitContextEngineProviderError::Unauthorized,
                403 => GitContextEngineProviderError::Forbidden,
                404 => GitContextEngineProviderError::NotFound,
                429 => GitContextEngineProviderError::RateLimited {
                    retry_after_secs: None,
                },
                500..=599 => GitContextEngineProviderError::Server(code),
                _ => GitContextEngineProviderError::HttpStatus(code),
            };
        }

        GitContextEngineProviderError::Network(e.to_string())
    }
}
