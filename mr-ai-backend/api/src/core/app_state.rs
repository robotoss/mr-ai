use mr_reviewer::review::llm::{LlmConfig, LlmKind};

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
    pub llm_config: LlmConfig,
}

impl AppState {
    /// Load shared state from environment variables.
    pub fn from_env() -> Self {
        Self {
            gitlab_api_base: std::env::var("GITLAB_API_BASE")
                .unwrap_or_else(|_| "https://gitlab.com/api/v4".into()),
            gitlab_token: std::env::var("GITLAB_TOKEN").expect("GITLAB_TOKEN is required"),
            trigger_secret: std::env::var("TRIGGER_SECRET")
                .unwrap_or_else(|_| "super-secret".into()),

            llm_config: LlmConfig {
                kind: LlmKind::Ollama,
                model: std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:32b".into()),
                // Prefer explicit OLLAMA_URL, fallback to localhost:OLLAMA_PORT
                endpoint: std::env::var("OLLAMA_URL").unwrap_or_else(|_| {
                    let port = std::env::var("OLLAMA_PORT").unwrap_or_else(|_| "11434".into());
                    format!("http://localhost:{port}")
                }),
                max_tokens: None,
            },
        }
    }
}
