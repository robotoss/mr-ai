use crate::errors::RagError;
use std::{future::Future, pin::Pin};

/// Asynchronous embedding provider.
///
/// Async is required because most real providers (Ollama, OpenAI, etc.)
/// perform HTTP requests.

/// Provider interface for embedding generation.
///
/// Implement this trait to plug in your own embedding backend (e.g., Ollama, OpenAI, local models).
pub trait EmbeddingsProvider: Send + Sync {
    /// Async embedding function.
    fn embed<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<f32>, RagError>> + Send + 'a>>;
}

/// Policy describing how to obtain embeddings during ingestion.
pub enum EmbeddingPolicy<'a> {
    /// Use precomputed embeddings if available, otherwise generate via the provider.
    PrecomputedOr(&'a dyn EmbeddingsProvider),
    /// Always generate embeddings using the provider (ignores any precomputed vectors).
    ProviderOnly(&'a dyn EmbeddingsProvider),
}

pub mod noop_embedder;
pub mod ollama;
