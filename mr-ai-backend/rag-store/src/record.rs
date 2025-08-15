//! Canonical data models and public API types.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Single JSONL record to be ingested into Qdrant.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RagRecord {
    /// Stable external ID; stored as Qdrant point ID (string).
    pub id: String,
    /// Text chunk content to be retrieved by similarity.
    pub text: String,
    /// Optional source path or title.
    #[serde(default)]
    pub source: Option<String>,
    /// Optional precomputed embedding vector.
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    /// Extra metadata; all unknown fields are preserved.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Filter abstraction used by retrieval methods.
#[derive(Clone, Debug)]
pub enum RagFilter {
    /// Filter by exact `source` equality.
    BySource(String),
    /// Filter by exact field equality.
    ByFieldEq { key: String, value: Value },
    /// Conjunction of filters.
    And(Vec<RagFilter>),
    /// Disjunction of filters.
    Or(Vec<RagFilter>),
}

/// Normalized query parameters for RAG context.
#[derive(Clone, Debug)]
pub struct RagQuery<'a> {
    /// Input query text to be embedded.
    pub text: &'a str,
    /// Number of results to return.
    pub top_k: u64,
    /// Optional filter to narrow down the search space.
    pub filter: Option<RagFilter>,
}

/// Normalized retrieval hit returned by `rag_context`.
#[derive(Clone, Debug, Serialize)]
pub struct RagHit {
    /// Similarity score returned by Qdrant.
    pub score: f32,
    /// Text chunk content from payload.
    pub text: String,
    /// Optional source metadata.
    pub source: Option<String>,
    /// Full payload as JSON for downstream processing.
    pub payload: serde_json::Value,
}

/// Distance function used for the vector space (re-exported from config).
#[derive(Clone, Copy, Debug)]
pub enum DistanceKind {
    /// Cosine distance.
    Cosine,
    /// Dot product.
    Dot,
    /// Euclidean distance (L2).
    Euclid,
}

/// Describes the vector space of the collection (re-exported from config).
#[derive(Clone, Debug)]
pub struct VectorSpace {
    /// Dimensionality of vectors.
    pub size: usize,
    /// Distance function.
    pub distance: DistanceKind,
}
