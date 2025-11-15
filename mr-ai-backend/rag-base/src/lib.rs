//! Public API:
//! - `load_fresh_index`: drop+create collection, ingest JSONL, create payload indexes.
//! - `search_code`: semantic search with lexical re-ranking and stitched code blocks.

mod embedding;
mod jsonl_reader;
mod search;
mod stitcher;
mod vector_db;

pub mod errors;
pub mod structs;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Instant;

use tracing::info;

use embedding::embed_texts_ollama;
use errors::rag_base_error::RagBaseError;
use jsonl_reader::read_jsonl_map_to_ingest_batched;
use structs::rag_base_config::RagConfig;
use structs::rag_store::IndexStats;
use vector_db::{connect, reset_collection, upsert_batch};

use crate::structs::search_result::CodeSearchResult;

/// Rebuild Qdrant index for the given project:
/// - drop collection;
/// - create collection with fresh vector configuration;
/// - create payload indexes;
/// - read JSONL and push all chunks to Qdrant.
pub async fn load_fresh_index(project_name: &str) -> Result<IndexStats, RagBaseError> {
    info!(
        target: "rag_base::index",
        project = project_name,
        "load_fresh_index: start"
    );

    let cfg: RagConfig = RagConfig::from_env(Some(project_name))?;

    // Connect to Qdrant and guarantee a fresh collection.
    let client = connect(&cfg).await?;
    reset_collection(&client, &cfg).await?;

    let started = Instant::now();

    // Count indexed points during ingestion (no second pass).
    let indexed_counter = Arc::new(AtomicUsize::new(0));
    let skipped: usize = 0; // batch reader already skips invalid lines.

    // Stream the JSONL file in batches → embed → upsert.
    read_jsonl_map_to_ingest_batched(
        cfg.code_jsonl.as_path(),
        cfg.qdrant.batch_size,
        cfg.clamp.preview_max_chars,
        cfg.clamp.embed_max_chars,
        {
            let cfg = cfg.clone();
            let client = client.clone();
            let indexed_counter = Arc::clone(&indexed_counter);

            move |batch| {
                let cfg = cfg.clone();
                let client = client.clone();
                let indexed_counter = Arc::clone(&indexed_counter);

                async move {
                    if batch.is_empty() {
                        return Ok(());
                    }

                    let texts: Vec<String> = batch.iter().map(|(_, t, _)| t.clone()).collect();
                    let vectors = embed_texts_ollama(&cfg, &texts).await?;

                    let points = batch
                        .into_iter()
                        .zip(vectors.into_iter())
                        .map(|((id, _text, payload), vec)| (id, vec, payload))
                        .collect::<Vec<_>>();

                    let written = upsert_batch(&client, &cfg, points).await?;
                    indexed_counter.fetch_add(written, Ordering::Relaxed);
                    Ok(())
                }
            }
        },
    )
    .await?;

    let duration_ms = started.elapsed().as_millis();
    let stats = IndexStats {
        indexed: indexed_counter.load(Ordering::Relaxed),
        skipped,
        duration_ms,
    };

    info!(
        target: "rag_base::index",
        project = project_name,
        indexed = stats.indexed,
        skipped = stats.skipped,
        duration_ms = stats.duration_ms,
        "load_fresh_index: finished"
    );

    Ok(stats)
}

/// Perform semantic search and return stitched code blocks.
///
/// This is the **only public search entry point**:
/// - performs vector search with lexical re-ranking and fallback scroll;
/// - hydrates hits from JSONL to restore exact spans;
/// - merges overlapping spans and returns stitched code blocks with full code.
///
/// The result is JSON-serializable and can be returned directly from an HTTP API.
pub async fn search_code(
    project_name: &str,
    query: &str,
    k: Option<usize>,
) -> Result<Vec<CodeSearchResult>, RagBaseError> {
    let hits = search::search_hits(project_name, query, k).await?;
    let results = stitcher::search_hits_to_code_results(project_name, &hits, k).await?;
    Ok(results)
}
