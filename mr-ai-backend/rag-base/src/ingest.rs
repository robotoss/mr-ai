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
use crate::chunk_mapping::{ChunkScope, chunk_to_triple};
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

    // 2) Single pass: convert chunks → triples, classify into
    //    keep / upsert on the fly. No intermediate `desired` HashMap
    //    of full payloads — previously we held two copies of every
    //    payload in memory at once for a ~50k-chunk repo.
    let mut to_upsert: Vec<(String, String, VectorPayload)> = Vec::with_capacity(chunks.len());
    let mut kept: usize = 0;
    let mut desired_ids: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(chunks.len());
    for chunk in chunks {
        let Some((id, embed_text, payload)) = chunk_to_triple(
            chunk,
            scope,
            cfg.clamp.preview_max_chars,
            cfg.clamp.embed_max_chars,
        ) else {
            continue;
        };
        if !desired_ids.insert(id.clone()) {
            // Two chunks colliding on the deterministic id within one
            // indexer run is a real bug; log loudly so the operator
            // can investigate. Later write wins — we already pushed
            // the earlier triple to `to_upsert`, but Qdrant's upsert
            // is keyed on the hash so it overwrites cleanly.
            warn!(
                target: "rag_base::ingest",
                id = %id,
                "upsert_repo_chunks: duplicate chunk id within current batch; later one overwrites"
            );
        }
        match existing_index.get(&id) {
            Some(meta) if meta.content_sha256 == payload.content_sha256 => {
                kept += 1;
            }
            _ => {
                to_upsert.push((id, embed_text, payload));
            }
        }
    }

    // 3) Anything present before but absent now is an orphan.
    let to_delete: Vec<String> = existing_index
        .keys()
        .filter(|id| !desired_ids.contains(*id))
        .cloned()
        .collect();

    info!(
        target: "rag_base::ingest",
        repo_id,
        existing = existing_index.len(),
        desired = desired_ids.len(),
        keep = kept,
        upsert = to_upsert.len(),
        delete = to_delete.len(),
        "upsert_repo_chunks: diff ready"
    );

    // 4) Embed + upsert in batches. We `drain` from the front of
    //    `to_upsert` instead of `chunks(...)` + clone so each triple
    //    lives in exactly one `Vec` at a time — embed sees a borrowed
    //    text slice, then the (id, vector, payload) tuples move into
    //    `upsert_batch` without a second copy.
    let batch_size = cfg.qdrant.batch_size.max(1);
    let mut upserted = 0usize;
    let mut embedded = 0usize;
    while !to_upsert.is_empty() {
        let take = to_upsert.len().min(batch_size);
        let texts: Vec<String> = to_upsert[..take].iter().map(|(_, t, _)| t.clone()).collect();
        let vectors = embed_texts(gateway, cfg, &texts).await?;
        embedded += vectors.len();
        if vectors.len() != take {
            return Err(RagBaseError::Embedding(format!(
                "embed_texts returned {} vectors, expected {}",
                vectors.len(),
                take
            )));
        }
        let points: Vec<(String, Vec<f32>, VectorPayload)> = to_upsert
            .drain(..take)
            .zip(vectors.into_iter())
            .map(|((id, _, payload), vec)| (id, vec, payload))
            .collect();
        let written = upsert_batch(client, cfg, points).await?;
        upserted += written;
    }

    // 5) Delete orphans last so a partial failure leaves the index a
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
