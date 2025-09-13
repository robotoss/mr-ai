//! Ollama embedding provider implementation.
//!
//! Provides asynchronous embedding calls to an Ollama server using
//! `reqwest::Client`.

use std::sync::Arc;

use crate::{EmbeddingsProvider, RagError};
use ai_llm_service::service_profiles::LlmServiceProfiles;

/// Configuration for the Ollama embedding backend.
#[derive(Clone, Debug)]
pub struct OllamaConfig {
    pub svc: Arc<LlmServiceProfiles>,
    /// Expected embedding dimension size.
    pub dim: usize,
}

/// Ollama embedding provider (async).
#[derive(Clone)]
pub struct OllamaEmbedder {
    pub svc: Arc<LlmServiceProfiles>,
    dim: usize,
}

impl OllamaEmbedder {
    /// Construct a new embedder from configuration.
    pub fn new(cfg: OllamaConfig) -> Self {
        Self {
            svc: cfg.svc,
            dim: cfg.dim,
        }
    }
}

impl EmbeddingsProvider for OllamaEmbedder {
    fn embed<'a>(
        &'a self,
        text: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f32>, RagError>> + Send + 'a>>
    {
        Box::pin(async move {
            let resp = self.svc.embed(&text).await.expect("Embed failed");

            if resp.len() != self.dim {
                println!("[SOME_TEST]: Len{}", resp.len());
                return Err(RagError::VectorSizeMismatch {
                    got: resp.len(),
                    want: self.dim,
                });
            }

            Ok(resp)
        })
    }
}
