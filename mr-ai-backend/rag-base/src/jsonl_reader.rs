//! Async JSONL reader utilities for `code_chunks.jsonl`.
//!
//! # What this module provides
//! - `read_jsonl_map_to_ingest`: read the whole file asynchronously and map
//!   each JSON line into `(id, text_to_embed, VectorPayload)`.
//! - `read_jsonl_map_to_ingest_batched`: process the file in **batches** and
//!   invoke an async callback for each batch (memory-friendly, backpressure-aware).
//!
//! # Why no persistent "stream" struct?
//! Many ingestion flows are single-shot per command. Exposing stateless async
//! functions keeps call sites simple while still leveraging incremental reading
//! with `tokio` and buffered I/O under the hood.

use std::path::Path;

use serde_json::Value;
use tokio::fs::File;
use tokio::io::{self, AsyncBufReadExt, BufReader};

use crate::embedding::build_embedding_text;
use crate::errors::rag_base_error::RagBaseError;
use crate::structs::rag_store::VectorPayload;

/// Read the entire JSONL file **asynchronously** and map each line into a triple:
/// `(id, text_to_embed, VectorPayload)`.
///
/// Lines that fail to parse or miss the `id` field are **skipped** to keep the
/// ingestion robust. This function is convenient for small/medium files.
///
/// If you index large files, prefer [`read_jsonl_map_to_ingest_batched`] with a
/// per-batch async callback to avoid holding everything in memory.
///
/// # Errors
/// Returns `RagBaseError::Io` on I/O errors.
///
/// # Example
/// ```no_run
/// # use rag_base::jsonl_reader::read_jsonl_map_to_ingest;
/// # use rag_base::errors::rag_base_error::RagBaseError;
/// # async fn demo() -> Result<(), RagBaseError> {
/// let triples = read_jsonl_map_to_ingest("code_data/out/project_x/code_chunks.jsonl", 4000).await?;
/// // triples: Vec<(String /*id*/, String /*embed text*/, VectorPayload)>
/// # Ok(()) }
/// ```
pub async fn read_jsonl_map_to_ingest<P: AsRef<Path>>(
    path: P,
    max_snippet_chars: usize,
) -> Result<Vec<(String, String, VectorPayload)>, RagBaseError> {
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let mut out = Vec::new();

    while let Some(line) = lines.next_line().await? {
        if let Some(triple) = map_line_to_triple(&line, max_snippet_chars) {
            out.push(triple);
        }
    }

    Ok(out)
}

/// Read the JSONL file **asynchronously in batches** and call `on_batch` for each
/// produced batch of `(id, text_to_embed, VectorPayload)`.
///
/// This is the recommended method for large inputs: you keep memory bounded and can
/// apply backpressure in `on_batch` (e.g., embed → upsert → await).
///
/// Lines that cannot be parsed or that miss the `id` are **skipped**.
///
/// # Arguments
/// - `path`: path to the JSONL file
/// - `batch_size`: maximum number of lines to group into a single callback invocation
/// - `max_snippet_chars`: snippet clamp budget passed to `build_embedding_text`
/// - `on_batch`: async handler that consumes a batch; returning an error aborts the read
///
/// # Errors
/// Returns `RagBaseError::Io` on I/O errors or propagates any error from `on_batch`.
///
/// # Example
/// ```no_run
/// # use rag_base::jsonl_reader::read_jsonl_map_to_ingest_batched;
/// # use rag_base::errors::rag_base_error::RagBaseError;
/// # async fn demo() -> Result<(), RagBaseError> {
/// read_jsonl_map_to_ingest_batched(
///     "code_data/out/project_x/code_chunks.jsonl",
///     256,
///     4000,
///     |batch| async move {
///         // embed & upsert here
///         // e.g., let texts: Vec<_> = batch.iter().map(|(_, t, _)| t.clone()).collect();
///         Ok(())
///     },
/// ).await
/// # }
/// ```
pub async fn read_jsonl_map_to_ingest_batched<P, F, Fut>(
    path: P,
    batch_size: usize,
    max_snippet_chars: usize,
    mut on_batch: F,
) -> Result<(), RagBaseError>
where
    P: AsRef<Path>,
    F: FnMut(Vec<(String, String, VectorPayload)>) -> Fut,
    Fut: std::future::Future<Output = Result<(), RagBaseError>>,
{
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let mut buf = Vec::with_capacity(batch_size.max(1));

    while let Some(line) = lines.next_line().await? {
        if let Some(triple) = map_line_to_triple(&line, max_snippet_chars) {
            buf.push(triple);
        }
        if buf.len() >= batch_size {
            on_batch(std::mem::take(&mut buf)).await?;
        }
    }

    if !buf.is_empty() {
        on_batch(buf).await?;
    }

    Ok(())
}

/// Map a single JSON line into `(id, text_to_embed, VectorPayload)`.
///
/// Returns `None` when JSON is invalid or `id` is missing.
fn map_line_to_triple(
    line: &str,
    max_snippet_chars: usize,
) -> Option<(String, String, VectorPayload)> {
    let val: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return None,
    };

    // Require `id`; skip otherwise.
    let id = val.get("id")?.as_str()?.to_string();

    let file = str_field(&val, "file");
    let language = str_field(&val, "language");
    let kind = str_field(&val, "kind");
    let symbol = str_field(&val, "symbol");
    let symbol_path = str_field(&val, "symbol_path");
    let signature = opt_str_field(&val, "signature");
    let doc = opt_str_field(&val, "doc");
    let snippet = opt_str_field(&val, "snippet");
    let content_sha256 = str_field(&val, "content_sha256");

    // imports: combine top-level `imports` and `graph.imports_out`
    let mut imports = str_array_field(&val, &["imports"]);
    if let Some(graph) = val.get("graph") {
        let extra = str_array_from_path(graph, &["imports_out"]);
        if !extra.is_empty() {
            imports.extend(extra);
        }
    }

    // lsp_fqn & tags (prefer lsp.tags, fallback to top-level tags)
    let (lsp_fqn, tags) = if let Some(lsp) = val.get("lsp") {
        let fqn = opt_str_from_path(lsp, &["fqn"]);
        let mut t = str_array_from_path(lsp, &["tags"]);
        if t.is_empty() {
            t = str_array_field(&val, &["tags"]);
        }
        (fqn, t)
    } else {
        let t = str_array_field(&val, &["tags"]);
        (None, t)
    };

    // Build payload
    let payload = VectorPayload {
        id: id.clone(),
        file,
        language: language.clone(),
        kind: kind.clone(),
        symbol,
        symbol_path: symbol_path.clone(),
        signature: signature.clone(),
        doc: doc.clone(),
        snippet: snippet.clone(),
        content_sha256,
        imports: imports.clone(),
        lsp_fqn,
        tags,
    };

    // Build embedding text
    let embed_text = build_embedding_text(
        &language,
        &kind,
        &symbol_path,
        signature.as_deref(),
        doc.as_deref(),
        snippet.as_deref(),
        &imports,
        max_snippet_chars,
    );

    Some((id, embed_text, payload))
}

// ──────────────────────────────────────────────────────────────────────────
// JSON helpers (defensive extraction)
// ──────────────────────────────────────────────────────────────────────────

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string()
}

fn opt_str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|s| s.as_str()).map(|s| s.to_string())
}

fn str_array_field(v: &Value, path: &[&str]) -> Vec<String> {
    let mut cur = v;
    for k in path {
        match cur.get(*k) {
            Some(next) => cur = next,
            None => return Vec::new(),
        }
    }
    if let Some(arr) = cur.as_array() {
        arr.iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect()
    } else {
        Vec::new()
    }
}

fn str_array_from_path(v: &Value, path: &[&str]) -> Vec<String> {
    let mut cur = v;
    for k in path {
        match cur.get(*k) {
            Some(next) => cur = next,
            None => return Vec::new(),
        }
    }
    if let Some(arr) = cur.as_array() {
        arr.iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect()
    } else {
        Vec::new()
    }
}

fn opt_str_from_path(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    cur.as_str().map(|s| s.to_string())
}
