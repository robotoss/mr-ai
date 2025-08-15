//! Embedding abstraction and policies.

use crate::errors::RagError;

/// Provider interface for embedding generation.
///
/// Implement this trait to plug in your own embedding backend (e.g., Ollama, OpenAI, local models).
pub trait EmbeddingsProvider: Send + Sync {
    /// Produces an embedding vector for the given text.
    fn embed(&self, text: &str) -> Result<Vec<f32>, RagError>;
}

/// Policy describing how to obtain embeddings during ingestion.
pub enum EmbeddingPolicy<'a> {
    /// Use precomputed embeddings if available, otherwise generate via the provider.
    PrecomputedOr(&'a dyn EmbeddingsProvider),
    /// Always generate embeddings using the provider (ignores any precomputed vectors).
    ProviderOnly(&'a dyn EmbeddingsProvider),
}
