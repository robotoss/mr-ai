use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

//
// ──────────────────────────────────────────────────────────────────────────
//  Core enums
// ──────────────────────────────────────────────────────────────────────────
//

/// Language discriminator for chunks and files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageKind {
    Dart,
    Rust,
    Python,
    Javascript,
    Typescripts,
    Other,
}

/// Symbol kind taxonomy aligned with IDE/LSP expectations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Module,
    Import,
    Class,
    Interface,
    Enum,
    Mixin,
    Extension,
    Function,
    Method,
    Constructor,
    Field,
    Variable,
    Typedef,
    Unknown,
}

/// Absolute byte and (row,col) span inside the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

/// Lightweight file/symbol features useful for filtering and UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkFeatures {
    /// Raw UTF-8 byte length of snippet/code for this chunk.
    pub byte_len: usize,
    /// Raw line count of snippet/code for this chunk.
    pub line_count: usize,
    /// True when rustdoc/docstring is present directly on the symbol.
    pub has_doc: bool,
    /// True when language-level annotations/attributes are present.
    pub has_annotations: bool,
}

/// Neighbor links to navigate the symbol tree without re-parsing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Neighbors {
    pub parent_id: Option<String>,
    pub prev_id: Option<String>,
    pub next_id: Option<String>,
    pub children_ids: Vec<String>,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  Import / origin modeling
// ──────────────────────────────────────────────────────────────────────────
//

/// Classifies the origin of definitions and imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    /// Dart SDK library, e.g. "dart:async".
    Sdk,
    /// Third-party package, e.g. "package:go_router/...".
    Package,
    /// Local repository file (repo-relative path).
    Local,
    /// Could not be resolved or unknown scheme/format.
    Unknown,
}

/// A definition target with origin and an optional LSP-like range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefLocation {
    /// Classified origin kind: sdk | package | local | unknown.
    pub origin: OriginKind,
    /// Canonical target string (e.g., "dart:async", "package:foo/bar.dart", "lib/src/x.dart").
    pub target: String,
    /// Optional (start_line, start_col, end_line, end_col).
    pub range: Option<(usize, usize, usize, usize)>,
}

/// A specific identifier provided by an import (for ranking and explainability).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportUse {
    /// sdk | package | local | unknown.
    pub origin: OriginKind,
    /// e.g., "dart:async", "pkg:go_router", "file:lib/src/feature.dart".
    pub label: String,
    /// e.g., "Timer", "Future", "GoRoute", "Provider".
    pub identifier: String,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  LSP-derived metrics and semantics
// ──────────────────────────────────────────────────────────────────────────
//

/// Lightweight heuristics extracted from LSP/type info for ranking & filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SymbolMetrics {
    /// True if the symbol uses async constructs (Future/Stream/async/await/Timer).
    pub is_async: bool,
    /// True if a class is a Flutter widget (extends Widget/StatelessWidget/StatefulWidget).
    pub is_widget: bool,
    /// Approximate lines of code inside the symbol span (whitespace excluded).
    pub loc: Option<u32>,
    /// Number of parameters if trivially parsable from signature/hover.
    pub params_count: Option<u8>,
}

/// Normalized top token share derived from semantic tokens histogram.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticTopToken {
    /// Token name from the server legend (e.g., "class", "function", "variable").
    pub name: String,
    /// Ratio in [0.0, 1.0] among all tokens for this symbol or file.
    pub ratio: f32,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  Fine-grained anchors, graph edges, retrieval hints
// ──────────────────────────────────────────────────────────────────────────
//

/// Fine-grained anchor inside a snippet (for highlighting and precise slicing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    /// Anchor kind (e.g., "identifier", "parameter", "string", "call").
    pub kind: String,
    /// Byte range within the *file* (absolute), not snippet-local.
    pub start_byte: usize,
    pub end_byte: usize,
    /// Optional canonical name (e.g., the identifier text).
    pub name: Option<String>,
}

/// Cross-code relations and domain-specific facts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphEdges {
    /// Fully-qualified symbols called by this chunk.
    pub calls_out: Vec<String>,
    /// Qualified type names used (annotations, generics, extends/implements).
    pub uses_types: Vec<String>,
    /// Normalized imports touched by this chunk (sdk/package/local).
    pub imports_out: Vec<String>,
    /// Domain-specific facts (e.g., for Flutter: routes, guards, DI bindings).
    pub facts: BTreeMap<String, serde_json::Value>,
}

/// Flattened hints for hybrid retrieval (BM25 + dense).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetrievalHints {
    /// Bag of words from identifiers/imports for BM25.
    pub keywords: Vec<String>,
    /// Optional language-specific category (e.g., "flutter_widget").
    pub category: Option<String>,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  LSP enrichment
// ──────────────────────────────────────────────────────────────────────────
//

/// LSP enrichment attached to a chunk (hover, defs, refs, semantic tokens).
///
/// Backward compatibility:
/// - `definition_uri` and `flags` are legacy.
/// - Prefer `definition`/`definitions` and structured tags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LspEnrichment {
    /// One-line signature extracted from LSP/hover (legacy).
    pub signature_lsp: Option<String>,
    /// Total number of references to this symbol (if requested).
    pub references_count: Option<u32>,
    /// Legacy flat URI of the primary definition (kept for compatibility).
    /// Prefer `definition`.
    pub definition_uri: Option<String>,
    /// Primary definition target with origin classification.
    pub definition: Option<DefLocation>,
    /// All discovered definition targets (multi-target scenarios).
    pub definitions: Vec<DefLocation>,

    /// Per-file or per-symbol semantic token histogram (raw counts).
    pub semantic_token_hist: Option<BTreeMap<String, u32>>,
    /// Optional top-K normalized semantic tokens derived from the histogram.
    pub semantic_top_k: Vec<SemanticTopToken>,
    /// Optional code outline range (start_line, end_line) for quick snippet slicing.
    pub outline_code_range: Option<(usize, usize)>,

    /// Short type line and trimmed hover Markdown doc (if available).
    pub hover_type: Option<String>,
    pub hover_doc_md: Option<String>,

    /// Fully-qualified name derived from LSP parents (file + nesting).
    /// Example: "lib/app_routing.dart::AppRouting::config".
    pub fqn: Option<String>,
    /// Stable content-based symbol identifier (e.g., sha256 of file:line:signature head).
    pub stable_id: Option<String>,

    /// Collapsed import-driven usages for retrieval/ranking.
    pub imports_used: Vec<ImportUse>,
    /// Lightweight metrics for filters and ranking.
    pub metrics: Option<SymbolMetrics>,

    /// Freeform string flags (legacy; still populated by some providers).
    pub flags: Vec<String>,
    /// Freeform tags (e.g., "pkg:go_router", "sdk:dart:async", "kind:Class").
    pub tags: Vec<String>,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  Primary chunk and micro-chunk
// ──────────────────────────────────────────────────────────────────────────
//

/// Primary indexable unit for code RAG.
///
/// Design:
/// - Keep one record per *addressable* entity (class, function, method, etc.).
/// - Avoid micro-entities like single identifiers; store them as `identifiers`.
/// - Use `anchors` for precise highlighting instead of splitting the chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    /// Globally unique chunk ID (e.g., "<repo_rev>/<file>#<start_byte>-<end_byte>").
    pub id: String,
    /// Language of the source file.
    pub language: LanguageKind,

    /// Repo-relative file path (e.g., "packages/home_feature/lib/.../base_home_page.dart").
    pub file: String,
    /// Short symbol name (e.g., "BaseHomePage", "build", "_onItemTapped").
    pub symbol: String,
    /// Canonical symbol path within the file (e.g., "file::Class::Method").
    pub symbol_path: String,
    /// Symbol kind (Class/Method/Function/...).
    pub kind: SymbolKind,
    /// Absolute file span for the symbol.
    pub span: Span,
    /// Owner chain from outer to inner (e.g., ["BaseHomePage"] for a method).
    pub owner_path: Vec<String>,

    /// Optional documentation attached to the symbol (cleaned).
    pub doc: Option<String>,
    /// Language-level annotations/attributes/raw markers (cleaned).
    pub annotations: Vec<String>,

    /// (Legacy/plain) raw imports. Prefer `lsp.imports_used` and `graph.imports_out`.
    pub imports: Vec<String>,

    /// Human-readable signature (from AST or LSP hover).
    pub signature: Option<String>,
    /// True for the defining occurrence (as opposed to a reference-only slice).
    pub is_definition: bool,
    /// True if generated code (heuristics).
    pub is_generated: bool,

    /// Condensed code snippet for display/RAG; may be clamped.
    pub snippet: Option<String>,
    /// Raw features such as size and doc presence.
    pub features: ChunkFeatures,
    /// Content-based hash for debouncing re-index.
    pub content_sha256: String,
    /// Neighbor links for fast navigation in UI.
    pub neighbors: Option<Neighbors>,

    /// Structured identifiers present in the chunk (deduped, case-preserving).
    pub identifiers: Vec<String>,
    /// Fine-grained anchors for highlighting (absolute byte ranges).
    pub anchors: Vec<Anchor>,
    /// Cross-code relations and domain facts (e.g., routes).
    pub graph: Option<GraphEdges>,
    /// Flattened hints for hybrid retrieval (BM25 + dense).
    pub hints: Option<RetrievalHints>,

    /// Optional LSP enrichment (hover, defs, refs, semantics).
    pub lsp: Option<LspEnrichment>,
}

/// Secondary slicing for long bodies (optional).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicroChunk {
    /// Unique micro-chunk ID.
    pub id: String,
    /// Parent `CodeChunk.id`.
    pub parent_chunk_id: String,
    /// Repo-relative file path.
    pub file: String,
    /// Same symbol path as the parent (keeps navigation consistent).
    pub symbol_path: String,
    /// Order inside the parent chunk (stable).
    pub order: u32,
    /// Absolute span of this micro-chunk.
    pub span: Span,
    /// Slice content (not necessarily clamped; usually shorter than chunk).
    pub snippet: String,
    /// Content hash for dedupe.
    pub content_sha256: String,
    /// Optional role ("if_arm", "loop_body", "match_arm", etc.) for ranking and UI.
    pub role: Option<String>,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  Snippet clamping helpers
// ──────────────────────────────────────────────────────────────────────────
//

/// Returns a clamped copy of `s` limited by `max_chars` and `max_lines`.
///
/// Rules:
/// - Stops at the earliest limit (lines or chars).
/// - Preserves line boundaries up to the limit.
/// - Appends an ellipsis `…` if truncation occurred and `add_ellipsis` is true.
/// - Does not split inside a line if the next full line would exceed `max_chars`.
///
/// # Parameters
/// - `s`: Input string.
/// - `max_chars`: Maximum number of UTF-8 bytes to keep (approximate char budget).
/// - `max_lines`: Maximum number of lines to keep.
/// - `add_ellipsis`: Whether to append `…` on truncation.
///
/// # Returns
/// Clamped string without trailing newline; may include `…` if truncated.
///
/// # Examples
/// ```
/// let s = "a\nb\nc";
/// assert_eq!(clamp_snippet_ex("a\nb\nc", 3, 2, true), "a\nb");
/// ```
pub fn clamp_snippet_ex(s: &str, max_chars: usize, max_lines: usize, add_ellipsis: bool) -> String {
    if s.is_empty() {
        return String::new();
    }
    if max_chars == 0 || max_lines == 0 {
        return String::new();
    }

    let mut out = String::new();
    let mut total = 0usize;
    let mut lines = 0usize;
    let mut truncated = false;

    for (i, line) in s.lines().enumerate() {
        if lines >= max_lines {
            truncated = true;
            break;
        }

        // +1 for '\n' except for the first line we place.
        let need = line.len() + if i > 0 { 1 } else { 0 };
        if total + need > max_chars {
            truncated = true;
            break;
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
        total += need;
        lines += 1;
    }

    if truncated && add_ellipsis {
        let ell = '…';
        let ell_len = ell.len_utf8();
        if total + ell_len <= max_chars {
            out.push(ell);
        } else {
            // Best-effort: remove a few bytes to fit the ellipsis; keep UTF-8 valid.
            while out.len() + ell_len > max_chars && !out.is_empty() {
                out.pop();
                // Pop continuation bytes until we reach a char boundary.
                while !out.is_empty()
                    && (out.as_bytes()[out.len() - 1] & 0b1100_0000) == 0b1000_0000
                {
                    out.pop();
                }
            }
            if !out.is_empty() {
                out.push(ell);
            }
        }
    }

    out
}

/// Backward-compatible clamping helper with fixed behavior (no ellipsis).
///
/// This preserves your original signature and routes to `clamp_snippet_ex`
/// with `add_ellipsis = false`.
pub fn clamp_snippet(s: &str, max_chars: usize, max_lines: usize) -> String {
    clamp_snippet_ex(s, max_chars, max_lines, false)
}
