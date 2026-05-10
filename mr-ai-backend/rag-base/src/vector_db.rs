//! Qdrant vector DB helpers: connection lifecycle, collection reset,
//! batched upserts, creating payload indexes, and top-K search using the modern `qdrant_client` API.

use qdrant_client::qdrant::{
    CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, Distance, FieldType, Filter,
    PointStruct, RetrievedPoint, ScrollPointsBuilder, SearchPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use serde_json::Value as JsonValue;
use tracing::{debug, error, info};

use crate::errors::rag_base_error::RagBaseError;
use crate::structs::rag_base_config::{DistanceMetric, RagConfig};
use crate::structs::rag_store::{SearchHit, VectorPayload};

/// Establish a gRPC connection to Qdrant using `cfg.qdrant.url`.
pub async fn connect(cfg: &RagConfig) -> Result<Qdrant, RagBaseError> {
    info!(
        target: "rag_base::vector_db",
        url = %cfg.qdrant.url,
        "connect: creating Qdrant client"
    );
    Qdrant::from_url(&cfg.qdrant.url)
        .build()
        .map_err(|e| RagBaseError::Qdrant(format!("client build: {e}")))
}

/// Drop the collection (if present), create a fresh one, and create payload indexes.
pub async fn reset_collection(client: &Qdrant, cfg: &RagConfig) -> Result<(), RagBaseError> {
    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        "reset_collection: dropping collection if exists"
    );

    // Best-effort delete: ignore errors like "not found".
    let _ = client.delete_collection(&cfg.qdrant.collection).await;

    let distance = match cfg.qdrant.distance {
        DistanceMetric::Cosine => Distance::Cosine,
        DistanceMetric::Dot => Distance::Dot,
        DistanceMetric::Euclid => Distance::Euclid,
    };

    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        dim = cfg.embedding.dim,
        ?distance,
        "reset_collection: creating collection"
    );

    client
        .create_collection(
            CreateCollectionBuilder::new(&cfg.qdrant.collection)
                .vectors_config(VectorParamsBuilder::new(cfg.embedding.dim as u64, distance)),
        )
        .await
        .map_err(|e| RagBaseError::Qdrant(format!("create_collection: {e}")))?;

    // Payload indexes for filterable fields.
    create_keyword_index(client, &cfg.qdrant.collection, "id").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "file").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "language").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "kind").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "symbol").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "symbol_path").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "content_sha256").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "tags").await?;
    create_bool_index(client, &cfg.qdrant.collection, "is_definition").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "routes").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "search_terms").await?;
    // Tenant / scope identity (S1) — required for /retrieve filtering.
    create_keyword_index(client, &cfg.qdrant.collection, "project_id").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "repo_id").await?;
    // Hierarchical chunking (S3) — emission lands later; index now so
    // reset_collection doesn't need a second pass.
    create_keyword_index(client, &cfg.qdrant.collection, "chunk_kind").await?;
    create_keyword_index(client, &cfg.qdrant.collection, "parent_symbol_id").await?;

    // Text indexes for full-text style lexical search.
    create_text_index(client, &cfg.qdrant.collection, "search_blob").await?;
    create_text_index(client, &cfg.qdrant.collection, "search_terms").await?;

    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        "reset_collection: finished"
    );

    Ok(())
}

/// Helper: create a Keyword payload index for a given field.
async fn create_keyword_index(
    client: &Qdrant,
    collection: &str,
    field: &str,
) -> Result<(), RagBaseError> {
    debug!(
        target: "rag_base::vector_db",
        collection,
        field,
        "create_keyword_index: creating index"
    );

    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(collection, field, FieldType::Keyword)
                .wait(true),
        )
        .await
        .map_err(|e| RagBaseError::Qdrant(format!("create_field_index[{field}]: {e}")))?;
    Ok(())
}

/// Helper: create a Bool payload index for a given field.
async fn create_bool_index(
    client: &Qdrant,
    collection: &str,
    field: &str,
) -> Result<(), RagBaseError> {
    debug!(
        target: "rag_base::vector_db",
        collection,
        field,
        "create_bool_index: creating index"
    );

    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(collection, field, FieldType::Bool).wait(true),
        )
        .await
        .map_err(|e| RagBaseError::Qdrant(format!("create_field_index[{field}]: {e}")))?;
    Ok(())
}

/// Helper: create a Text payload index for a given field.
///
/// This index is intended for full-text and BM25-like search over `search_blob`
/// and similar fields.
async fn create_text_index(
    client: &Qdrant,
    collection: &str,
    field: &str,
) -> Result<(), RagBaseError> {
    debug!(
        target: "rag_base::vector_db",
        collection,
        field,
        "create_text_index: creating index"
    );

    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(collection, field, FieldType::Text).wait(true),
        )
        .await
        .map_err(|e| RagBaseError::Qdrant(format!("create_field_index[text:{field}]: {e}")))?;
    Ok(())
}

/// Convert `VectorPayload` to Qdrant `Payload` (serde → JSON → try_into()).
fn payload_to_qdrant(payload: &VectorPayload) -> Result<Payload, RagBaseError> {
    let as_json: JsonValue = serde_json::to_value(payload)
        .map_err(|e| RagBaseError::Qdrant(format!("payload json: {e}")))?;
    as_json
        .try_into()
        .map_err(|e| RagBaseError::Qdrant(format!("payload convert: {e}")))
}

/// Upsert a batch of points: `(id, vector, payload)`.
pub async fn upsert_batch(
    client: &Qdrant,
    cfg: &RagConfig,
    batch: Vec<(String, Vec<f32>, VectorPayload)>,
) -> Result<usize, RagBaseError> {
    if batch.is_empty() {
        return Ok(0);
    }

    let dim = cfg.embedding.dim;
    let mut points: Vec<PointStruct> = Vec::with_capacity(batch.len());

    for (id, vector, payload) in batch {
        if vector.len() != dim {
            return Err(RagBaseError::InvalidConfig(format!(
                "vector length {} != EMBEDDING_DIM {} for id {}",
                vector.len(),
                dim,
                id
            )));
        }

        // Derive stable numeric id from external id.
        let numeric_id = hash_to_u64(&id);

        // Convert payload.
        let q_payload = payload_to_qdrant(&payload)?;

        // Build point.
        let point = PointStruct::new(numeric_id, vector, q_payload);
        points.push(point);
    }

    let point_len = points.len();

    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        count = point_len,
        "upsert_batch: upserting points"
    );

    client
        .upsert_points(
            qdrant_client::qdrant::UpsertPointsBuilder::new(&cfg.qdrant.collection, points)
                .wait(true),
        )
        .await
        .map_err(|e| {
            error!(
                target: "rag_base::vector_db",
                error = %e,
                "upsert_batch: qdrant upsert failed"
            );
            RagBaseError::Qdrant(format!("upsert_points: {e}"))
        })?;

    Ok(point_len)
}

/// Run k-NN search and return preview-friendly hits.
/// IMPORTANT: No server-side score threshold — fetch a wide pool for local reranking.
pub async fn search_top_k(
    client: &Qdrant,
    cfg: &RagConfig,
    query_vec: Vec<f32>,
    k: usize,
) -> Result<Vec<SearchHit>, RagBaseError> {
    if query_vec.len() != cfg.embedding.dim {
        return Err(RagBaseError::InvalidConfig(format!(
            "query vector length {} != EMBEDDING_DIM {}",
            query_vec.len(),
            cfg.embedding.dim
        )));
    }

    // Fetch more candidates for downstream lexical rerank (cap at 400).
    let fetch_k = (k.saturating_mul(8)).min(400).max(k);

    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        k,
        fetch_k,
        "search_top_k: start"
    );

    // Do NOT set score_threshold here — it might hide relevant candidates before rerank.
    let builder = SearchPointsBuilder::new(&cfg.qdrant.collection, query_vec, fetch_k as u64)
        .with_payload(true);

    let resp = client.search_points(builder).await.map_err(|e| {
        error!(
            target: "rag_base::vector_db",
            error = %e,
            "search_top_k: qdrant search failed"
        );
        RagBaseError::Qdrant(format!("search_points: {e}"))
    })?;

    debug!(
        target: "rag_base::vector_db",
        returned = resp.result.len(),
        "search_top_k: got scored points"
    );

    let hits = resp
        .result
        .into_iter()
        .map(map_scored_point_to_hit)
        .collect::<Vec<_>>();

    Ok(hits)
}

/// Scroll points in Qdrant collection with a given filter.
///
/// The function returns up to `limit` points with payloads and without vectors.
/// It is intended for secondary lookups (e.g. exact/lexical matches) on top of
/// the main vector search.
pub async fn scroll_with_filter(
    client: &Qdrant,
    cfg: &RagConfig,
    filter: Filter,
    limit: u32,
) -> Result<Vec<RetrievedPoint>, RagBaseError> {
    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        limit,
        "scroll_with_filter: start"
    );

    let builder = ScrollPointsBuilder::new(&cfg.qdrant.collection)
        .filter(filter)
        .with_payload(true)
        .with_vectors(false)
        .limit(limit);

    let response = client.scroll(builder).await.map_err(|e| {
        error!(
            target: "rag_base::vector_db",
            error = %e,
            "scroll_with_filter: qdrant scroll failed"
        );
        RagBaseError::Qdrant(format!("scroll: {e}"))
    })?;

    let count = response.result.len();
    info!(
        target: "rag_base::vector_db",
        count,
        "scroll_with_filter: done"
    );

    Ok(response.result)
}

/// Scroll points using a payload filter and map them into `SearchHit` with zero vector score.
///
/// This is used as a lexical fallback to guarantee recall for very short or code-like queries.
pub async fn scroll_points_filtered(
    client: &Qdrant,
    cfg: &RagConfig,
    filter: Filter,
    limit: usize,
) -> Result<Vec<SearchHit>, RagBaseError> {
    let limit_u32 = limit.min(u32::MAX as usize) as u32;

    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        limit = limit_u32,
        "scroll_points_filtered: start"
    );

    // Reuse generic scroll helper so pagination can be added later without
    // touching higher-level search code.
    let retrieved = scroll_with_filter(client, cfg, filter, limit_u32).await?;

    debug!(
        target: "rag_base::vector_db",
        returned = retrieved.len(),
        "scroll_points_filtered: got retrieved points"
    );

    let hits = retrieved
        .into_iter()
        .map(map_retrieved_point_to_hit)
        .collect::<Vec<_>>();

    Ok(hits)
}

/// Delete every point matching the supplied filter.
///
/// Wired into the worker in S2 (incremental dedup path) and into
/// `/admin/reindex_repo` in S5. Public until then so downstream crates
/// can call it once `ReindexHandler` lands.
pub async fn delete_by_filter(
    client: &Qdrant,
    cfg: &RagConfig,
    filter: Filter,
) -> Result<(), RagBaseError> {
    use qdrant_client::qdrant::DeletePointsBuilder;

    client
        .delete_points(
            DeletePointsBuilder::new(&cfg.qdrant.collection)
                .points(filter)
                .wait(true),
        )
        .await
        .map_err(|e| {
            error!(target: "rag_base::vector_db", error = %e, "delete_by_filter failed");
            RagBaseError::Qdrant(format!("delete_points: {e}"))
        })?;
    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        "delete_by_filter: done"
    );
    Ok(())
}

/// Drop every point belonging to a given repo (used by `/admin/reindex_repo`
/// and by the incremental dedup path when a repo is fully removed).
pub async fn delete_by_repo(
    client: &Qdrant,
    cfg: &RagConfig,
    repo_id: &str,
) -> Result<(), RagBaseError> {
    use qdrant_client::qdrant::Condition;
    let filter = Filter::must([Condition::matches("repo_id", repo_id.to_owned())]);
    delete_by_filter(client, cfg, filter).await
}

/// Drop every chunk attributed to a specific file inside a repo. Used by
/// the incremental dedup path when a file disappears from the worktree.
pub async fn delete_by_file(
    client: &Qdrant,
    cfg: &RagConfig,
    repo_id: &str,
    file: &str,
) -> Result<(), RagBaseError> {
    use qdrant_client::qdrant::Condition;
    let filter = Filter::must([
        Condition::matches("repo_id", repo_id.to_owned()),
        Condition::matches("file", file.to_owned()),
    ]);
    delete_by_filter(client, cfg, filter).await
}

/// Lightweight metadata used by the incremental dedup path: maps every
/// existing chunk for the supplied repo to its content hash. The S2
/// pipeline diffs the result with the newly-emitted chunks to decide
/// keep / upsert / delete sets without re-embedding unchanged content.
#[derive(Debug, Clone)]
pub struct ChunkMeta {
    pub id: String,
    pub content_sha256: String,
    pub file: String,
}

pub async fn scroll_repo_chunk_metas(
    client: &Qdrant,
    cfg: &RagConfig,
    repo_id: &str,
    page_size: u32,
) -> Result<Vec<ChunkMeta>, RagBaseError> {
    use qdrant_client::qdrant::Condition;
    let filter = Filter::must([Condition::matches("repo_id", repo_id.to_owned())]);
    // Internal pagination loop — Qdrant's scroll API returns a cursor we
    // do not surface to callers (the dedup pass needs the whole repo).
    let mut all = Vec::new();
    let mut offset: Option<qdrant_client::qdrant::PointId> = None;
    loop {
        let mut builder = ScrollPointsBuilder::new(&cfg.qdrant.collection)
            .filter(filter.clone())
            .with_payload(true)
            .with_vectors(false)
            .limit(page_size);
        if let Some(off) = offset.take() {
            builder = builder.offset(off);
        }
        let response = client
            .scroll(builder)
            .await
            .map_err(|e| RagBaseError::Qdrant(format!("scroll_repo_chunk_metas: {e}")))?;
        for point in &response.result {
            let id = extract_payload_str(&point.payload, "id").unwrap_or_default();
            let sha = extract_payload_str(&point.payload, "content_sha256").unwrap_or_default();
            let file = extract_payload_str(&point.payload, "file").unwrap_or_default();
            if id.is_empty() || sha.is_empty() {
                debug!(target: "rag_base::vector_db", "scroll_repo_chunk_metas: skipping malformed point");
                continue;
            }
            all.push(ChunkMeta { id, content_sha256: sha, file });
        }
        if let Some(next) = response.next_page_offset {
            offset = Some(next);
        } else {
            break;
        }
    }
    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        repo_id,
        total = all.len(),
        "scroll_repo_chunk_metas: done"
    );
    Ok(all)
}

fn extract_payload_str(
    payload: &std::collections::HashMap<String, qdrant_client::qdrant::Value>,
    key: &str,
) -> Option<String> {
    payload
        .get(key)
        .and_then(|v| v.clone().into_json().as_str().map(str::to_owned))
}

/// Map a `ScoredPoint` into `SearchHit` (best-effort payload extraction).
fn map_scored_point_to_hit(sp: qdrant_client::qdrant::ScoredPoint) -> SearchHit {
    // Prefer original id from payload; else use Qdrant numeric/uuid id.
    let mut original_id: Option<String> = None;
    if !sp.payload.is_empty() {
        if let Some(v) = sp.payload.get("id") {
            if let Some(s) = v.clone().into_json().as_str() {
                original_id = Some(s.to_owned());
            }
        }
    }

    let fallback_id = if let Some(pid) = sp.id {
        match pid.point_id_options {
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => n.to_string(),
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(s)) => s,
            None => String::new(),
        }
    } else {
        String::new()
    };

    let id = original_id.unwrap_or(fallback_id);

    // Extract preview fields.
    let mut file = String::new();
    let mut language = String::new();
    let mut kind = String::new();
    let mut symbol_path = String::new();
    let mut symbol = String::new();
    let mut signature: Option<String> = None;
    let mut snippet: Option<String> = None;

    if !sp.payload.is_empty() {
        if let Some(v) = sp.payload.get("file") {
            if let Some(s) = v.clone().into_json().as_str() {
                file = s.to_owned();
            }
        }
        if let Some(v) = sp.payload.get("language") {
            if let Some(s) = v.clone().into_json().as_str() {
                language = s.to_owned();
            }
        }
        if let Some(v) = sp.payload.get("kind") {
            if let Some(s) = v.clone().into_json().as_str() {
                kind = s.to_owned();
            }
        }
        if let Some(v) = sp.payload.get("symbol_path") {
            if let Some(s) = v.clone().into_json().as_str() {
                symbol_path = s.to_owned();
            }
        }
        if let Some(v) = sp.payload.get("symbol") {
            if let Some(s) = v.clone().into_json().as_str() {
                symbol = s.to_owned();
            }
        }
        if let Some(v) = sp.payload.get("signature") {
            if !v.is_null() {
                if let Some(s) = v.clone().into_json().as_str() {
                    signature = Some(s.to_owned());
                }
            }
        }
        if let Some(v) = sp.payload.get("snippet") {
            if !v.is_null() {
                if let Some(s) = v.clone().into_json().as_str() {
                    snippet = Some(s.to_owned());
                }
            }
        }
    }

    SearchHit {
        score: sp.score,
        id,
        file,
        language,
        kind,
        symbol_path,
        symbol,
        signature,
        snippet,
    }
}

/// Map a `RetrievedPoint` into `SearchHit` for scroll-based retrieval (score is zero).
fn map_retrieved_point_to_hit(rp: RetrievedPoint) -> SearchHit {
    // Prefer original id from payload; else use Qdrant numeric/uuid id.
    let mut original_id: Option<String> = None;
    if !rp.payload.is_empty() {
        if let Some(v) = rp.payload.get("id") {
            if let Some(s) = v.clone().into_json().as_str() {
                original_id = Some(s.to_owned());
            }
        }
    }

    let fallback_id = if let Some(pid) = rp.id {
        match pid.point_id_options {
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => n.to_string(),
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(s)) => s,
            None => String::new(),
        }
    } else {
        String::new()
    };

    let id = original_id.unwrap_or(fallback_id);

    let mut file = String::new();
    let mut language = String::new();
    let mut kind = String::new();
    let mut symbol_path = String::new();
    let mut symbol = String::new();
    let mut signature: Option<String> = None;
    let mut snippet: Option<String> = None;

    if !rp.payload.is_empty() {
        if let Some(v) = rp.payload.get("file") {
            if let Some(s) = v.clone().into_json().as_str() {
                file = s.to_owned();
            }
        }
        if let Some(v) = rp.payload.get("language") {
            if let Some(s) = v.clone().into_json().as_str() {
                language = s.to_owned();
            }
        }
        if let Some(v) = rp.payload.get("kind") {
            if let Some(s) = v.clone().into_json().as_str() {
                kind = s.to_owned();
            }
        }
        if let Some(v) = rp.payload.get("symbol_path") {
            if let Some(s) = v.clone().into_json().as_str() {
                symbol_path = s.to_owned();
            }
        }
        if let Some(v) = rp.payload.get("symbol") {
            if let Some(s) = v.clone().into_json().as_str() {
                symbol = s.to_owned();
            }
        }
        if let Some(v) = rp.payload.get("signature") {
            if !v.is_null() {
                if let Some(s) = v.clone().into_json().as_str() {
                    signature = Some(s.to_owned());
                }
            }
        }
        if let Some(v) = rp.payload.get("snippet") {
            if !v.is_null() {
                if let Some(s) = v.clone().into_json().as_str() {
                    snippet = Some(s.to_owned());
                }
            }
        }
    }

    SearchHit {
        score: 0.0,
        id,
        file,
        language,
        kind,
        symbol_path,
        symbol,
        signature,
        snippet,
    }
}

/// Deterministically hash an arbitrary string to a u64 ID.
pub(crate) fn hash_to_u64(s: &str) -> u64 {
    let digest = blake3::hash(s.as_bytes());
    let bytes = &digest.as_bytes()[..8];
    u64::from_le_bytes(bytes.try_into().expect("slice with incorrect length"))
}

/// Delete Qdrant points by their **string** id (the rich
/// `<repo_uuid>:<file>:<symbol_path>:<sha[..16]>` value carried in the
/// payload `id` field). The IDs are re-hashed with the same scheme as
/// `upsert_batch` so storage layout stays consistent.
pub async fn delete_by_string_ids(
    client: &Qdrant,
    cfg: &RagConfig,
    ids: &[String],
) -> Result<(), RagBaseError> {
    use qdrant_client::qdrant::points_selector::PointsSelectorOneOf;
    use qdrant_client::qdrant::{DeletePointsBuilder, PointId, PointsIdsList};

    if ids.is_empty() {
        return Ok(());
    }
    let point_ids: Vec<PointId> = ids
        .iter()
        .map(|s| PointId::from(hash_to_u64(s)))
        .collect();
    let selector = PointsSelectorOneOf::Points(PointsIdsList { ids: point_ids });
    client
        .delete_points(
            DeletePointsBuilder::new(&cfg.qdrant.collection)
                .points(selector)
                .wait(true),
        )
        .await
        .map_err(|e| {
            error!(target: "rag_base::vector_db", error = %e, "delete_by_string_ids failed");
            RagBaseError::Qdrant(format!("delete_points: {e}"))
        })?;
    info!(
        target: "rag_base::vector_db",
        collection = %cfg.qdrant.collection,
        deleted = ids.len(),
        "delete_by_string_ids: done"
    );
    Ok(())
}
