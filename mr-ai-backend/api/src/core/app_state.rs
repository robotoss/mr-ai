use std::{env, fmt, sync::Arc};

use ai_llm_service::LlmGateway;
use domain::ProjectId;
use secrets::SecretProvider;
use services::llm_health::LlmHealthMonitor;
use sqlx::PgPool;

/// Application configuration loaded from environment variables plus the
/// single-project invariant captured from `projects.toml` at boot.
#[derive(Clone, Debug)]
pub struct AppConfig {
    /// Slug of the one logical project this deployment serves. Sourced
    /// from `projects.toml` at boot (no env override); enforces the
    /// single-project invariant introduced in S5.
    pub project_slug: String,
    /// UUID of that project, cached so handlers can scope Qdrant
    /// filters and Postgres lookups without re-reading the config file.
    pub default_project_id: ProjectId,
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
    /// `projects.toml` declared zero or more-than-one projects, violating
    /// the single-project invariant introduced in S5.
    ExpectedExactlyOneProject { found: usize },
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
            ConfigError::ExpectedExactlyOneProject { found } => write!(
                f,
                "projects.toml must declare exactly one [[project]]; found {found}"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

impl AppConfig {
    /// Read the env-driven half of the config. The project identity
    /// half (`project_slug` + `default_project_id`) is filled in by
    /// [`AppConfig::with_project`] at boot, after `projects.toml` has
    /// been parsed.
    pub fn from_env_partial() -> Result<EnvAppConfig, ConfigError> {
        fn must_var(name: &'static str) -> Result<String, ConfigError> {
            let v = env::var(name).map_err(|_| ConfigError::MissingVar { name })?;
            if v.trim().is_empty() {
                return Err(ConfigError::MissingVar { name });
            }
            Ok(v)
        }

        let git_api_base = must_var("GIT_API_BASE")?;
        let git_token = must_var("GIT_TOKEN")?;
        let trigger_secret = must_var("TRIGGER_SECRET")?;

        if !(git_api_base.starts_with("http://") || git_api_base.starts_with("https://")) {
            return Err(ConfigError::InvalidValue {
                name: "GIT_API_BASE",
                reason: "expected http(s) URL".into(),
            });
        }

        Ok(EnvAppConfig {
            git_api_base,
            git_token,
            trigger_secret,
        })
    }

    /// Combine the env-derived parts with the project identity captured
    /// from `projects.toml` to produce the final [`AppConfig`].
    pub fn with_project(env: EnvAppConfig, project_slug: String, project_id: ProjectId) -> Self {
        Self {
            project_slug,
            default_project_id: project_id,
            git_api_base: env.git_api_base,
            git_token: env.git_token,
            trigger_secret: env.trigger_secret,
        }
    }
}

/// Env-only portion of [`AppConfig`]. Held briefly during boot before
/// the project identity is stitched in.
#[derive(Clone, Debug)]
pub struct EnvAppConfig {
    pub git_api_base: String,
    pub git_token: String,
    pub trigger_secret: String,
}

/// Shared application state for all HTTP handlers.
#[derive(Clone)]
pub struct AppState {
    /// Immutable configuration.
    pub config: Arc<AppConfig>,
    /// Universal LLM Gateway (provider-agnostic).
    pub gateway: Arc<LlmGateway>,
    /// Secret resolution backend (env or mounted files). Always present —
    /// defaults to `EnvSecretProvider` when nothing is configured.
    pub secrets: Arc<dyn SecretProvider>,
    /// Optional Postgres pool. `None` means persistence is disabled (legacy
    /// path, controlled by `DATABASE_OPTIONAL=true`). Handlers that need DB
    /// access must report a clean error when this is `None`.
    pub db: Option<PgPool>,
    /// Background-refreshed snapshot of LLM gateway health. Surfaced by
    /// `/health/detailed` and `/health/ready` so probes do not pay for an
    /// LLM round-trip on every tick.
    pub llm_health: LlmHealthMonitor,
}

impl AppState {
    /// Create state with full dependency wiring.
    pub fn new(
        config: Arc<AppConfig>,
        gateway: Arc<LlmGateway>,
        secrets: Arc<dyn SecretProvider>,
        db: Option<PgPool>,
        llm_health: LlmHealthMonitor,
    ) -> Self {
        Self {
            config,
            gateway,
            secrets,
            db,
            llm_health,
        }
    }
}
