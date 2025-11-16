//! Data types for vector-store interaction: payload shapes, search hits,
//! and indexing statistics. No parsing structs are defined here.

use serde::{Deserialize, Serialize};

/// Minimal payload stored alongside the vector in Qdrant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPayload {
    // Identification and light filters
    pub id: String,       // unique chunk id for hydration from JSONL
    pub file: String,     // file path for grouping / simple filtering
    pub language: String, // snake_case language
    pub kind: String,     // snake_case symbol kind (class/method/etc)

    // Preview and ranking context
    pub symbol: String,            // short symbol name
    pub symbol_path: String,       // <file>::Class::method
    pub signature: Option<String>, // short signature (hover/AST)
    pub doc: Option<String>,       // first doc line only
    pub snippet: Option<String>,   // clamped preview, ~300-600 chars max

    // Dedup / consistency
    pub content_sha256: String, // same chunks collapse when merging content

    // Light semantic & filter signals
    pub imports_top: Vec<String>, // top-N normalized imports (up to 8)
    pub tags: Vec<String>,        // short LSP tags (kind:file etc)
    pub lsp_fqn: Option<String>,  // optional FQN for explainability

    // Noise control
    pub is_definition: bool, // filter: drop reference-only slices

    // Domain-specific signals
    pub routes: Vec<String>, // normalized routes like "/games", "/splash_page"
    pub search_terms: Vec<String>, // compact token bag for lexical rerank

    // Full-text searchable blob (FTS index at Qdrant)
    pub search_blob: String,
}

/// A single semantic search hit (ranked by similarity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub score: f32,
    pub id: String,

    // Lightweight preview fields for UI
    pub file: String,
    pub language: String,
    pub kind: String,
    pub symbol_path: String,
    pub symbol: String,
    pub signature: Option<String>,
    pub snippet: Option<String>,
}

/// Summary statistics for a full reindex operation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct IndexStats {
    pub indexed: usize,
    pub skipped: usize,
    pub duration_ms: u128,
}
