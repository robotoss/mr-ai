//! Configuration data structures for the AST/Graph preparation pipeline.
//!
//! These are split into logical groups for easier maintenance:
//! - [`GraphConfig`]: top-level container for all config groups
//! - [`ExtractConfig`]: options for AST extraction/enrichment
//! - [`Filters`]: which files/entities to include/exclude
//! - [`Limits`]: size/time limits
//! - [`FeatureFlags`]: toggle optional features (calls graph, inheritance graph, etc.)
//!
//! All structs are `serde`-friendly so they can be loaded from YAML/JSON.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::{env, path::Path};

/// Top-level configuration for the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphConfig {
    /// Which files/entities to include/exclude.
    pub filters: Filters,
    /// Size/time limits.
    pub limits: Limits,
    /// Extraction-specific settings.
    pub extract: ExtractConfig,
    /// Feature toggles.
    pub features: FeatureFlags,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            filters: Filters::default(),
            limits: Limits::default(),
            extract: ExtractConfig::default(),
            features: FeatureFlags::default(),
        }
    }
}

impl GraphConfig {
    /// Load configuration from environment variables or fallback to defaults.
    ///
    /// This method is intentionally tolerant: unknown variables are ignored,
    /// and parsing errors fall back to defaults. After load, a basic validation
    /// is performed to ensure sane values.
    ///
    /// Supported ENV vars (all optional):
    /// - `GRAPH_MAX_FILE_BYTES`                (usize)
    /// - `GRAPH_SNIPPET_CONTEXT_LINES`         (usize)
    /// - `GRAPH_MAX_AST_NODES`                 (usize)
    /// - `GRAPH_EXCLUDE_GENERATED`             (bool: true/false/1/0)
    /// - `GRAPH_GENERATED_GLOBS`               (comma-separated)
    /// - `GRAPH_IGNORE_GLOBS`                  (comma-separated)
    /// - `GRAPH_EXTRACT_DOCSTRINGS`            (bool)
    /// - `GRAPH_EXTRACT_SIGNATURES`            (bool)
    /// - `GRAPH_RESOLVE_IMPORTS`               (bool)
    /// - `GRAPH_FEATURE_CALLS`                 (bool)
    /// - `GRAPH_FEATURE_INHERITANCE`           (bool)
    pub fn load_from_env_or_default(_root: &Path) -> Result<Self> {
        let mut cfg = Self::default();

        // Limits
        if let Some(v) = env_usize("GRAPH_MAX_FILE_BYTES") {
            cfg.limits.max_file_bytes = v;
        }
        if let Some(v) = env_usize("GRAPH_SNIPPET_CONTEXT_LINES") {
            cfg.limits.snippet_context_lines = v;
        }
        if let Some(v) = env_usize("GRAPH_MAX_AST_NODES") {
            cfg.limits.max_ast_nodes = v;
        }

        // Filters
        if let Some(v) = env_bool("GRAPH_EXCLUDE_GENERATED") {
            cfg.filters.exclude_generated = v;
        }
        if let Some(v) = env_list("GRAPH_GENERATED_GLOBS") {
            cfg.filters.generated_globs = v;
        }
        if let Some(v) = env_list("GRAPH_IGNORE_GLOBS") {
            cfg.filters.ignore_globs = v;
        }

        // Extract
        if let Some(v) = env_bool("GRAPH_EXTRACT_DOCSTRINGS") {
            cfg.extract.extract_docstrings = v;
        }
        if let Some(v) = env_bool("GRAPH_EXTRACT_SIGNATURES") {
            cfg.extract.extract_signatures = v;
        }
        if let Some(v) = env_bool("GRAPH_RESOLVE_IMPORTS") {
            cfg.extract.resolve_imports = v;
        }

        // Features
        if let Some(v) = env_bool("GRAPH_FEATURE_CALLS") {
            cfg.features.build_calls_graph = v;
        }
        if let Some(v) = env_bool("GRAPH_FEATURE_INHERITANCE") {
            cfg.features.build_inheritance_graph = v;
        }

        cfg.validate()?;
        Ok(cfg)
    }

    /// Basic config validation — ensures limits and options are consistent.
    pub fn validate(&self) -> Result<()> {
        if self.limits.max_file_bytes == 0 {
            return Err(anyhow!("max_file_bytes must be greater than 0"));
        }
        if self.limits.snippet_context_lines > 50 {
            return Err(anyhow!(
                "snippet_context_lines is too large: {}",
                self.limits.snippet_context_lines
            ));
        }
        Ok(())
    }
}

/// File/entity filtering rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Filters {
    /// Whether to exclude generated files entirely.
    pub exclude_generated: bool,
    /// Glob patterns for generated files (comma-separated string or YAML array).
    pub generated_globs: Vec<String>,
    /// Glob patterns for files to ignore (in addition to generated).
    pub ignore_globs: Vec<String>,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            exclude_generated: true,
            generated_globs: vec![],
            ignore_globs: vec![
                String::from("**/.git/**"),
                String::from("**/node_modules/**"),
                String::from("**/build/**"),
                String::from("**/target/**"),
            ],
        }
    }
}

/// Limits for scanning, parsing, and chunking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum file size to parse (bytes).
    pub max_file_bytes: usize,
    /// Context lines to include around a snippet chunk.
    pub snippet_context_lines: usize,
    /// Maximum number of AST nodes to process (0 = unlimited).
    pub max_ast_nodes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 2 * 1024 * 1024, // 2 MB
            snippet_context_lines: 4,
            max_ast_nodes: 0,
        }
    }
}

/// Extraction configuration: controls how AST nodes are enriched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractConfig {
    /// Whether to extract docstrings/comments for entities.
    pub extract_docstrings: bool,
    /// Whether to extract function/method signatures.
    pub extract_signatures: bool,
    /// Whether to resolve import targets (cross-file linking).
    pub resolve_imports: bool,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            extract_docstrings: true,
            extract_signatures: true,
            resolve_imports: true,
        }
    }
}

/// Optional features for graph building and enrichment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlags {
    /// Whether to build call graphs (function call edges).
    pub build_calls_graph: bool,
    /// Whether to build inheritance graphs (class extends/implements).
    pub build_inheritance_graph: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            build_calls_graph: false,
            build_inheritance_graph: false,
        }
    }
}

/* ------------------------- ENV helpers ------------------------- */

fn env_bool(key: &str) -> Option<bool> {
    env::var(key).ok().and_then(|s| {
        let v = s.trim().to_ascii_lowercase();
        match v.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    })
}

fn env_usize(key: &str) -> Option<usize> {
    env::var(key)
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
}

fn env_list(key: &str) -> Option<Vec<String>> {
    let raw = env::var(key).ok()?;
    let list = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    Some(list)
}
