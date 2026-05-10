//! Logging / tracing configuration.

use std::path::PathBuf;

/// Settings for the dual-layer (stdout + JSON file rotation) tracing subscriber.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Directory where rotated JSON logs are written.
    pub dir: PathBuf,
    /// File-name prefix for rotated logs (e.g., `mr-ai`).
    pub file_prefix: String,
    /// `EnvFilter`-compatible level expression (e.g., `info`, `debug,reqwest=info`).
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("logs"),
            file_prefix: "mr-ai".to_string(),
            level: "info".to_string(),
        }
    }
}
