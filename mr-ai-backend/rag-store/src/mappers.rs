//! Mappers turning heterogeneous JSON lines into canonical `RagRecord`s.
//!
//! These functions convert various graph/AST JSON structures into a common
//! [`RagRecord`] representation used throughout the RAG pipeline.

use crate::normalize::{join_compact, normalize_code_light};
use crate::record::RagRecord;
use serde_json::Value;
use std::collections::BTreeMap;

/// Map AST node JSON into a compact textual chunk.
///
/// Heuristic:
/// - Prefer `"signature"` / `"name"` / `"symbol"` for the main identifier.
/// - Include `"doc"` / `"comment"` / `"documentation"` as inline text.
/// - If `"body"`/`"code"` present, normalize it and append a snippet.
/// - Fall back to `stable_hash` if no ID is present.
///
/// This is intended for **function/method/class** nodes extracted from AST.
pub fn map_ast_node(v: Value, max_chars: usize) -> Option<RagRecord> {
    let obj = v.as_object()?;

    // Pick or synthesize a stable ID
    let id = pick_str(obj, &["id", "uuid", "hash", "name"])
        .map(|s| s.to_string())
        .unwrap_or_else(|| stable_hash(&v));

    let signature = pick_str(obj, &["signature", "name", "symbol"]).unwrap_or("");
    let doc = pick_str(obj, &["doc", "comment", "documentation"]).unwrap_or("");
    let body = pick_str(obj, &["body", "code"]).unwrap_or("");

    // Build the textual representation
    let text = if !body.is_empty() {
        let code = normalize_code_light(body, max_chars.saturating_sub(200));
        format!("{signature} :: {doc}\n{code}\n")
    } else {
        join_compact(&[signature, doc], max_chars)
    };

    let source = pick_str(obj, &["file", "path", "source", "uri"]).map(|s| s.to_string());

    Some(RagRecord {
        id,
        text,
        source,
        embedding: None,
        extra: to_btree(obj),
    })
}

/// Map graph node JSON into a compact chunk.
///
/// Typical fields used:
/// - `"label"` / `"name"` / `"symbol"` → main identifier
/// - `"description"` / `"doc"` → auxiliary description
pub fn map_graph_node(v: Value, max_chars: usize) -> Option<RagRecord> {
    let obj = v.as_object()?;

    let id = pick_str(obj, &["id", "uuid", "hash", "name"])
        .map(|s| s.to_string())
        .unwrap_or_else(|| stable_hash(&v));

    let label = pick_str(obj, &["label", "name", "symbol"]).unwrap_or("");
    let descr = pick_str(obj, &["description", "doc", "comment"]).unwrap_or("");
    let text = join_compact(&[label, descr], max_chars);

    let source = pick_str(obj, &["file", "path", "source", "uri"]).map(|s| s.to_string());

    Some(RagRecord {
        id,
        text,
        source,
        embedding: None,
        extra: to_btree(obj),
    })
}

/// Map graph edge JSON into a relation triple.
///
/// Example output text:
/// `fn_a --calls--> fn_b`
///
/// Heuristic:
/// - `"from"` / `"src"` / `"caller"` / `"a"` → left node
/// - `"label"` / `"relation"` / `"kind"` → edge relation (default `"rel"`)
/// - `"to"` / `"dst"` / `"callee"` / `"b"` → right node
pub fn map_graph_edge(v: Value, max_chars: usize) -> Option<RagRecord> {
    let obj = v.as_object()?;

    let from = pick_str(obj, &["from", "src", "caller", "a"]).unwrap_or("");
    let rel = pick_str(obj, &["label", "relation", "kind"]).unwrap_or("rel");
    let to = pick_str(obj, &["to", "dst", "callee", "b"]).unwrap_or("");

    // Use explicit ID if present, otherwise synthesize edge signature
    let id = pick_str(obj, &["id", "uuid", "hash"])
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("edge:{from}:{rel}:{to}"));

    let hint = pick_str(obj, &["hint", "description", "doc"]).unwrap_or("");
    let text = join_compact(&[from, &format!("--{rel}-->",), to, hint], max_chars);

    let source = pick_str(obj, &["file", "path", "source", "uri"]).map(|s| s.to_string());

    Some(RagRecord {
        id,
        text,
        source,
        embedding: None,
        extra: to_btree(obj),
    })
}

// ----- small helpers -----

/// Picks the first non-empty string among the given keys.
fn pick_str<'a>(obj: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str()) {
            return Some(s);
        }
    }
    None
}

/// Converts a JSON map into a `BTreeMap` for stable ordering.
fn to_btree(obj: &serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    obj.clone().into_iter().collect()
}

/// Computes a stable hash string for arbitrary JSON value.
/// Used when no explicit `"id"` is available.
fn stable_hash(v: &Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    // Using `serde_json::to_string` ensures deterministic ordering
    // (unlike `Value::to_string` which can differ across versions).
    let s = serde_json::to_string(v).unwrap_or_default();
    s.hash(&mut h);

    format!("rec_{:016x}", h.finish())
}
