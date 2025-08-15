//! End-to-end ingestion pipeline: read JSONL -> resolve vectors -> upsert into Qdrant.

use crate::config::{RagConfig, VectorSpace};
use crate::discovery::{latest_dump_dir, rag_records_path};
use crate::embed::EmbeddingPolicy;
use crate::errors::RagError;
use crate::io_jsonl::read_all_records; // strict reader -> RagRecord
use crate::qdrant_facade::QdrantFacade;
use crate::record::RagRecord;

use qdrant_client::qdrant::{
    PointId, PointStruct, Value as QValue, Vector, Vectors, point_id, value, vectors,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::{collections::BTreeMap, path::Path};
use tracing::trace;

/// Ingests the latest JSONL dump under `<root>/project_x/graphs_data/<timestamp>`.
///
/// # Errors
/// Returns I/O, parsing, vector size mismatch, or Qdrant errors.
pub async fn ingest_latest_from(
    cfg: &RagConfig,
    root: impl AsRef<std::path::Path>,
    policy: EmbeddingPolicy<'_>,
    client: &QdrantFacade,
) -> Result<usize, RagError> {
    let dir = latest_dump_dir(root)?;
    let jsonl = rag_records_path(dir);
    ingest_file(cfg, jsonl, policy, client).await
}

/// Ingests records from a specific JSONL path (rag_records.jsonl).
///
/// Strategy:
/// 1) Try strict parser (`RagRecord` via `read_all_records`).
/// 2) If it fails (e.g. missing `text`), fallback to flexible line-by-line mapper.
pub async fn ingest_file(
    cfg: &RagConfig,
    jsonl_path: impl AsRef<std::path::Path>,
    policy: EmbeddingPolicy<'_>,
    client: &QdrantFacade,
) -> Result<usize, RagError> {
    trace!("ingest::ingest_file path={:?}", jsonl_path.as_ref());

    // --- Strict path
    let mut records = match read_all_records(&jsonl_path) {
        Ok(v) => {
            trace!(
                "ingest::ingest_file strict parser accepted {} records",
                v.len()
            );
            v
        }
        Err(e) => {
            trace!(
                "ingest::ingest_file strict parser failed: {e}. Falling back to flexible parser…"
            );
            read_rag_records_flexible(&jsonl_path)?
        }
    };

    if records.is_empty() {
        trace!("ingest::ingest_file no records after parsing");
        return Ok(0);
    }

    // Determine vector size from precomputed or provider.
    let vector_size = determine_vector_size(&records, &policy)?;
    trace!("ingest::ingest_file vector_size={}", vector_size);

    // Ensure collection exists with appropriate vector space.
    client
        .ensure_collection(&VectorSpace {
            size: vector_size,
            distance: cfg.distance,
        })
        .await?;

    // Prepare and upsert in batches.
    let mut total = 0usize;
    let batch_size = cfg.upsert_batch.max(1);
    for chunk in records.drain(..).collect::<Vec<_>>().chunks(batch_size) {
        let points = build_points(chunk, vector_size, &policy)?;
        trace!("ingest::ingest_file upserting batch size={}", points.len());
        total += client.upsert_points(points).await?;
    }

    trace!("ingest::ingest_file total_upserted={total}");
    Ok(total)
}

fn determine_vector_size(
    records: &[RagRecord],
    policy: &EmbeddingPolicy<'_>,
) -> Result<usize, RagError> {
    // Try to find first record with precomputed embedding.
    if let Some(v) = records.iter().find_map(|r| r.embedding.as_ref()) {
        return Ok(v.len());
    }
    // Otherwise embed the first record's text to infer size.
    match policy {
        EmbeddingPolicy::PrecomputedOr(p) | EmbeddingPolicy::ProviderOnly(p) => {
            let v = p.embed(&records[0].text)?;
            Ok(v.len())
        }
    }
}

/// Converts JSON payload map into `Payload` and constructs `PointStruct`
/// using protobuf types from `qdrant_client::qdrant`.
fn build_points(
    chunk: &[RagRecord],
    vector_size: usize,
    policy: &EmbeddingPolicy<'_>,
) -> Result<Vec<PointStruct>, RagError> {
    trace!("ingest::build_points chunk_size={}", chunk.len());
    let mut pts = Vec::with_capacity(chunk.len());

    for mut r in chunk.to_owned() {
        // Resolve embedding vector according to the policy.
        let vector = match (&r.embedding, policy) {
            (Some(v), _) => v.clone(),
            (None, EmbeddingPolicy::PrecomputedOr(p)) => p.embed(&r.text)?,
            (None, EmbeddingPolicy::ProviderOnly(p)) => p.embed(&r.text)?,
        };

        if vector.len() != vector_size {
            return Err(RagError::VectorSizeMismatch {
                got: vector.len(),
                want: vector_size,
            });
        }

        // ---- Payload: HashMap<String, qdrant::Value>
        let mut payload_map: HashMap<String, QValue> = HashMap::new();
        payload_map.insert("text".into(), json_to_qvalue(Value::String(r.text)));
        if let Some(src) = r.source.take() {
            payload_map.insert("source".into(), json_to_qvalue(Value::String(src)));
        }
        for (k, v) in r.extra.into_iter() {
            payload_map.insert(k, json_to_qvalue(v));
        }

        // ---- Vectors
        let vectors_wrapped = {
            // Fill only data; other optional prost fields via defaults.
            let v = Vector {
                data: vector,
                ..Default::default()
            };
            Vectors {
                vectors_options: Some(vectors::VectorsOptions::Vector(v)),
            }
        };

        // ---- PointId
        let id_opts = match r.id.parse::<u64>() {
            Ok(n) => point_id::PointIdOptions::Num(n),
            Err(_) => point_id::PointIdOptions::Uuid(r.id),
        };
        let point_id = PointId {
            point_id_options: Some(id_opts),
        };

        // ---- Final point
        let point = PointStruct {
            id: Some(point_id),
            payload: payload_map,
            vectors: Some(vectors_wrapped),
            ..Default::default()
        };

        pts.push(point);
    }

    Ok(pts)
}

/// Flexible reader for `rag_records.jsonl` when strict struct deserialization fails.
///
/// This function accepts various field names:
/// - text: ["text","chunk","chunk_text","content","body","doc","code","message","description"]
/// - id:   ["id","uuid","hash","key","name"] (or stable hash fallback)
/// - src:  ["source","file","path","uri","origin"]
/// - vec:  ["embedding","vector","values","embedding_vector"] (array of numbers)
fn read_rag_records_flexible(jsonl_path: impl AsRef<Path>) -> Result<Vec<RagRecord>, RagError> {
    trace!(
        "ingest::read_rag_records_flexible path={:?}",
        jsonl_path.as_ref()
    );

    let file = File::open(jsonl_path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                trace!("line {}: skip malformed JSON: {}", i + 1, e);
                continue;
            }
        };

        if let Some(rec) = map_any_rag_line(v) {
            out.push(rec);
        } else {
            trace!("line {}: skip (cannot derive text/id)", i + 1);
        }
    }

    trace!(
        "ingest::read_rag_records_flexible parsed={} items",
        out.len()
    );
    Ok(out)
}

/// Maps an arbitrary JSON object to our canonical `RagRecord`.
fn map_any_rag_line(v: Value) -> Option<RagRecord> {
    use Value::{Array, Null, Number, Object, String as JStr};

    // Try common shapes:
    let obj = match &v {
        Object(m) => m,
        _ => return None,
    };

    // text
    let text = pick_str(
        obj,
        &[
            "text",
            "chunk",
            "chunk_text",
            "content",
            "body",
            "doc",
            "code",
            "message",
            "description",
        ],
    )
    .or_else(|| {
        obj.get("payload")
            .and_then(|p| p.get("text"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
    })
    .unwrap_or_else(|| v.to_string()); // fallback to full JSON

    // id
    let id = pick_str(obj, &["id", "uuid", "hash", "key", "name"])
        .or_else(|| {
            obj.get("payload")
                .and_then(|p| pick_str(p.as_object()?, &["id", "uuid", "hash", "key", "name"]))
        })
        .unwrap_or_else(|| stable_hash(&text));

    // source
    let source = pick_str(obj, &["source", "file", "path", "uri", "origin"]).or_else(|| {
        obj.get("payload")
            .and_then(|p| pick_str(p.as_object()?, &["source", "file", "path", "uri", "origin"]))
    });

    // embedding (optional)
    let embedding = pick_vec_f32(obj, &["embedding", "vector", "values", "embedding_vector"])
        .or_else(|| {
            obj.get("payload").and_then(|p| {
                pick_vec_f32(
                    p.as_object()?,
                    &["embedding", "vector", "values", "embedding_vector"],
                )
            })
        });

    // extra payload: keep the original JSON (object) as a BTreeMap
    let extra = to_btree(obj);

    Some(RagRecord {
        id,
        text,
        source,
        embedding,
        extra,
    })
}

/// Picks a string field from object by any of the given keys.
fn pick_str(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

/// Picks a vector<f32> from object by any of the given keys.
fn pick_vec_f32(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<Vec<f32>> {
    for k in keys {
        if let Some(arr) = obj.get(*k).and_then(|v| v.as_array()) {
            let mut out = Vec::with_capacity(arr.len());
            for x in arr {
                if let Some(f) = x.as_f64() {
                    out.push(f as f32);
                } else if let Some(i) = x.as_i64() {
                    out.push(i as f32);
                } else {
                    return None; // mixed/unsupported
                }
            }
            return Some(out);
        }
    }
    None
}

/// Converts an object to a `BTreeMap<String, Value>` (shallow copy).
fn to_btree(obj: &serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Stable, human-friendly hash id for missing identifiers.
fn stable_hash(s: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("rec_{:016x}", h.finish())
}

/// Converts `serde_json::Value` into `qdrant::Value`.
///
/// Strings -> `StringValue`
/// Integers -> `IntegerValue`
/// Floats -> `DoubleValue`
/// Booleans -> `BoolValue`
/// Other (null/arrays/objects) -> stringified `StringValue`
fn json_to_qvalue(v: serde_json::Value) -> QValue {
    use serde_json::Value as J;
    use value::Kind as K;

    match v {
        J::String(s) => QValue {
            kind: Some(K::StringValue(s)),
        },
        J::Number(n) => {
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
        J::Bool(b) => QValue {
            kind: Some(K::BoolValue(b)),
        },
        other => QValue {
            kind: Some(K::StringValue(other.to_string())),
        },
    }
}
