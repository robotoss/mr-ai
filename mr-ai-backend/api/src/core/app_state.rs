use std::{env, fmt, sync::Arc};

use ai_llm_service::LlmGateway;
use rag_base::structs::rag_base_config::RagConfig;
use secrets::SecretProvider;
use services::llm_health::LlmHealthMonitor;
use sqlx::PgPool;

/// Application configuration. Sprint C4 (🅲 multi-tenant) removed the
/// single-project invariant — `project_slug` and `default_project_id`
/// no longer live here; tenant identity is resolved per-request from
/// the `X-Project-Slug` header via
/// [`crate::middleware_layer::tenant::extract_tenant`].
#[derive(Clone, Debug)]
pub struct AppConfig {
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
    /// Read configuration from env. Pure env now that the project
    /// identity is per-request (sprint C4 of 🅲).
    pub fn from_env() -> Result<Self, ConfigError> {
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

        Ok(Self {
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
    /// Immutable RAG / Qdrant configuration captured at boot. The
    /// `/retrieve` hot path reads it on every request, and re-reading
    /// ~14 env vars per call burns CPU + read-locks. Cache once here.
    pub rag_cfg: Arc<RagConfig>,
    /// Prometheus exposition handle. `/metrics` renders from it. `None`
    /// in tests / when the recorder couldn't be installed (e.g. second
    /// install in a single process).
    pub metrics: Option<observability::MetricsHandle>,
    /// Cached snapshot served by `/health/dashboard`. `None` when
    /// persistence is disabled (the monitor needs a `PgPool`).
    pub dashboard: Option<services::dashboard_monitor::DashboardCache>,
}

impl AppState {
    /// Create state with full dependency wiring.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<AppConfig>,
        gateway: Arc<LlmGateway>,
        secrets: Arc<dyn SecretProvider>,
        db: Option<PgPool>,
        llm_health: LlmHealthMonitor,
        rag_cfg: Arc<RagConfig>,
        metrics: Option<observability::MetricsHandle>,
        dashboard: Option<services::dashboard_monitor::DashboardCache>,
    ) -> Self {
        Self {
            config,
            gateway,
            secrets,
            db,
            llm_health,
            rag_cfg,
            metrics,
            dashboard,
        }
    }
}
