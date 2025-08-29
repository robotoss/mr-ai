//! Core types used by the context layer.

/// Anchor range (inclusive, 1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorRange {
    pub start: usize,
    pub end: usize,
}

/// Primary per-target context packaged for prompting.
#[derive(Debug, Clone)]
pub struct PrimaryCtx {
    /// File path (repo-relative) or empty for global targets.
    pub path: String,
    /// Numbered snippet: HEAD file lines with absolute 1-based line numbers.
    pub numbered_snippet: String,
    /// Coarse allowed anchors derived from the target mapping (Line/Range/Symbol).
    pub allowed_anchors: Vec<AnchorRange>,
    /// Optional full-file read-only body for side checks (imports, symbol presence).
    pub full_file_readonly: Option<String>,
}
