//! Configuration loader and validator.
//!
//! Responsibilities:
//! - Read environment variables or config file(s) to populate [`GraphConfig`]
//! - Apply defaults when values are missing
//! - Validate constraints (e.g., max_file_bytes must be > 0)
//!
//! Note: For now, only ENV loading is implemented; config file support can be added later.

pub mod model;

use crate::config::model::GraphConfig;
use anyhow::{Result, anyhow};
use std::path::Path;

/// Load [`GraphConfig`] from ENV variables, falling back to defaults if not set.
/// This is the main entry for the pipeline to obtain its configuration.
///
/// # Arguments
/// * `root` - canonical path to the repository root (may be used for resolving config file path).
pub fn load_from_env_or_default(_root: &Path) -> Result<GraphConfig> {
    // In the future:
    // 1) Look for `.graphconfig.yml` or similar in `root`
    // 2) If not found, read ENV vars
    // 3) Merge with defaults
    //
    // For now — just return defaults
    let cfg = GraphConfig::default();

    validate(&cfg)?;
    Ok(cfg)
}

/// Basic config validation — ensures limits and options are consistent.
/// Returns an error if validation fails.
fn validate(cfg: &GraphConfig) -> Result<()> {
    if cfg.limits.max_file_bytes == 0 {
        return Err(anyhow!("max_file_bytes must be greater than 0"));
    }
    if cfg.limits.snippet_context_lines > 50 {
        return Err(anyhow!(
            "snippet_context_lines is too large: {}",
            cfg.limits.snippet_context_lines
        ));
    }
    Ok(())
}
