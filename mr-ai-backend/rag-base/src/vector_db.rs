//! Qdrant vector DB helpers: connection lifecycle, collection reset,
//! batched upserts, and top-K search using the **modern** `qdrant_client` API.
//!
//! ## Why this module?
//! Keep the vector-store concerns isolated and easy to replace:
//! - Connect to Qdrant over gRPC (`qdrant_client::Qdrant`).
//! - Ensure a **fresh** collection (drop → create) with the right dim/metric.
//! - Upsert points in batches (string/UUID ids + dense vector + payload).
//! - Perform k-NN search with optional `score_threshold`.
//!
//! This module does **not** parse JSONL or create embeddings — only DB I/O.
//!
//! ### References
//! - Client docs: <https://docs.rs/qdrant-client/latest/qdrant_client/>
//! - Usage examples (builders & delete/create/upsert/search): repository README.
//!
//! ## Public API
//! - [`connect`] → `Qdrant`
//! - [`reset_collection`] → drop+create collection
//! - [`upsert_batch`] → write `(id, vector, payload)`
//! - [`search_top_k`] → return preview-friendly hits

use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, PointStruct, SearchPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use serde_json::json;

use crate::errors::rag_base_error::RagBaseError;
use crate::structs::rag_base_config::{DistanceMetric, RagConfig};
use crate::structs::rag_store::{SearchHit, VectorPayload};

/// Establish a gRPC connection to Qdrant using `cfg.qdrant.url`.
///
/// This call **does not** touch any collections.
///
/// # Errors
/// Returns `RagBaseError::Qdrant` if the client cannot be constructed.
pub async fn connect(cfg: &RagConfig) -> Result<Qdrant, RagBaseError> {
    Qdrant::from_url(&cfg.qdrant.url)
        .build()
        .map_err(|e| RagBaseError::Qdrant(format!("client build: {e}")))
}

/// Drop the collection (if present) and create a new one
/// with the configured vector size and distance.
///
/// This guarantees a **clean** index and prevents stale state.
///
/// # Errors
/// Returns `RagBaseError::Qdrant` on transport/server failures when creating.
pub async fn reset_collection(client: &Qdrant, cfg: &RagConfig) -> Result<(), RagBaseError> {
    // Best-effort delete: ignore errors (e.g., not found) to keep idempotency.
    let _ = client.delete_collection(&cfg.qdrant.collection).await;

    let distance = match cfg.qdrant.distance {
        DistanceMetric::Cosine => Distance::Cosine,
        DistanceMetric::Dot => Distance::Dot,
        DistanceMetric::Euclid => Distance::Euclid,
    };

    client
        .create_collection(
            CreateCollectionBuilder::new(&cfg.qdrant.collection)
                .vectors_config(VectorParamsBuilder::new(cfg.embedding.dim as u64, distance)),
        )
        .await
        .map_err(|e| RagBaseError::Qdrant(format!("create_collection: {e}")))?;

    Ok(())
}

/// Convert our lightweight [`VectorPayload`] to Qdrant [`Payload`].
///
/// We serialize to JSON and then `try_into()` → `Payload` as recommended by the client.
fn payload_to_qdrant(payload: &VectorPayload) -> Result<Payload, RagBaseError> {
    let as_json = json!({
        "id": payload.id,
        "file": payload.file,
        "language": payload.language,
        "kind": payload.kind,
        "symbol": payload.symbol,
        "symbol_path": payload.symbol_path,
        "signature": payload.signature,
        "doc": payload.doc,
        "snippet": payload.snippet,
        "content_sha256": payload.content_sha256,
        "imports": payload.imports,
        "lsp_fqn": payload.lsp_fqn,
        "tags": payload.tags,
    });
    as_json
        .try_into()
        .map_err(|e| RagBaseError::Qdrant(format!("payload convert: {e}")))
}

/// Upsert a batch of points: `(point_id, vector, payload)`.
///
/// The vector **length must equal** `cfg.embedding.dim`.
///
/// Returns the number of upserted points.
///
/// # Errors
/// - `InvalidConfig` if any vector has the wrong dimensionality.
/// - `Qdrant` on transport/server errors.
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

        let q_payload = payload_to_qdrant(&payload)?;
        // `PointStruct::new` supports numeric and UUID/string IDs.
        let point = PointStruct::new(id, vector, q_payload);
        points.push(point);
    }

    let point_len = points.len();

    client
        .upsert_points(qdrant_client::qdrant::UpsertPointsBuilder::new(
            &cfg.qdrant.collection,
            points,
        ))
        .await
        .map_err(|e| RagBaseError::Qdrant(format!("upsert_points: {e}")))?;

    Ok(point_len)
}

/// Run k-NN search for a **query vector** and return preview-friendly hits.
///
/// This version requests **payload** back and tries to fill `SearchHit`
/// fields from it. If fields are missing or types mismatch, the hit will
/// gracefully fall back to empty strings/`None`.
///
/// If `min_score` is provided, it's passed to the request as `score_threshold`.
///
/// # Errors
/// - `InvalidConfig` if the query vector length mismatches `EMBEDDING_DIM`.
/// - `Qdrant` on transport/server errors.
pub async fn search_top_k(
    client: &Qdrant,
    cfg: &RagConfig,
    query_vec: Vec<f32>,
    k: usize,
    min_score: Option<f32>,
) -> Result<Vec<SearchHit>, RagBaseError> {
    if query_vec.len() != cfg.embedding.dim {
        return Err(RagBaseError::InvalidConfig(format!(
            "query vector length {} != EMBEDDING_DIM {}",
            query_vec.len(),
            cfg.embedding.dim
        )));
    }

    let mut builder =
        SearchPointsBuilder::new(&cfg.qdrant.collection, query_vec, k as u64).with_payload(true);

    if let Some(t) = min_score {
        builder = builder.score_threshold(t);
    }

    let resp = client
        .search_points(builder)
        .await
        .map_err(|e| RagBaseError::Qdrant(format!("search_points: {e}")))?;

    let hits = resp
        .result
        .into_iter()
        .map(map_scored_point_to_hit)
        .collect::<Vec<_>>();

    Ok(hits)
}

/// Helper: map a `ScoredPoint` into our [`SearchHit`], extracting payload best-effort.
fn map_scored_point_to_hit(sp: qdrant_client::qdrant::ScoredPoint) -> SearchHit {
    // Extract ID in a stable string form.
    let id = if let Some(pid) = sp.id {
        match pid.point_id_options {
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(s)) => s,
            Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => n.to_string(),
            None => String::new(),
        }
    } else {
        String::new()
    };

    // Extract preview fields from payload if present.
    let mut file = String::new();
    let mut language = String::new();
    let mut kind = String::new();
    let mut symbol_path = String::new();
    let mut signature: Option<String> = None;
    let mut snippet: Option<String> = None;

    if !sp.payload.is_empty() {
        // Values are `qdrant_client::qdrant::Value`; use `to_json()`/`into_json()` to read.
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
        signature,
        snippet,
    }
}
