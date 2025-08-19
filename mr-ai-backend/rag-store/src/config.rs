//! Library configuration and distance kinds.

use crate::errors::RagError;

/// Distance metric kind for Qdrant collection.
#[derive(Clone, Copy, Debug)]
pub enum DistanceKind {
    Cosine,
    Dot,
    Euclid,
}

/// High-level configuration for Qdrant-backed RAG.
#[derive(Clone, Debug)]
pub struct RagConfig {
    pub qdrant_url: String,
    pub qdrant_api_key: Option<String>,
    pub collection: String,
    pub distance: DistanceKind,
    pub upsert_batch: usize,
    pub exact_search: bool,
    /// Expected embedding dimensionality (e.g. EMBEDDING_DIM=1024).
    pub embedding_dim: Option<usize>,
    /// Parallelism for embedding provider calls (EMBEDDING_CONCURRENCY).
    pub embedding_concurrency: Option<usize>,
}

impl RagConfig {
    /// Build `RagConfig` from environment variables.
    ///
    /// Recognized vars:
    /// - QDRANT_URL (required), QDRANT_COLLECTION (required)
    /// - QDRANT_DISTANCE = Cosine|Dot|Euclid (default: Cosine)
    /// - QDRANT_BATCH_SIZE (default: 256)
    /// - QDRANT_API_KEY (optional)
    /// - EXACT_SEARCH=true/false (default: false)
    /// - EMBEDDING_DIM (optional)
    /// - EMBEDDING_CONCURRENCY (optional)
    pub fn from_env() -> Result<Self, RagError> {
        use std::env;
        let url = env::var("QDRANT_URL")
            .map_err(|_| RagError::Config("QDRANT_URL is required".into()))?;
        let collection = env::var("QDRANT_COLLECTION")
            .map_err(|_| RagError::Config("QDRANT_COLLECTION is required".into()))?;

        let distance = match env::var("QDRANT_DISTANCE")
            .unwrap_or_else(|_| "Cosine".into())
            .as_str()
        {
            "Cosine" | "cosine" => DistanceKind::Cosine,
            "Dot" | "dot" => DistanceKind::Dot,
            "Euclid" | "euclid" | "L2" => DistanceKind::Euclid,
            other => {
                return Err(RagError::Config(format!(
                    "Unknown QDRANT_DISTANCE: {other}"
                )));
            }
        };

        let upsert_batch = env::var("QDRANT_BATCH_SIZE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(256);

        let exact_search = env::var("EXACT_SEARCH")
            .ok()
            .map(|s| s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let api_key = env::var("QDRANT_API_KEY").ok();

        let embedding_dim = env::var("EMBEDDING_DIM")
            .ok()
            .and_then(|s| s.parse::<usize>().ok());

        let embedding_concurrency = env::var("EMBEDDING_CONCURRENCY")
            .ok()
            .and_then(|s| s.parse::<usize>().ok());

        Ok(Self {
            qdrant_url: url,
            qdrant_api_key: api_key,
            collection,
            distance,
            upsert_batch,
            exact_search,
            embedding_dim,
            embedding_concurrency,
        })
    }

    /// Validates mandatory fields.
    pub fn validate(&self) -> Result<(), RagError> {
        if self.qdrant_url.trim().is_empty() {
            return Err(RagError::Config("empty QDRANT_URL".into()));
        }
        if self.collection.trim().is_empty() {
            return Err(RagError::Config("empty QDRANT_COLLECTION".into()));
        }
        Ok(())
    }
}

/// Vector space settings used for collection creation.
#[derive(Clone, Debug)]
pub struct VectorSpace {
    pub size: usize,
    pub distance: DistanceKind,
}
