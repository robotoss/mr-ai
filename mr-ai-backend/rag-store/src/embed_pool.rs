//! Embedding executor with concurrency and dimension checks.

use crate::{embed::EmbeddingsProvider, errors::RagError, record::RagRecord};
use futures::stream::{self, StreamExt};
use tracing::{debug, info};

/// Embeds texts for records that have no precomputed vectors.
///
/// # Arguments
/// - `records`: mutable slice of `RagRecord`s.
/// - `provider`: embedding backend (synchronous API).
/// - `expected_dim`: if `Some`, enforces this vector size (error on mismatch).
/// - `concurrency`: maximum number of concurrent embedding tasks.
///
/// # Errors
/// Returns [`RagError::VectorSizeMismatch`] if dimensions mismatch,
/// or [`RagError::Provider`] if the provider fails.
pub async fn embed_missing(
    records: &mut [RagRecord],
    provider: &dyn EmbeddingsProvider,
    expected_dim: Option<usize>,
    concurrency: usize,
) -> Result<(), RagError> {
    info!(
        "embed_pool::embed_missing: total={} concurrency={}",
        records.len(),
        concurrency
    );

    let idxs: Vec<usize> = records
        .iter()
        .enumerate()
        .filter_map(|(i, r)| if r.embedding.is_none() { Some(i) } else { None })
        .collect();

    if idxs.is_empty() {
        debug!("embed_pool::embed_missing: nothing to embed");
        return Ok(());
    }

    let results: Vec<(usize, Vec<f32>)> = stream::iter(idxs.into_iter())
        .map(|i| {
            let text = records[i].text.clone();
            async move {
                let v = provider.embed(&text).await?;
                Ok::<(usize, Vec<f32>), RagError>((i, v))
            }
        })
        .buffer_unordered(concurrency.max(1))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, RagError>>()?;

    for (i, v) in results {
        if let Some(want) = expected_dim {
            if v.len() != want {
                return Err(RagError::VectorSizeMismatch { got: v.len(), want });
            }
        }
        records[i].embedding = Some(v);
    }

    debug!("embed_pool::embed_missing: embeddings filled");
    Ok(())
}
