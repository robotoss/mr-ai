//! Telemetry init facade. Real implementation lives in the
//! `observability` crate; this module exists so existing callers can
//! keep importing `ai_llm_service::init_tracing` without churn.

use observability::{init_telemetry, TelemetryConfig, TelemetryError, TelemetryGuard};

use crate::config::LogConfig;
use crate::errors::{GatewayError, HealthError};

/// Initialise the global tracing subscriber (pretty stdout + JSON
/// daily-rotated file). Returns a guard whose `Drop` flushes the file
/// appender — keep it alive for the lifetime of the process.
pub fn init_tracing(cfg: &LogConfig) -> Result<TelemetryGuard, GatewayError> {
    let obs_cfg = TelemetryConfig {
        log_dir: cfg.dir.clone(),
        log_file_prefix: cfg.file_prefix.clone(),
        log_level: cfg.level.clone(),
    };
    init_telemetry(&obs_cfg).map_err(|err| match err {
        TelemetryError::LogDir { path, source } => GatewayError::Health(HealthError::Decode(
            format!("failed to create log dir {path}: {source}"),
        )),
    })
}
