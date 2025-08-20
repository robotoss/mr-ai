//! Typed error for the contextor crate.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ContextorError {
    /// Errors from the underlying rag-store crate.
    #[error("RAG error: {0}")]
    Rag(#[from] rag_store::RagError),

    /// HTTP/transport errors when calling Ollama.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON (de)serialization issues (should be rare).
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Generic IO if needed by future extensions.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
