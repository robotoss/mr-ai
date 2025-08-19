//! Error types used across the RAG library.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RagError {
    #[error("config error: {0}")]
    Config(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("qdrant error: {0}")]
    Qdrant(String),

    #[error("missing embedding")]
    MissingEmbedding,

    #[error("vector size mismatch: got={got}, want={want}")]
    VectorSizeMismatch { got: usize, want: usize },
}
