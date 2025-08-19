//! End-to-end ingestion pipeline: read JSONL → normalize → resolve vectors → upsert into Qdrant.
//!
//! Sources: `rag_records.jsonl`, `ast_nodes.jsonl`, `graph_nodes.jsonl`, `graph_edges.jsonl`.
//! Embeddings are resolved via policy or computed dynamically.
//! Final structure stored in Qdrant is a vector + compact payload (text + metadata).

use crate::config::{RagConfig, VectorSpace};
use crate::discovery::{latest_dump_dir, rag_records_path, read_dump_summary};
use crate::embed::{EmbeddingPolicy, EmbeddingsProvider};
use crate::embed_pool::embed_missing;
use crate::errors::RagError;
use crate::io_jsonl::{read_all_jsonl, read_all_records};
use crate::mappers::{map_ast_node, map_graph_edge, map_graph_node};
use crate::normalize::normalize_code_light;
use crate::qdrant_facade::QdrantFacade;
use crate::record::{RagRecord, clamp_snippet};

use indicatif::{ProgressBar, ProgressStyle};
use qdrant_client::qdrant::{
    ListValue, PointId, PointStruct, Struct, Value as QValue, Vector, Vectors, value, vectors,
};
use serde_json::Value;
use services::uuid::stable_uuid;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use tracing::{debug, info, warn};

/// Ingest the latest dump under `<root>/project_x/graphs_data/<timestamp>`.
/// Uses [`ingest_file`] internally.
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

/// Ingests records from `rag_records.jsonl`.
///
/// 1. Try strict schema (`read_all_records`).
/// 2. Fallback to loose JSONL parsing + [`map_any_rag_line`].
/// 3. Normalize text, ensure collection exists, upsert in batches.
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

    // Normalize text for compact embeddings
    let max_chars = chunk_max_chars();
    for r in &mut records {
        r.text = normalize_code_light(&r.text, max_chars);
    }

    let vector_size = determine_vector_size(&records, &policy, cfg.embedding_dim).await?;
    debug!("Vector size determined: {}", vector_size);

    client
        .ensure_collection(&VectorSpace {
            size: vector_size,
            distance: cfg.distance,
        })
        .await?;

    // Upsert points in batches
    let mut total: u64 = 0;
    let batch_size = cfg.upsert_batch.max(1);
    for chunk in records.chunks(batch_size) {
        let points = build_points(chunk, vector_size, &policy).await?;
        total += client.upsert_points(points).await?;
    }

    info!("Ingested {} records from file", total);
    Ok(total)
}

/// Ingests **all files** from the latest dump and computes embeddings for everything.
///
/// - `rag_records.jsonl`
/// - `ast_nodes.jsonl`
/// - `graph_nodes.jsonl`
/// - `graph_edges.jsonl`
///
/// Uses [`embed_missing`] to fill vectors, then upserts into Qdrant with progress bar.
pub async fn ingest_latest_all_embedded(
    cfg: &RagConfig,
    root: impl AsRef<std::path::Path>,
    provider: &(dyn EmbeddingsProvider + Send + Sync),
    client: &QdrantFacade,
) -> Result<u64, RagError> {
    info!(
        "Ingesting latest dump with embeddings from {:?}",
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

    // Progress bar for batch uploads
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

/// Try parsing with strict schema, fallback to flexible JSONL mapper.
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

/// Determine the embedding dimensionality.
/// Uses provided config, or checks precomputed vectors, or queries provider.
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
/// Embedding is resolved via policy. Payload is compact and consistent.
async fn build_points(
    chunk: &[RagRecord],
    vector_size: usize,
    policy: &EmbeddingPolicy<'_>,
) -> Result<Vec<PointStruct>, RagError> {
    let mut pts = Vec::with_capacity(chunk.len());

    for r in chunk {
        // --- resolve embedding ---
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

        // --- payload ---
        let mut payload: HashMap<String, QValue> = HashMap::new();

        // canon: text (for embeddings)
        payload.insert("text".into(), qstring(&r.text));

        // canon: source (prefer record.source; fallback to path in extra)
        if let Some(src) = r.source.clone().or_else(|| {
            r.extra
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }) {
            payload.insert("source".into(), qstring(&src));
        }

        // canon: eid (original id for graph-fanout later)
        payload.insert("eid".into(), qstring(&r.id));

        // canon: language/kind/fqn
        if let Some(lang) = r.extra.get("language").and_then(|v| v.as_str()) {
            payload.insert("language".into(), qstring(lang));
        }
        if let Some(kind) = r.extra.get("kind").and_then(|v| v.as_str()) {
            payload.insert("kind".into(), qstring(kind));
        }
        if let Some(fqn) = r.extra.get("fqn").and_then(|v| v.as_str()) {
            payload.insert("fqn".into(), qstring(fqn));
        }

        // canon: snippet (trimmed)
        if let Some(raw_snippet) = r
            .extra
            .get("snippet")
            .and_then(|v| v.as_str())
            .or_else(|| r.extra.get("body").and_then(|v| v.as_str()))
            .or_else(|| r.extra.get("code").and_then(|v| v.as_str()))
        {
            let max_chars = std::env::var("SNIPPET_MAX_CHARS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2000);
            let max_lines = std::env::var("SNIPPET_MAX_LINES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(120);
            let sn = clamp_snippet(raw_snippet, max_chars, max_lines);
            if !sn.is_empty() {
                payload.insert("snippet".into(), qstring(&sn));
            }
        }

        // canon: tags
        if let Some(tags) = r.extra.get("tags").and_then(|v| v.as_array()) {
            let arr: Vec<QValue> = tags
                .iter()
                .filter_map(|x| x.as_str())
                .map(qstring)
                .collect();
            if !arr.is_empty() {
                payload.insert(
                    "tags".into(),
                    QValue {
                        kind: Some(value::Kind::ListValue(qdrant_client::qdrant::ListValue {
                            values: arr,
                        })),
                    },
                );
            }
        }

        // canon: neighbors, metrics, owner_path (if have)
        if let Some(neigh) = r.extra.get("neighbors") {
            payload.insert("neighbors".into(), json_to_qvalue(neigh.clone()));
        }
        if let Some(metrics) = r.extra.get("metrics") {
            payload.insert("metrics".into(), json_to_qvalue(metrics.clone()));
        }
        if let Some(owner) = r.extra.get("owner_path") {
            payload.insert("owner_path".into(), json_to_qvalue(owner.clone()));
        }

        // --- stable point id ---
        let pid: PointId = stable_uuid(&r.id).to_string().into();

        // --- vector wrapper ---
        let vectors = Vectors {
            vectors_options: Some(vectors::VectorsOptions::Vector(Vector {
                data: vector,
                indices: None,
                vectors_count: None,
                vector: None,
            })),
        };

        pts.push(PointStruct {
            id: Some(pid),
            payload,
            vectors: Some(vectors),
            ..Default::default()
        });
    }

    Ok(pts)
}

/// Wraps a string into Qdrant `Value`.
fn qstring(s: &str) -> QValue {
    QValue {
        kind: Some(value::Kind::StringValue(s.to_string())),
    }
}

/// Converts `serde_json::Value` into Qdrant `Value` (handles arrays/objects).
fn json_to_qvalue(v: serde_json::Value) -> QValue {
    use value::Kind as K;
    match v {
        serde_json::Value::String(s) => QValue {
            kind: Some(K::StringValue(s)),
        },
        serde_json::Value::Number(n) => {
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
        serde_json::Value::Bool(b) => QValue {
            kind: Some(K::BoolValue(b)),
        },
        serde_json::Value::Array(arr) => {
            let vals: Vec<QValue> = arr.into_iter().map(json_to_qvalue).collect();
            QValue {
                kind: Some(K::ListValue(ListValue { values: vals })),
            }
        }
        serde_json::Value::Object(map) => {
            let fields = map
                .into_iter()
                .map(|(k, v)| (k, json_to_qvalue(v)))
                .collect();
            QValue {
                kind: Some(K::StructValue(Struct { fields })),
            }
        }
        serde_json::Value::Null => QValue { kind: None },
    }
}

/// Deduplicate records by `(source,text)` to avoid duplicates in Qdrant.
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

/// Compose embedding text from signature → snippet → doc.
/// Deduplicates and trims.
fn compose_text(
    signature: Option<&str>,
    snippet: Option<&str>,
    doc: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for s in [signature, snippet, doc].into_iter().flatten() {
        let s = s.trim();
        if !s.is_empty() {
            parts.push(s);
        }
    }
    if parts.is_empty() {
        return None;
    }
    parts.dedup();
    Some(parts.join("\n"))
}

/// Quick filter to skip structural nodes like imports/exports.
fn should_index_kind(kind: &str) -> bool {
    matches!(kind, "Function" | "Method" | "Class" | "File")
}

/// Flexible JSONL mapper into `RagRecord` for fallback ingestion.
/// Tries to preserve `snippet` (code) in `extra["snippet"]`, and builds
/// a compact `text` (signature + doc OR kind+name+file) for embeddings.
fn map_any_rag_line(v: serde_json::Value) -> Option<RagRecord> {
    let obj = v.as_object()?;

    // id
    let id = pick_str(obj, &["id", "uuid", "hash"])
        .map(|s| s.to_string())
        .or_else(|| pick_str(obj, &["name"]).map(|s| s.to_string()))
        .unwrap_or_else(|| stable_hash(&v));

    // kind filter (skip imports/exports)
    let kind = pick_str(obj, &["kind"]).unwrap_or("");
    if matches!(kind, "Import" | "Export") {
        return None;
    }

    // primary fields
    let signature = pick_str(obj, &["signature"]);
    let snippet = pick_str(obj, &["snippet"])
        .or_else(|| pick_str(obj, &["body"]))
        .or_else(|| pick_str(obj, &["code"]));
    let doc = pick_str(obj, &["doc", "comment", "documentation"]);
    let name_fqn = pick_str(obj, &["name", "fqn"]);
    let path = pick_str(obj, &["path", "file", "source", "uri"]);

    // text for embeddings: signature + doc, otherwise kind+name(+file)
    let text = if signature.is_some() || doc.is_some() {
        [signature, doc]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else if let Some(name) = name_fqn {
        if !kind.is_empty() && path.is_some() {
            format!("{kind} {name} (file: {})", path.unwrap())
        } else if !kind.is_empty() {
            format!("{kind} {name}")
        } else {
            name.to_string()
        }
    } else {
        // last chance
        v.to_string()
    };

    if text.trim().is_empty() {
        return None;
    }

    // source
    let source = path.map(|s| s.to_string());

    // embedding (if there is suddenly)
    let embedding = pick_vec_f32(obj, &["embedding", "vector", "values", "embedding_vector"]);

    // extra: take everything + put the found snippet in explicit form
    let mut extra: BTreeMap<String, serde_json::Value> =
        obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    if let Some(sn) = snippet {
        extra.insert(
            "snippet".to_string(),
            serde_json::Value::String(sn.to_string()),
        );
    }

    Some(RagRecord {
        id,
        text,
        source,
        embedding,
        extra,
    })
}

/// Pick string by keys from JSON map.
fn pick_str<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            return Some(s);
        }
    }
    None
}

/// Pick vector<f32> by keys from JSON map.
fn pick_vec_f32(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<Vec<f32>> {
    for k in keys {
        if let Some(a) = obj.get(*k).and_then(|v| v.as_array()) {
            let mut out = Vec::with_capacity(a.len());
            for x in a {
                if let Some(f) = x.as_f64() {
                    out.push(f as f32);
                } else if let Some(i) = x.as_i64() {
                    out.push(i as f32);
                }
            }
            return Some(out);
        }
    }
    None
}

/// Fallback stable hash when no `id` present.
fn stable_hash(v: &Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    v.to_string().hash(&mut h);
    format!("rec_{:016x}", h.finish())
}
