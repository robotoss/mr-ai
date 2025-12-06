use ai_llm_service::error_handler::AiLlmError;
use thiserror::Error;

use crate::publish::GitProviderKind;

/// Single error type for the review engine.
///
/// This enum aggregates errors coming from the LLM service as well as
/// validation and publishing failures.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AiReviewEngineError {
    /// Errors coming from the underlying AI LLM service.
    ///
    /// `AiLlmError` already implements Display and appends "[AI LLM Service]".
    #[error("AI review generation failed: {0}")]
    Ai(#[from] AiLlmError),

    /// Review request is invalid (for example, empty targets).
    #[error("invalid review request: {0}")]
    InvalidRequest(String),

    /// Comment publishing failed in one of the supported Git providers.
    #[error("comment publishing failed: {0}")]
    Publish(#[from] MrPublishError),
}

/// Unified error type for MR/PR comment publishing.
///
/// This error is focused only on the "pushing comments to Git provider"
/// concerns and can be reused in different layers of the application.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum MrPublishError {
    /// Invalid or inconsistent publish request detected before network calls.
    #[error("invalid publish request: {0}")]
    InvalidRequest(String),

    /// Provider configuration error (token, base URL, headers).
    #[error("invalid provider configuration: {0}")]
    InvalidConfig(String),

    /// Required data missing for a specific provider.
    ///
    /// Example: missing `head_sha` for GitHub inline comments.
    #[error("missing required data: {0}")]
    MissingData(String),

    /// HTTP client error (connection, timeout, DNS, serialization and so on).
    #[error("http client error: {0}")]
    Http(#[from] reqwest::Error),

    /// Provider API returned a non-success status code.
    #[error("provider {provider} API error: status={status}, body={body}")]
    Provider {
        /// Provider kind (GitLab, GitHub, GitBucket).
        provider: GitProviderKind,
        /// Numeric HTTP status code returned by the provider.
        status: u16,
        /// Response body (possibly truncated to prevent log noise).
        body: String,
    },
}

impl AiReviewEngineError {
    /// Convenience helper to expose human-readable error message.
    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl MrPublishError {
    /// Convenience helper to expose human-readable error message.
    pub fn message(&self) -> String {
        self.to_string()
    }

    /// Build a provider error from status and body.
    pub fn provider_error(provider: GitProviderKind, status: u16, body: String) -> Self {
        MrPublishError::Provider {
            provider,
            status,
            body,
        }
    }

    /// Build a provider error by consuming `reqwest::Response` and reading body.
    ///
    /// Body is trimmed and truncated, so it is safe to log and propagate.
    pub async fn from_response(
        provider: GitProviderKind,
        resp: reqwest::Response,
    ) -> MrPublishError {
        let status = resp.status().as_u16();
        let body = match resp.text().await {
            Ok(text) => {
                let trimmed = text.trim();
                if trimmed.len() > 2048 {
                    format!("{}...", &trimmed[..2048])
                } else {
                    trimmed.to_string()
                }
            }
            Err(e) => {
                tracing::warn!("failed to read error body: {}", e);
                "<body unavailable>".to_string()
            }
        };

        tracing::warn!(
            "provider={} returned error status={} body={}",
            provider,
            status,
            body
        );

        MrPublishError::provider_error(provider, status, body)
    }
}
