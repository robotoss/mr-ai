//! Core data models used by the library.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Canonical record stored in Qdrant and used in ingestion.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RagRecord {
    pub id: String,
    pub text: String,
    pub source: Option<String>,
    pub embedding: Option<Vec<f32>>,
    #[serde(default)]
    pub extra: BTreeMap<String, Value>,
}

/// Query parameters for RAG retrieval.
pub struct RagQuery<'a> {
    pub text: &'a str,
    pub top_k: u64,
    pub filter: Option<RagFilter>,
}

/// A single retrieval hit with score, text and source.
#[derive(Clone, Debug)]
pub struct RagHit {
    pub score: f32,
    pub text: String,
    pub source: Option<String>,
    pub raw_payload: serde_json::Value,
}

/// Simple filter (placeholder). Extend as needed.
#[derive(Clone, Debug)]
pub struct RagFilter {
    /// Exact match on a field, e.g. {"source": "path/to/file.rs"}
    pub equals: Vec<(String, serde_json::Value)>,
}
