//! End-to-end ingestion pipeline: read JSONL → normalize → resolve vectors → upsert into Qdrant.
//!
//! Supports both strict and flexible parsing of `rag_records.jsonl`, and also
//! ingestion from AST/graph JSONL files with compact mappers.
//! Embeddings are resolved via policy or computed within this module.

use crate::config::{RagConfig, VectorSpace};
use crate::discovery::{latest_dump_dir, rag_records_path, read_dump_summary};
use crate::embed::{EmbeddingPolicy, EmbeddingsProvider};
use crate::embed_pool::embed_missing;
use crate::errors::RagError;
use crate::io_jsonl::{read_all_jsonl, read_all_records};
use crate::mappers::{map_ast_node, map_graph_edge, map_graph_node};
use crate::normalize::normalize_code_light;
use crate::qdrant_facade::QdrantFacade;
use crate::record::RagRecord;

use indicatif::{ProgressBar, ProgressStyle};
use qdrant_client::qdrant::{
    PointId, PointStruct, Value as QValue, Vector, Vectors, point_id, value, vectors,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use tracing::{debug, info, warn};

/// Ingests the latest JSONL dump under `<root>/project_x/graphs_data/<timestamp>`.
///
/// Uses [`ingest_file`] under the hood.
pub async fn ingest_latest_from(
    cfg: &RagConfig,
    root: impl AsRef<std::path::Path>,
    policy: EmbeddingPolicy<'_>,
    client: &QdrantFacade,
) -> Result<u64, RagError> {
    let dir = latest_dump_dir(root)?;
    let jsonl = rag_records_path(&dir);
    ingest_file(cfg, jsonl, policy, client).await
}

/// Ingests records from a specific JSONL path (strict reader with a flexible fallback).
///
/// 1. Tries `read_all_records` (strict schema).
/// 2. On failure, falls back to `read_all_jsonl` + [`map_any_rag_line`].
/// 3. Normalizes text, ensures collection exists, upserts in batches.
pub async fn ingest_file(
    cfg: &RagConfig,
    jsonl_path: impl AsRef<std::path::Path>,
    policy: EmbeddingPolicy<'_>,
    client: &QdrantFacade,
) -> Result<u64, RagError> {
    info!("Ingesting file {:?}", jsonl_path.as_ref());

    let mut records = read_strict_or_fallback(&jsonl_path)?;

    if records.is_empty() {
        debug!("No records found in file");
        return Ok(0);
    }

    // Normalize texts for compact embeddings
    let max_chars = chunk_max_chars();
    for r in &mut records {
        r.text = normalize_code_light(&r.text, max_chars);
    }

    // Vector dimensionality
    let vector_size = determine_vector_size(&records, &policy, cfg.embedding_dim).await?;
    debug!("Vector size determined: {}", vector_size);

    client
        .ensure_collection(&VectorSpace {
            size: vector_size,
            distance: cfg.distance,
        })
        .await?;

    // Upsert in batches
    let mut total: u64 = 0;
    let batch_size = cfg.upsert_batch.max(1);
    for chunk in records.chunks(batch_size) {
        let points = build_points(chunk, vector_size, &policy).await?;
        total += client.upsert_points(points).await?;
    }

    info!("Ingested {} records from file", total);
    Ok(total)
}

/// Ingests **all** supported files from the latest dump and computes embeddings on the fly.
///
/// Sources:
/// - `rag_records.jsonl` (strict → fallback)
/// - `ast_nodes.jsonl`
/// - `graph_nodes.jsonl`
/// - `graph_edges.jsonl`
/// Ingests the latest JSONL dump under `<root>/project_x/graphs_data/<timestamp>`,
/// embedding all records (ignores precomputed vectors if provider-only).
///
/// Uses `embed_missing` to fill vectors and then upserts into Qdrant.
pub async fn ingest_latest_all_embedded(
    cfg: &RagConfig,
    root: impl AsRef<std::path::Path>,
    provider: &(dyn EmbeddingsProvider + Send + Sync),
    client: &QdrantFacade,
) -> Result<u64, RagError> {
    info!(
        "Ingesting latest dump with embedding from {:?}",
        root.as_ref()
    );

    let dir = latest_dump_dir(root)?;
    let summary = read_dump_summary(&dir).map_err(RagError::Io)?;

    let max_chars = chunk_max_chars();
    let mut records: Vec<RagRecord> = Vec::new();

    // rag_records.jsonl
    let rr = dir.join("rag_records.jsonl");
    if rr.exists() {
        records.extend(read_strict_or_fallback(&rr)?);
    }

    // ast_nodes.jsonl
    if let Some(p) = summary.files.get("ast_nodes_jsonl") {
        records.extend(
            read_all_jsonl(p)?
                .into_iter()
                .filter_map(|v| map_ast_node(v, max_chars)),
        );
    }
    // graph_nodes.jsonl
    if let Some(p) = summary.files.get("graph_nodes_jsonl") {
        records.extend(
            read_all_jsonl(p)?
                .into_iter()
                .filter_map(|v| map_graph_node(v, max_chars)),
        );
    }
    // graph_edges.jsonl
    if let Some(p) = summary.files.get("graph_edges_jsonl") {
        records.extend(
            read_all_jsonl(p)?
                .into_iter()
                .filter_map(|v| map_graph_edge(v, max_chars)),
        );
    }

    if records.is_empty() {
        warn!("No records collected from dump");
        return Ok(0);
    }

    dedup_in_place(&mut records);

    let want_dim = cfg.embedding_dim;
    let conc = cfg.embedding_concurrency.unwrap_or(4);
    embed_missing(&mut records, provider, want_dim, conc).await?;

    let vector_size = determine_vector_size(
        &records,
        &EmbeddingPolicy::PrecomputedOr(provider),
        want_dim,
    )
    .await?;
    client
        .ensure_collection(&VectorSpace {
            size: vector_size,
            distance: cfg.distance,
        })
        .await?;

    // --- NEW: progress bar ---
    let total_chunks = (records.len() + cfg.upsert_batch - 1) / cfg.upsert_batch;
    let pb = ProgressBar::new(total_chunks as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta})",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let mut total: u64 = 0;
    let batch_size = cfg.upsert_batch.max(1);
    for chunk in records.chunks(batch_size) {
        let points = build_points(
            chunk,
            vector_size,
            &EmbeddingPolicy::PrecomputedOr(provider),
        )
        .await?;
        total += client.upsert_points(points).await?;
        pb.inc(1);
    }

    pb.finish_with_message("Ingestion complete ✔");

    info!("Ingested {} records total", total);
    Ok(total)
}

// ---------- helpers ----------

/// Pick strict records, fallback to flexible mapper.
fn read_strict_or_fallback(
    jsonl_path: impl AsRef<std::path::Path>,
) -> Result<Vec<RagRecord>, RagError> {
    match read_all_records(&jsonl_path) {
        Ok(v) => Ok(v),
        Err(e) => {
            warn!("Strict parser failed: {e}. Falling back to flexible mapper…");
            let raw = read_all_jsonl(&jsonl_path)?;
            Ok(raw.into_iter().filter_map(map_any_rag_line).collect())
        }
    }
}

/// Parse `CHUNK_MAX_CHARS` env var (default=4000).
fn chunk_max_chars() -> usize {
    std::env::var("CHUNK_MAX_CHARS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(4000)
}

/// Determines the embedding dimensionality.
async fn determine_vector_size(
    records: &[RagRecord],
    policy: &EmbeddingPolicy<'_>,
    expected_dim: Option<usize>,
) -> Result<usize, RagError> {
    if let Some(dim) = expected_dim {
        if let Some(v) = records.iter().find_map(|r| r.embedding.as_ref()) {
            if v.len() != dim {
                return Err(RagError::VectorSizeMismatch {
                    got: v.len(),
                    want: dim,
                });
            }
        }
        return Ok(dim);
    }

    if let Some(v) = records.iter().find_map(|r| r.embedding.as_ref()) {
        return Ok(v.len());
    }

    match policy {
        EmbeddingPolicy::PrecomputedOr(p) | EmbeddingPolicy::ProviderOnly(p) => {
            let v = p.embed(&records[0].text).await?;
            Ok(v.len())
        }
    }
}

/// Builds Qdrant points for a batch of records.
async fn build_points(
    chunk: &[RagRecord],
    vector_size: usize,
    policy: &EmbeddingPolicy<'_>,
) -> Result<Vec<PointStruct>, RagError> {
    let mut pts = Vec::with_capacity(chunk.len());

    for r in chunk {
        // Resolve embedding
        let vector = match (&r.embedding, policy) {
            (Some(v), _) => v.clone(),
            (None, EmbeddingPolicy::PrecomputedOr(p)) => p.embed(&r.text).await?,
            (None, EmbeddingPolicy::ProviderOnly(p)) => p.embed(&r.text).await?,
        };

        if vector.len() != vector_size {
            return Err(RagError::VectorSizeMismatch {
                got: vector.len(),
                want: vector_size,
            });
        }

        // Payload
        let mut payload: HashMap<String, QValue> = HashMap::new();
        payload.insert(
            "text".into(),
            QValue {
                kind: Some(value::Kind::StringValue(r.text.clone())),
            },
        );
        if let Some(src) = &r.source {
            payload.insert(
                "source".into(),
                QValue {
                    kind: Some(value::Kind::StringValue(src.clone())),
                },
            );
        }
        for (k, v) in &r.extra {
            payload.insert(k.clone(), json_to_qvalue(v.clone()));
        }

        // ID
        let id = if let Ok(n) = r.id.parse::<u64>() {
            PointId {
                point_id_options: Some(point_id::PointIdOptions::Num(n)),
            }
        } else {
            PointId {
                point_id_options: Some(point_id::PointIdOptions::Uuid(r.id.clone())),
            }
        };

        // Vector wrapper
        let vectors = Vectors {
            vectors_options: Some(vectors::VectorsOptions::Vector(Vector {
                data: vector,
                indices: None,
                vectors_count: None,
                vector: None,
            })),
        };

        pts.push(PointStruct {
            id: Some(id),
            payload,
            vectors: Some(vectors),
            ..Default::default()
        });
    }

    Ok(pts)
}

/// Converts `serde_json::Value` into Qdrant `Value`.
fn json_to_qvalue(v: serde_json::Value) -> QValue {
    use value::Kind as K;
    match v {
        Value::String(s) => QValue {
            kind: Some(K::StringValue(s)),
        },
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                QValue {
                    kind: Some(K::IntegerValue(i)),
                }
            } else if let Some(f) = n.as_f64() {
                QValue {
                    kind: Some(K::DoubleValue(f)),
                }
            } else {
                QValue {
                    kind: Some(K::StringValue(n.to_string())),
                }
            }
        }
        Value::Bool(b) => QValue {
            kind: Some(K::BoolValue(b)),
        },
        other => QValue {
            kind: Some(K::StringValue(other.to_string())),
        },
    }
}

/// Flexible mapper for arbitrary JSONL lines → `RagRecord`.
fn map_any_rag_line(v: Value) -> Option<RagRecord> {
    let obj = v.as_object()?;

    // id
    let id = pick_str(obj, &["id", "uuid", "hash", "name"])
        .map(|s| s.to_string())
        .unwrap_or_else(|| stable_hash(&v));

    // text
    let mut text = pick_str(
        obj,
        &[
            "text",
            "content",
            "chunk",
            "code",
            "body",
            "doc",
            "description",
            "summary",
        ],
    )
    .unwrap_or("")
    .to_string();

    if text.is_empty() {
        for sub in obj.values() {
            if let Some(m) = sub.as_object() {
                if let Some(s) = pick_str(
                    m,
                    &["text", "content", "doc", "description", "code", "body"],
                ) {
                    text = s.to_string();
                    break;
                }
            }
        }
    }
    if text.is_empty() {
        text = v.to_string();
    }

    let source = pick_str(obj, &["source", "file", "path", "uri"]).map(|s| s.to_string());

    let embedding = pick_vec_f32(obj, &["embedding", "vector", "values", "embedding_vector"])
        .or_else(|| {
            for sub in obj.values() {
                if let Some(m) = sub.as_object() {
                    if let Some(vec) =
                        pick_vec_f32(m, &["embedding", "vector", "values", "embedding_vector"])
                    {
                        return Some(vec);
                    }
                }
            }
            None
        });

    let extra = obj
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<BTreeMap<_, _>>();

    Some(RagRecord {
        id,
        text,
        source,
        embedding,
        extra,
    })
}

/// Best-effort deduplication by `(source,text)`.
fn dedup_in_place(recs: &mut Vec<RagRecord>) {
    fn key_of(r: &RagRecord) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        let mut h = DefaultHasher::new();
        r.source.hash(&mut h);
        r.text.hash(&mut h);
        h.finish()
    }
    let mut seen: HashSet<u64> = HashSet::with_capacity(recs.len());
    recs.retain(|r| seen.insert(key_of(r)));
}

/// Helper: pick string by keys.
fn pick_str<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            return Some(s);
        }
    }
    None
}

/// Helper: pick vector<f32> by keys (no deep recursion).
fn pick_vec_f32(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<Vec<f32>> {
    for k in keys {
        if let Some(a) = obj.get(*k).and_then(|v| v.as_array()) {
            let mut out = Vec::with_capacity(a.len());
            for x in a {
                if let Some(f) = x.as_f64() {
                    out.push(f as f32);
                } else if let Some(i) = x.as_i64() {
                    out.push(i as f32);
                } else {
                    return None;
                }
            }
            return Some(out);
        }
    }
    None
}

/// Stable hash used when no `id` present.
fn stable_hash(v: &Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    v.to_string().hash(&mut h);
    format!("rec_{:016x}", h.finish())
}
