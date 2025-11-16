//! Async JSONL reader → `(id, embed_text, VectorPayload)` tuples.
//! Streams `code_chunks.jsonl`, builds compact payload + high-signal embed text.

use std::collections::BTreeSet;
use std::path::Path;

use code_indexer::CodeChunk;
use regex::Regex;
use serde::Serialize;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, info};

use crate::embedding::{build_embedding_text, clamp_snippet_ex};
use crate::errors::rag_base_error::RagBaseError;
use crate::structs::rag_store::VectorPayload;

/// Stream a JSONL file in batches and invoke `on_batch` for each non-empty batch.
pub async fn read_jsonl_map_to_ingest_batched<P, F, Fut>(
    path: P,
    batch_size: usize,
    preview_max_snippet_chars: usize,
    embed_max_snippet_chars: usize,
    mut on_batch: F,
) -> Result<(), RagBaseError>
where
    P: AsRef<Path>,
    F: FnMut(Vec<(String, String, VectorPayload)>) -> Fut,
    Fut: std::future::Future<Output = Result<(), RagBaseError>>,
{
    let path_ref = path.as_ref();
    info!(
        target: "rag_base::jsonl_reader",
        path = %path_ref.display(),
        batch_size,
        "read_jsonl_map_to_ingest_batched: start"
    );

    let file = File::open(path_ref).await?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let mut buf = Vec::with_capacity(batch_size.max(1));
    let mut total_lines: usize = 0;
    let mut mapped_lines: usize = 0;

    while let Some(line) = lines.next_line().await? {
        total_lines += 1;
        if let Some(triple) =
            map_line_to_triple(&line, preview_max_snippet_chars, embed_max_snippet_chars)
        {
            mapped_lines += 1;
            buf.push(triple);
        }
        if buf.len() >= batch_size {
            debug!(
                target: "rag_base::jsonl_reader",
                buffered = buf.len(),
                "read_jsonl_map_to_ingest_batched: flushing batch"
            );
            on_batch(std::mem::take(&mut buf)).await?;
        }
    }

    if !buf.is_empty() {
        debug!(
            target: "rag_base::jsonl_reader",
            buffered = buf.len(),
            "read_jsonl_map_to_ingest_batched: flushing final batch"
        );
        on_batch(buf).await?;
    }

    info!(
        target: "rag_base::jsonl_reader",
        total_lines,
        mapped_lines,
        "read_jsonl_map_to_ingest_batched: finished"
    );

    Ok(())
}

/// Map one JSONL line (parsed as `CodeChunk`) into `(id, embed_text, VectorPayload)`.
fn map_line_to_triple(
    line: &str,
    preview_max_snippet_chars: usize,
    embed_max_snippet_chars: usize,
) -> Option<(String, String, VectorPayload)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let chunk: CodeChunk = serde_json::from_str(trimmed).ok()?;
    if chunk.id.is_empty() {
        return None;
    }

    // language/kind → stable snake_case via serde
    let language = enum_to_snake(&chunk.language);
    let kind = enum_to_snake(&chunk.kind);

    // Top imports: legacy `imports` + `graph.imports_out`
    let mut imports_top: Vec<String> = Vec::new();
    imports_top.extend(chunk.imports.iter().cloned());
    if let Some(graph) = &chunk.graph {
        if !graph.imports_out.is_empty() {
            imports_top.extend(graph.imports_out.iter().cloned());
        }
    }
    imports_top.sort();
    imports_top.dedup();
    if imports_top.len() > 8 {
        imports_top.truncate(8);
    }

    // Routes: prefer graph.facts.routes, also extract generically from text
    let mut routes: Vec<String> = chunk
        .graph
        .as_ref()
        .and_then(|g| g.facts.get("routes"))
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.into_iter()
                .filter_map(|j| j.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if let Some(sn) = &chunk.snippet {
        routes.extend(extract_routes_from_text(sn));
    }
    if let Some(sig) = &chunk.signature {
        routes.extend(extract_routes_from_text(sig));
    }
    routes.extend(extract_routes_from_text(&chunk.file));
    routes.sort();
    routes.dedup();

    // Keywords: hints.keywords + generic extraction (both raw tokens and decomposed)
    let mut keywords: Vec<String> = chunk
        .hints
        .as_ref()
        .map(|h| h.keywords.clone())
        .unwrap_or_default();
    if let Some(sn) = &chunk.snippet {
        keywords.extend(extract_keywords_from_text(sn));
    }
    if let Some(sig) = &chunk.signature {
        keywords.extend(extract_keywords_from_text(sig));
    }
    keywords.extend(extract_keywords_from_text(&chunk.file));
    keywords.sort();
    keywords.dedup();
    if keywords.len() > 32 {
        keywords.truncate(32);
    }

    // LSP enrichment
    let (lsp_fqn, tags, lsp_signature): (Option<String>, Vec<String>, Option<String>) =
        if let Some(lsp) = &chunk.lsp {
            let t: Vec<String> = set_to_vec(&lsp.tags);
            (lsp.fqn.clone(), t, lsp.signature.clone())
        } else {
            (None, Vec::new(), None)
        };

    // First doc line only for preview
    let doc_first = chunk
        .doc
        .as_ref()
        .and_then(|d| d.lines().next().map(|s| s.to_string()));

    // Short preview snippet in payload (uses preview_max_snippet_chars)
    let snippet_preview = chunk
        .snippet
        .as_ref()
        .map(|s| clamp_snippet_ex(s, preview_max_snippet_chars, 50, true));

    // Build search_blob: text field for pure lexical search / filtering.
    let search_blob = build_search_blob(
        &chunk.file,
        &chunk.symbol_path,
        chunk.signature.as_deref().or(lsp_signature.as_deref()),
        chunk.snippet.as_deref(),
        doc_first.as_deref(),
        &imports_top,
        &routes,
        &keywords,
    );

    // Lightweight payload
    let payload = VectorPayload {
        id: chunk.id.clone(),
        file: chunk.file.clone(),
        language: language.clone(),
        kind: kind.clone(),
        symbol: chunk.symbol.clone(),
        symbol_path: chunk.symbol_path.clone(),
        signature: chunk.signature.clone().or(lsp_signature),
        doc: doc_first,
        snippet: snippet_preview,
        content_sha256: chunk.content_sha256.clone(),
        imports_top: imports_top.clone(),
        tags,
        lsp_fqn,
        is_definition: chunk.is_definition,
        routes: routes.clone(),
        search_terms: keywords.clone(),
        search_blob,
    };

    // Embedding text (uses embed_max_snippet_chars)
    let embed_text = build_embedding_text(
        &language,
        &kind,
        &payload.symbol_path,
        payload.signature.as_deref(),
        payload.doc.as_deref(),
        payload.snippet.as_deref(),
        &imports_top,
        &routes,
        &keywords,
        embed_max_snippet_chars,
    );

    Some((chunk.id, embed_text, payload))
}

#[inline]
fn enum_to_snake<T: Serialize>(e: &T) -> String {
    let s = serde_json::to_string(e).unwrap_or_else(|_| "\"unknown\"".into());
    s.trim_matches('"').to_string()
}

#[inline]
fn set_to_vec(set: &BTreeSet<String>) -> Vec<String> {
    set.iter().cloned().collect()
}

fn extract_routes_from_text(s: &str) -> Vec<String> {
    let mut out = Vec::new();

    // Quoted path literals
    if let Ok(re_q) = Regex::new(r#"['"](/[\w\-./:?=&%+*@!$',\[\]{}:]*?)['"]"#) {
        for cap in re_q.captures_iter(s) {
            if let Some(m) = cap.get(1) {
                let p = m.as_str();
                if looks_like_route(p) {
                    out.push(p.to_string());
                }
            }
        }
    }

    // Bare paths starting with `/`
    if let Ok(re_bare) = Regex::new(r"(?P<p>/[\w\-./:?=&%+*@!$'\[\]{}:]+)") {
        for cap in re_bare.captures_iter(s) {
            if let Some(m) = cap.name("p") {
                let p = m.as_str();
                if looks_like_route(p) {
                    out.push(p.to_string());
                }
            }
        }
    }

    out
}

#[inline]
fn looks_like_route(p: &str) -> bool {
    if !p.starts_with('/') || p.trim_matches('/').is_empty() {
        return false;
    }
    true
}

/// Extract compact keyword set from text, including both raw tokens and decomposed forms.
///
/// This helper is tuned for code/text hybrid search:
/// - keeps raw tokens like `initialLocation`, `/games`, `go_router`.
/// - additionally splits on `/ . - _` and camelCase to improve recall for partial matches.
/// - drops purely numeric tokens and tokens shorter than 3 chars.
fn extract_keywords_from_text(s: &str) -> Vec<String> {
    let mut toks: Vec<String> = Vec::new();

    // Raw token detection: identifiers, paths, etc.
    if let Ok(re) = Regex::new(r"[A-Za-z0-9_./-]{3,}") {
        for m in re.find_iter(s) {
            let raw = m.as_str();
            let raw_lower = raw.to_lowercase();

            // Keep the full raw token as-is (normalized to lower-case).
            if raw_lower.len() >= 3 && !raw_lower.chars().all(|c| c.is_ascii_digit()) {
                toks.push(raw_lower.clone());
            }

            // Split by structural delimiters and camelCase.
            for piece in raw.split(|c| c == '/' || c == '.' || c == '-' || c == '_') {
                if piece.len() < 3 {
                    continue;
                }
                for part in split_camel_case(piece) {
                    let t = part.to_lowercase();
                    if t.len() >= 3 && !t.chars().all(|c| c.is_ascii_digit()) {
                        toks.push(t);
                    }
                }
            }
        }
    }

    toks.sort();
    toks.dedup();
    if toks.len() > 48 {
        toks.truncate(48);
    }
    toks
}

/// Build a flat text blob for purely lexical search / indexing.
///
/// It intentionally содержит:
/// - file, symbol_path
/// - signature, snippet, doc
/// - imports, routes, keywords
fn build_search_blob(
    file: &str,
    symbol_path: &str,
    signature: Option<&str>,
    snippet: Option<&str>,
    doc: Option<&str>,
    imports_top: &[String],
    routes: &[String],
    keywords: &[String],
) -> String {
    let mut buf = String::new();
    buf.push_str(file);
    buf.push('\n');
    buf.push_str(symbol_path);
    buf.push('\n');

    if let Some(sig) = signature {
        buf.push_str(sig);
        buf.push('\n');
    }
    if let Some(sn) = snippet {
        buf.push_str(sn);
        buf.push('\n');
    }
    if let Some(d) = doc {
        buf.push_str(d);
        buf.push('\n');
    }
    if !imports_top.is_empty() {
        buf.push_str(&imports_top.join(" "));
        buf.push('\n');
    }
    if !routes.is_empty() {
        buf.push_str(&routes.join(" "));
        buf.push('\n');
    }
    if !keywords.is_empty() {
        buf.push_str(&keywords.join(" "));
        buf.push('\n');
    }

    buf
}

fn split_camel_case(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let bytes = s.as_bytes();
    for i in 1..bytes.len() {
        let c_prev = bytes[i - 1] as char;
        let c = bytes[i] as char;
        if c.is_uppercase() && (c_prev.is_lowercase() || c_prev.is_ascii_digit()) {
            parts.push(&s[start..i]);
            start = i;
        }
    }
    parts.push(&s[start..]);
    parts
}
