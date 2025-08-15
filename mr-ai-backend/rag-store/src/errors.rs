//! Unified error types for the crate.

use thiserror::Error;

/// Top-level error for rag-store operations.
#[derive(Debug, Error)]
pub enum RagError {
    /// I/O or filesystem errors.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON parsing / serialization errors.
    #[error("parse error: {0}")]
    Parse(#[from] serde_json::Error),

    /// Invalid or unsupported configuration.
    #[error("config error: {0}")]
    Config(String),

    /// Mismatch in vector dimensionality across records.
    #[error("vector size mismatch: got {got}, want {want}")]
    VectorSizeMismatch { got: usize, want: usize },

    /// Missing embedding and no provider available.
    #[error("missing embedding and no provider supplied")]
    MissingEmbedding,

    /// Qdrant client errors (wrapped).
    #[error("qdrant error: {0}")]
    Qdrant(String),

    /// Generic error from anyhow chain.
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}
