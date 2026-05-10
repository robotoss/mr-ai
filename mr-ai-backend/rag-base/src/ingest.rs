//! High-level per-repo incremental ingestion.
//!
//! `upsert_repo_chunks` is the single entry point used by the worker's
//! `ReindexHandler` after `graph_persist`. It diffs the supplied chunks
//! against what already lives in Qdrant for the given `repo_id`, then:
//!
//! 1. **Keeps** unchanged chunks (id present, `content_sha256` matches).
//! 2. **Upserts** new or changed chunks — embedded in batches sized by
//!    `cfg.qdrant.batch_size`.
//! 3. **Deletes** orphans (points present in Qdrant whose deterministic
//!    id is no longer emitted by the indexer; typically removed files
//!    or files whose chunks all shifted).
//!
//! Embedding is sequential by default (one gateway call per batch)
//! because parallelising it stresses the embedding model server and
//! offers little speed-up at our chunk sizes. The pipeline never
//! re-embeds chunks that survived the diff.

use std::sync::Arc;
use std::time::Instant;

use ai_llm_service::LlmGateway;
use code_indexer::CodeChunk;
use tracing::{debug, info, warn};

use crate::embedding::embed_texts;
use crate::errors::rag_base_error::RagBaseError;
use crate::jsonl_reader::{ChunkScope, chunk_to_triple};
use crate::structs::rag_base_config::RagConfig;
use crate::structs::rag_store::{UpsertReport, VectorPayload};
use crate::vector_db::{
    ChunkMeta, delete_by_string_ids, scroll_repo_chunk_metas, upsert_batch,
};

/// Diff the supplied chunks against the existing Qdrant state for `repo_id`,
/// then run the keep/upsert/delete pipeline.
///
/// On success returns an [`UpsertReport`] describing what the pipeline
/// actually did so the worker can log it and the operator can spot
/// runaway re-embeddings caused by content_sha drift.
pub async fn upsert_repo_chunks(
    client: &qdrant_client::Qdrant,
    cfg: &RagConfig,
    gateway: &Arc<LlmGateway>,
    repo_id: &str,
    project_id: Option<&str>,
    chunks: &[CodeChunk],
) -> Result<UpsertReport, RagBaseError> {
    let started = Instant::now();

    let scope = ChunkScope {
        project_id,
        repo_id: Some(repo_id),
    };

    // 1) Snapshot existing points for this repo so we can diff.
    let existing: Vec<ChunkMeta> =
        scroll_repo_chunk_metas(client, cfg, repo_id, 512).await?;
    let mut existing_index: std::collections::HashMap<String, ChunkMeta> =
        std::collections::HashMap::with_capacity(existing.len());
    for meta in existing {
        existing_index.insert(meta.id.clone(), meta);
    }

    // 2) Build the desired set from the indexer output.
    let mut desired: std::collections::HashMap<String, (String, VectorPayload)> =
        std::collections::HashMap::with_capacity(chunks.len());
    for chunk in chunks {
        let Some((id, embed_text, payload)) = chunk_to_triple(
            chunk,
            scope,
            cfg.clamp.preview_max_chars,
            cfg.clamp.embed_max_chars,
        ) else {
            continue;
        };
        if let Some(prev) = desired.insert(id.clone(), (embed_text, payload)) {
            // Two chunks resolving to the same deterministic id within
            // one indexer run is a real bug — log loudly and keep the
            // last write so the run still completes.
            warn!(
                target: "rag_base::ingest",
                id = %id,
                ?prev,
                "upsert_repo_chunks: duplicate chunk id within current batch; later one wins"
            );
        }
    }

    // 3) Split into keep / upsert.
    let mut to_upsert: Vec<(String, String, VectorPayload)> =
        Vec::with_capacity(desired.len());
    let mut kept: usize = 0;
    for (id, (embed_text, payload)) in desired.iter() {
        match existing_index.get(id) {
            Some(meta) if meta.content_sha256 == payload.content_sha256 => {
                kept += 1;
            }
            _ => {
                to_upsert.push((id.clone(), embed_text.clone(), payload.clone()));
            }
        }
    }

    // 4) Anything present before but absent now is an orphan.
    let to_delete: Vec<String> = existing_index
        .keys()
        .filter(|id| !desired.contains_key(*id))
        .cloned()
        .collect();

    info!(
        target: "rag_base::ingest",
        repo_id,
        existing = existing_index.len(),
        desired = desired.len(),
        keep = kept,
        upsert = to_upsert.len(),
        delete = to_delete.len(),
        "upsert_repo_chunks: diff ready"
    );

    // 5) Embed + upsert in batches.
    let batch_size = cfg.qdrant.batch_size.max(1);
    let mut upserted = 0usize;
    let mut embedded = 0usize;
    for batch in to_upsert.chunks(batch_size) {
        let texts: Vec<String> = batch.iter().map(|(_, t, _)| t.clone()).collect();
        let vectors = embed_texts(gateway, cfg, &texts).await?;
        embedded += vectors.len();

        let points: Vec<(String, Vec<f32>, VectorPayload)> = batch
            .iter()
            .zip(vectors.into_iter())
            .map(|((id, _, payload), vec)| (id.clone(), vec, payload.clone()))
            .collect();
        let written = upsert_batch(client, cfg, points).await?;
        upserted += written;
    }

    // 6) Delete orphans last so a partial failure leaves the index a
    //    superset rather than missing rows.
    if !to_delete.is_empty() {
        debug!(
            target: "rag_base::ingest",
            count = to_delete.len(),
            "upsert_repo_chunks: deleting orphans"
        );
        delete_by_string_ids(client, cfg, &to_delete).await?;
    }

    let duration_ms = started.elapsed().as_millis();
    let report = UpsertReport {
        upserted,
        deleted: to_delete.len(),
        kept,
        embedded,
        duration_ms,
    };
    info!(
        target: "rag_base::ingest",
        repo_id,
        ?report,
        "upsert_repo_chunks: finished"
    );
    Ok(report)
}
