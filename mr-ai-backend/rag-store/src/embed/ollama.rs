//! Ollama embedding provider implementation.
//!
//! Provides asynchronous embedding calls to an Ollama server using
//! `reqwest::Client`.

use crate::{EmbeddingsProvider, RagError};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Configuration for the Ollama embedding backend.
#[derive(Clone, Debug)]
pub struct OllamaConfig {
    /// Base URL of the Ollama server (e.g. http://localhost:7869).
    pub url: String,
    /// Model name or tag to use (e.g. "nomic-embed-text").
    pub model: String,
    /// Expected embedding dimension size.
    pub dim: usize,
}

/// Ollama embedding provider (async).
#[derive(Clone)]
pub struct OllamaEmbedder {
    client: Client,
    url: String,
    model: String,
    dim: usize,
}

impl OllamaEmbedder {
    /// Construct a new embedder from configuration.
    pub fn new(cfg: OllamaConfig) -> Self {
        Self {
            client: Client::new(),
            url: cfg.url,
            model: cfg.model,
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
            #[derive(Serialize)]
            struct Request<'a> {
                model: &'a str,
                prompt: &'a str,
            }

            #[derive(Deserialize)]
            struct Response {
                embedding: Vec<f32>,
            }

            let req = Request {
                model: &self.model,
                prompt: text,
            };

            let resp = self
                .client
                .post(&format!("{}/api/embeddings", self.url))
                .json(&req)
                .send()
                .await
                .map_err(|e| RagError::Provider(format!("Ollama request failed: {e}")))?
                .error_for_status()
                .map_err(|e| RagError::Provider(format!("Ollama HTTP error: {e}")))?;

            let parsed: Response = resp
                .json()
                .await
                .map_err(|e| RagError::Provider(format!("Ollama JSON parse failed: {e}")))?;

            if parsed.embedding.len() != self.dim {
                return Err(RagError::VectorSizeMismatch {
                    got: parsed.embedding.len(),
                    want: self.dim,
                });
            }

            Ok(parsed.embedding)
        })
    }
}
