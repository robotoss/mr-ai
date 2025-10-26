//! Data types for vector-store interaction: payload shapes, search hits,
//! and indexing statistics. No parsing structs are defined here.

use serde::{Deserialize, Serialize};

/// Minimal payload stored alongside the vector in Qdrant.
/// Tailored for preview/filters during search results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorPayload {
    pub id: String,
    pub file: String,
    pub language: String, // snake_case language name
    pub kind: String,     // snake_case symbol kind
    pub symbol: String,
    pub symbol_path: String,
    pub signature: Option<String>,
    pub doc: Option<String>,     // first line if available
    pub snippet: Option<String>, // clamped snippet for preview
    pub content_sha256: String,
    pub imports: Vec<String>,
    pub lsp_fqn: Option<String>,
    pub tags: Vec<String>,
}

/// A single semantic search hit (ranked by similarity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// Similarity score (Cosine) as returned by Qdrant.
    pub score: f32,
    /// The original chunk id / Qdrant point id.
    pub id: String,
    /// Short preview fields for UI.
    pub file: String,
    pub language: String,
    pub kind: String,
    pub symbol_path: String,
    pub signature: Option<String>,
    pub snippet: Option<String>,
}

/// Summary statistics for a full reindex operation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct IndexStats {
    /// Number of JSONL records successfully indexed.
    pub indexed: usize,
    /// Number of records skipped due to deserialization/embedding errors.
    pub skipped: usize,
    /// Total wall-clock time in milliseconds.
    pub duration_ms: u128,
}
