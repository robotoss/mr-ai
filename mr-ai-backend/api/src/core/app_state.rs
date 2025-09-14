use std::{env, fmt, sync::Arc};

use ai_llm_service::service_profiles::LlmServiceProfiles;

/// Application configuration loaded from environment variables.
#[derive(Clone, Debug)]
pub struct AppConfig {
    /// Human-readable project name.
    pub project_name: String,
    /// Base URL for the Git service API (e.g. GitLab/GitHub/Gitea).
    pub git_api_base: String,
    /// Access token for the Git service API.
    pub git_token: String,
    /// Secret used to protect trigger endpoints.
    pub trigger_secret: String,
}

/// Errors that may occur while loading configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// Required variable is missing or empty.
    MissingVar { name: &'static str },
    /// Variable is present but contains an invalid value.
    InvalidValue { name: &'static str, reason: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::MissingVar { name } => {
                write!(f, "missing required environment variable: {}", name)
            }
            ConfigError::InvalidValue { name, reason } => {
                write!(f, "invalid value for {}: {}", name, reason)
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl AppConfig {
    /// Load configuration strictly from environment variables.
    pub fn from_env() -> Result<Self, ConfigError> {
        fn must_var(name: &'static str) -> Result<String, ConfigError> {
            let v = env::var(name).map_err(|_| ConfigError::MissingVar { name })?;
            if v.trim().is_empty() {
                return Err(ConfigError::MissingVar { name });
            }
            Ok(v)
        }

        let project_name = must_var("PROJECT_NAME")?;
        let git_api_base = must_var("GIT_API_BASE")?;
        let git_token = must_var("GIT_TOKEN")?;
        let trigger_secret = must_var("TRIGGER_SECRET")?;

        if !(git_api_base.starts_with("http://") || git_api_base.starts_with("https://")) {
            return Err(ConfigError::InvalidValue {
                name: "GIT_API_BASE",
                reason: "expected http(s) URL".into(),
            });
        }

        Ok(Self {
            project_name,
            git_api_base,
            git_token,
            trigger_secret,
        })
    }
}

/// Shared application state for all HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    /// Immutable configuration.
    pub config: Arc<AppConfig>,
    /// LLM service profiles (e.g. Ollama).
    pub llm_profiles: Arc<LlmServiceProfiles>,
}

impl AppState {
    /// Create state from pre-loaded configuration.
    pub fn new(config: Arc<AppConfig>, llm_profiles: Arc<LlmServiceProfiles>) -> Self {
        Self {
            config,
            llm_profiles,
        }
    }

    /// Convenience constructor: load config from ENV and return state.
    pub fn try_from_env(llm_profiles: Arc<LlmServiceProfiles>) -> Result<Self, ConfigError> {
        let config = Arc::new(AppConfig::from_env()?);
        Ok(Self::new(config, llm_profiles))
    }
}
