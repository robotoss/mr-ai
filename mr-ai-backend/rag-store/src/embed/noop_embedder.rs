use crate::{EmbeddingsProvider, RagError};
use std::{future::Future, pin::Pin};

#[derive(Clone)]
pub struct NoopEmbedder;

impl EmbeddingsProvider for NoopEmbedder {
    fn embed<'a>(
        &'a self,
        _text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<f32>, RagError>> + Send + 'a>> {
        Box::pin(async { Err(RagError::MissingEmbedding) })
    }
}
