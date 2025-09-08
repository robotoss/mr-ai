//! Core types used by the context layer.

/// Anchor range (inclusive, 1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorRange {
    pub start: usize,
    pub end: usize,
}

/// Serializable reference to a chunk of a parent entity (used by chunk.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRef {
    /// 1-based index of this chunk.
    pub index: usize,
    /// Total number of chunks in the parent entity.
    pub total: usize,
    /// Stable identifier of the parent entity (e.g., AST symbol id).
    pub parent_id: String,
}

/// Descriptor of an enclosing symbol.
#[derive(Debug, Clone)]
pub struct EnclosingInfo {
    /// Human-friendly kind (e.g., "Class", "Function", "Method").
    pub kind: String,
    /// Symbol name.
    pub name: String,
    /// Enclosing start line (1-based, inclusive).
    pub start_line: usize,
    /// Enclosing end line (1-based, inclusive).
    pub end_line: usize,
}

/// Metadata and text for a focused chunk inside the enclosing scope.
#[derive(Debug, Clone)]
pub struct ChunkInfo {
    /// 1-based chunk index inside the enclosing scope.
    pub index: usize,
    /// Total number of chunks in the enclosing scope.
    pub total: usize,
    /// Chunk start line (1-based, inclusive).
    pub from: usize,
    /// Chunk end line (1-based, inclusive).
    pub to: usize,
    /// Chunk snippet text.
    pub snippet: String,
}

/// Compact, language-agnostic facts near a target anchor.
#[derive(Debug, Clone)]
pub struct CodeFacts {
    /// Repository-relative file path.
    pub file: String,
    /// Anchor range that facts are built around.
    pub anchor: AnchorRange,
    /// Optional enclosing symbol information.
    pub enclosing: Option<EnclosingInfo>,
    /// Full enclosing snippet (entire body or a wide fallback window).
    pub enclosing_snippet: String,
    /// One focused chunk with {index/total} and precise line bounds.
    pub chunk: ChunkInfo,
    /// Top call identifiers in the enclosing snippet.
    pub calls_top: Vec<String>,
    /// Likely write targets (assignment LHS) in the enclosing snippet.
    pub writes: Vec<String>,
    /// Control-flow outline (e.g., `return`, `throw`) in the enclosing snippet.
    pub control_flow: Vec<String>,
    /// Cleanup-like signals (e.g., `dispose`, `close`) in the enclosing snippet.
    pub cleanup_like: Vec<String>,
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
    /// Structured code facts near the anchor (HEAD authoritative).
    pub code_facts: Option<CodeFacts>,
}

/// Strict output spec injected into the prompt to enforce deterministic JSON.
pub const STRICT_OUTPUT_SPEC: &str = r#"
OUTPUT FORMAT (STRICT JSON):
- Emit JSON with either:
  { "NoIssues": true }
  OR
  { "comments": [ { "anchor": {"start":N,"end":M}, "severity": "...", "title": "...", "body": "...", "patch": "..." }, ... ] }
- Use severity "needs_context" when you must ask questions.
- Base every claim on the provided code; quote exact lines where possible.
- Do not mention files not present in the blocks.
- PRECEDENCE & GROUNDING:
  * PRIMARY and FULL FILE are HEAD (authoritative).
  * RELATED is BASE/external (non-authoritative).
  * On conflicts, trust HEAD.
- CHUNKING:
  * CodeFacts include FULL enclosing snippet and one CHUNK snippet with {index/total}.
  * Related blocks may include chunk meta; use only as extra context.
"#;
