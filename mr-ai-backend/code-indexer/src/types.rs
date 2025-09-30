use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

//
// ──────────────────────────────────────────────────────────────────────────
//  Core enums
// ──────────────────────────────────────────────────────────────────────────
//

/// Language discriminator for chunks and files.
///
/// Keep this list stable and language-neutral. If a language is missing,
/// prefer `Other` and use `CodeChunk.extras` to pass per-language metadata.
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

/// Symbol kind taxonomy aligned with common IDE/LSP expectations.
///
/// This is intentionally generic. For language-specific refinements,
/// put additional hints into `CodeChunk.extras` or `LspEnrichment.tags`.
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
///
/// Offsets are absolute within the file (not snippet-local). Rows/cols are
/// 0-based and refer to UTF-16 columns only if your parser requires it.
/// For most use cases treat them as display hints; byte offsets are the ground truth.
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
    /// True when documentation is present directly on the symbol.
    pub has_doc: bool,
    /// True when language-level annotations/attributes/metadata are present.
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
// —  Import / origin modeling (language-agnostic)
// ──────────────────────────────────────────────────────────────────────────
//

/// Classifies the origin of definitions and imports.
///
/// Examples:
/// - `Sdk`: "python:asyncio", "java:util";
/// - `Package`: third-party coordinates;
/// - `Local`: repository-relative file path;
/// - `Unknown`: unresolvable or custom schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Sdk,
    Package,
    Local,
    Unknown,
}

/// A definition target with origin and an optional LSP-like range.
///
/// This is intentionally generic and does not assume any concrete language scheme.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefLocation {
    /// Classified origin kind: sdk | package | local | unknown.
    pub origin: OriginKind,
    /// Canonical target string (e.g., "python:asyncio", "package:lodash", "src/foo/bar.ts").
    pub target: String,
    /// Optional (start_line, start_col, end_line, end_col).
    pub range: Option<(usize, usize, usize, usize)>,
}

/// A specific identifier provided by an import (for ranking and explainability).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportUse {
    /// sdk | package | local | unknown.
    pub origin: OriginKind,
    /// e.g., "python:asyncio", "pkg:lodash", "file:src/feature.ts".
    pub label: String,
    /// e.g., "Future", "GoRoute", "Provider", "Request".
    pub identifier: String,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  LSP-derived metrics and semantics (language-agnostic)
// ──────────────────────────────────────────────────────────────────────────
//

/// Lightweight heuristics extracted from IDE/LSP/type info for ranking & filters.
///
/// Fields here must be language-neutral. Any language/framework-specific flags
/// should go into `custom` with a namespaced key (e.g., "dart.is_widget": true).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SymbolMetrics {
    /// True if the symbol uses asynchronous constructs (async/await/futures/promise).
    pub is_async: bool,
    /// Approximate lines of code inside the symbol span.
    pub loc: Option<u32>,
    /// Number of parameters if trivially parsable from signature/hover.
    pub params_count: Option<u8>,
    /// Language/framework-specific metrics (namespaced keys, e.g., "dart.is_widget").
    pub custom: BTreeMap<String, serde_json::Value>,
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
//  Fine-grained anchors, graph edges, retrieval hints (language-agnostic)
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
///
/// All fields are language-agnostic. If you need language-specific facts,
/// namespaced keys in `facts` are recommended (e.g., "dart.routes": [...]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphEdges {
    /// Fully-qualified symbols called by this chunk.
    pub calls_out: Vec<String>,
    /// Qualified type names used (annotations, generics, extends/implements).
    pub uses_types: Vec<String>,
    /// Normalized imports touched by this chunk (e.g., "sdk:...", "package:...", "file:...").
    pub imports_out: Vec<String>,
    /// Domain-specific facts (namespaced keys recommended for per-language data).
    pub facts: BTreeMap<String, serde_json::Value>,
}

/// Flattened hints for hybrid retrieval (BM25 + dense).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetrievalHints {
    /// Bag of words for BM25 (identifiers, normalized imports, stable tokens).
    pub keywords: Vec<String>,
    /// Optional category label for UI/ranking (e.g., "test", "config", "component").
    pub category: Option<String>,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  LSP enrichment (language-agnostic)
// ──────────────────────────────────────────────────────────────────────────
//

/// LSP enrichment attached to a chunk (hover, defs, refs, semantic tokens).
///
/// Backward compatibility:
/// - `definition_uri` and `flags` are legacy.
/// - Prefer `definition`/`definitions` and structured tags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LspEnrichment {
    /// One-line signature extracted from hover (legacy).
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
    /// Example: "src/routing.ts::Router::config".
    pub fqn: Option<String>,
    /// Stable content-based symbol identifier (e.g., sha256 of file:line:signature head).
    pub stable_id: Option<String>,

    /// Collapsed import-driven usages for retrieval/ranking.
    pub imports_used: Vec<ImportUse>,
    /// Lightweight metrics for filters and ranking.
    pub metrics: Option<SymbolMetrics>,

    /// Freeform string flags (legacy; still populated by some providers).
    pub flags: Vec<String>,
    /// Freeform tags (e.g., "pkg:lodash", "sdk:python:asyncio", "kind:Class").
    pub tags: Vec<String>,
}

//
// ──────────────────────────────────────────────────────────────────────────
//  Primary chunk and micro-chunk (language-agnostic)
// ──────────────────────────────────────────────────────────────────────────
//

/// Primary indexable unit for code RAG (language-agnostic).
///
/// Design:
/// - One record per *addressable* entity (class, function, method, etc.).
/// - Avoid micro-entities like single local identifiers; store them in `identifiers`.
/// - Use `anchors` for precise highlighting instead of splitting by tokens.
/// - Per-language extras should be placed in `extras` (JSON), namespaced keys advised.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    /// Globally unique chunk ID (e.g., "<repo_rev>/<file>#<start_byte>-<end_byte>").
    pub id: String,
    /// Language of the source file.
    pub language: LanguageKind,

    /// Repo-relative file path (e.g., "packages/.../base_home_page.dart").
    pub file: String,
    /// Short symbol name (e.g., "BaseHomePage", "build", "_onItemTapped").
    pub symbol: String,
    /// Canonical symbol path (e.g., "<file>::Class::Method").
    pub symbol_path: String,
    /// Symbol kind (Class/Method/Function/...).
    pub kind: SymbolKind,
    /// Absolute file span for the symbol.
    pub span: Span,
    /// Owner chain from outer to inner (e.g., ["Router", "State"] for a method).
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
    /// Cross-code relations and domain facts (e.g., routes in a router, endpoints in a service).
    pub graph: Option<GraphEdges>,
    /// Flattened hints for hybrid retrieval (BM25 + dense).
    pub hints: Option<RetrievalHints>,

    /// Optional LSP enrichment (hover, defs, refs, semantics).
    pub lsp: Option<LspEnrichment>,

    /// Opaque per-language extras encoded as JSON.
    ///
    /// Conventions:
    /// - Use namespaced keys, e.g., "dart.is_widget", "rust.unsafe_blocks", "python.decorators".
    /// - Keep it small and essential for retrieval/explainability.
    pub extras: Option<serde_json::Value>,
}

/// Secondary slicing for long bodies (optional, language-agnostic).
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
//  Snippet clamping helpers (language-agnostic)
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
/// - `max_chars`: Maximum UTF-8 byte budget (approximate).
/// - `max_lines`: Maximum number of lines to keep.
/// - `add_ellipsis`: Whether to append `…` on truncation.
///
/// # Returns
/// Clamped string without trailing newline; may include `…` if truncated.
pub fn clamp_snippet_ex(s: &str, max_chars: usize, max_lines: usize, add_ellipsis: bool) -> String {
    if s.is_empty() || max_chars == 0 || max_lines == 0 {
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
#[inline]
pub fn clamp_snippet(s: &str, max_chars: usize, max_lines: usize) -> String {
    clamp_snippet_ex(s, max_chars, max_lines, false)
}
