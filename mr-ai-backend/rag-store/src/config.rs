//! Runtime and collection configuration.

use crate::errors::RagError;

/// Distance function used for the vector space.
#[derive(Clone, Copy, Debug)]
pub enum DistanceKind {
    /// Cosine distance (recommended for most embeddings).
    Cosine,
    /// Dot product (useful for normalized vectors).
    Dot,
    /// Euclidean distance (L2).
    Euclid,
}

/// Describes the vector space of the collection.
#[derive(Clone, Debug)]
pub struct VectorSpace {
    /// Dimensionality of vectors.
    pub size: usize,
    /// Distance function.
    pub distance: DistanceKind,
}

/// Configuration for RAG ingestion and retrieval.
#[derive(Clone, Debug)]
pub struct RagConfig {
    /// Qdrant HTTP endpoint, e.g. `http://localhost:6334`.
    pub qdrant_url: String,
    /// Optional API key for Qdrant Cloud.
    pub qdrant_api_key: Option<String>,
    /// Target collection name.
    pub collection: String,
    /// Distance function (Cosine by default).
    pub distance: DistanceKind,
    /// Upsert batch size (typical range: 128..512).
    pub upsert_batch: usize,
    /// Exact search flag (false = HNSW ANN).
    pub exact_search: bool,
}

impl RagConfig {
    /// Creates a sane default config for a given collection name and Qdrant endpoint.
    pub fn new_default(url: impl Into<String>, collection: impl Into<String>) -> Self {
        Self {
            qdrant_url: url.into(),
            qdrant_api_key: None,
            collection: collection.into(),
            distance: DistanceKind::Cosine,
            upsert_batch: 256,
            exact_search: false,
        }
    }

    /// Validates config values.
    pub fn validate(&self) -> Result<(), RagError> {
        if self.qdrant_url.trim().is_empty() {
            return Err(RagError::Config("qdrant_url is empty".into()));
        }
        if self.collection.trim().is_empty() {
            return Err(RagError::Config("collection is empty".into()));
        }
        if self.upsert_batch == 0 {
            return Err(RagError::Config("upsert_batch must be > 0".into()));
        }
        Ok(())
    }
}
