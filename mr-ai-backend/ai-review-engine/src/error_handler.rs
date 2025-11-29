use ai_llm_service::error_handler::AiLlmError;
use thiserror::Error;

/// Single error type for the review engine.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AiReviewEngineError {
    /// Errors coming from the underlying AI LLM service.
    ///
    /// AiLlmError already implements Display and appends "[AI LLM Service]".
    #[error("AI review generation failed: {0}")]
    Ai(#[from] AiLlmError),

    /// Review request is invalid (e.g., empty targets).
    #[error("invalid review request: {0}")]
    InvalidRequest(String),
}

impl AiReviewEngineError {
    /// Convenience helper to get a human-readable error message.
    pub fn message(&self) -> String {
        self.to_string()
    }
}
