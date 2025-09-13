use std::sync::Arc;

use ai_llm_service::service_profiles::LlmServiceProfiles;

/// Shared state for all HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    /// API base for GitLab, e.g. "https://gitlab.com/api/v4"
    pub gitlab_api_base: String,
    /// Token for GitLab API ("PRIVATE-TOKEN" PAT or project access token).
    pub gitlab_token: String,
    /// Shared secret to protect the trigger endpoint from random callers.
    pub trigger_secret: String,
    /// Configuration for the LLM used(e.g., Ollama).
    pub svc: Arc<LlmServiceProfiles>,
}

impl AppState {
    /// Load shared state from environment variables.
    pub fn new(svc: Arc<LlmServiceProfiles>) -> Self {
        Self {
            gitlab_api_base: std::env::var("GITLAB_API_BASE")
                .unwrap_or_else(|_| "https://gitlab.com/api/v4".into()),
            gitlab_token: std::env::var("GITLAB_TOKEN").expect("GITLAB_TOKEN is required"),
            trigger_secret: std::env::var("TRIGGER_SECRET")
                .unwrap_or_else(|_| "super-secret".into()),

            svc: svc,
        }
    }
}
