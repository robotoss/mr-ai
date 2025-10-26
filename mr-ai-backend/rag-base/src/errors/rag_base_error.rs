//! Unified error type for the rag-base crate.

use thiserror::Error;

/// Errors produced by the RAG base module.
#[derive(Debug, Error)]
pub enum RagBaseError {
    // ── Configuration / environment ──────────────────────────────────────────
    /// Required environment variable is missing.
    #[error("missing env variable: {key}")]
    EnvMissing { key: String },

    /// Failed to parse an environment variable into the expected type.
    #[error("failed to parse env variable: {key} = '{value}'")]
    EnvParse { key: String, value: String },

    /// Configuration combination is invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    // ── I/O & filesystem ────────────────────────────────────────────────────
    /// Underlying I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    // ── JSON / serialization ────────────────────────────────────────────────
    /// JSON (de)serialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    // ── Qdrant client / transport ───────────────────────────────────────────
    /// Transport / client error from Qdrant (map underlying error later).
    #[error("qdrant error: {0}")]
    Qdrant(String),

    // ── Embeddings backend ──────────────────────────────────────────────────
    /// Embedding backend failed to initialize or to embed inputs.
    #[error("embedding error: {0}")]
    Embedding(String),

    // ── Generic operation errors ────────────────────────────────────────────
    /// A requested operation is not implemented (placeholder for TODOs).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}
