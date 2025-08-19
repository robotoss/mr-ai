//! Configuration data structures for the AST/Graph → RAG preparation pipeline.
//!
//! Groups:
//! - [`GraphConfig`]   — top-level container for all config groups
//! - [`Filters`]       — which files/entities to include/exclude
//! - [`Limits`]        — size/time limits (files, AST nodes, chunk/snippet)
//! - [`ExtractConfig`] — enrichment options (docstrings, signatures, imports)
//! - [`FeatureFlags`]  — toggle optional features (calls graph, inheritance, etc.)
//!
//! All structs are `serde`-friendly so they can be loaded from YAML/JSON.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Top-level configuration for the pipeline.
///
/// Wraps all sub-configs and provides validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphConfig {
    /// Which files/entities to include/exclude.
    pub filters: Filters,
    /// Size/time limits (files, AST nodes, chunk/snippet).
    pub limits: Limits,
    /// Extraction-specific enrichment settings.
    pub extract: ExtractConfig,
    /// Optional graph feature toggles.
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
    /// Validate config sanity (no degenerate or absurd values).
    pub fn validate(&self) -> Result<()> {
        if self.limits.max_file_bytes == 0 {
            return Err(anyhow!("`max_file_bytes` must be greater than 0"));
        }
        if self.limits.snippet_context_lines > 50 {
            return Err(anyhow!(
                "`snippet_context_lines` too large: {}",
                self.limits.snippet_context_lines
            ));
        }
        if self.limits.max_chunk_lines == 0 {
            return Err(anyhow!("`max_chunk_lines` must be greater than 0"));
        }
        if self.limits.max_chunk_chars == 0 {
            return Err(anyhow!("`max_chunk_chars` must be greater than 0"));
        }
        Ok(())
    }
}

/// File/entity filtering rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Filters {
    /// Whether to exclude generated files entirely.
    pub exclude_generated: bool,
    /// Glob patterns for generated files.
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
                "**/.git/**".into(),
                "**/node_modules/**".into(),
                "**/build/**".into(),
                "**/target/**".into(),
            ],
        }
    }
}

/// Limits for scanning, parsing, and chunking.
///
/// These caps are designed to protect performance and keep RAG-friendly
/// context windows (so embedding/vector DB queries stay within limits).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum file size to parse (bytes).
    pub max_file_bytes: usize,
    /// Context lines to include around a snippet chunk.
    pub snippet_context_lines: usize,
    /// Maximum number of AST nodes to process (0 = unlimited).
    pub max_ast_nodes: usize,
    /// Maximum number of lines allowed in one chunk (e.g. 200).
    pub max_chunk_lines: usize,
    /// Maximum number of characters allowed in one chunk (e.g. 4000).
    pub max_chunk_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 2 * 1024 * 1024, // 2 MB
            snippet_context_lines: 4,
            max_ast_nodes: 0,
            max_chunk_lines: 200,
            max_chunk_chars: 4000, // ~2k-3k tokens depending on code density
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
