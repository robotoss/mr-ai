//! Chunking module: splits large AST entities into line-based chunks for RAG.
//!
//! Strategy:
//! - Prefer in-memory [`AstNode.snippet`] if present;
//! - Fallback: slice from file by [`AstNode.span`];
//! - If snippet fits limits → single chunk (index=1,total=1);
//! - If longer → split by lines with overlap from [`GraphConfig.limits`];
//! - Each chunk gets a stable id: `{parent_symbol_id}#c{index}`.
//!
//! Important: `neighbors` in [`RagRecord`] is **not** used for chunk adjacency.
//! It is reserved for graph relations (imports/exports/declares/calls/etc).

use std::fs;

use crate::{
    config::model::GraphConfig,
    model::{
        ast::AstNode,
        graph::GraphEdgeLabel,
        payload::{ChunkMeta, Metrics, RagRecord},
    },
};
use anyhow::Result;
use petgraph::Graph;
use tracing::{info, warn};

/// Split AST nodes into [`RagRecord`]s with chunk metadata.
///
/// Uses parameters from [`GraphConfig.limits`] to control chunk size and overlap:
/// - `max_chunk_lines` — max lines per chunk;
/// - `snippet_context_lines` — number of overlapping lines between consecutive chunks;
/// - `max_chunk_chars` — per-chunk character cap (guards against very long lines).
pub fn chunk_ast_nodes(
    nodes: &[AstNode],
    _graph: &Graph<AstNode, GraphEdgeLabel>,
    config: &GraphConfig,
) -> Result<Vec<RagRecord>> {
    let overlap = config.limits.snippet_context_lines;
    let max_lines = config.limits.max_chunk_lines;
    let max_chars = config.limits.max_chunk_chars;

    info!(
        "chunking: start, nodes={}, max_lines={}, overlap={}, max_chars={}",
        nodes.len(),
        max_lines,
        overlap,
        max_chars
    );

    let mut out: Vec<RagRecord> = Vec::new();

    for n in nodes {
        // 1) Obtain full text for the node.
        let full = match snippet_for_node(n) {
            Some(s) => s,
            None => {
                // Still emit a tiny empty chunk to keep downstream consistent.
                warn!(
                    "chunking: empty snippet for node '{}' ({:?}) in {}",
                    n.name, n.kind, n.file
                );
                out.push(make_record(
                    n,
                    String::new(),
                    ChunkMeta {
                        index: 1,
                        total: 1,
                        parent_id: n.symbol_id.clone(),
                    },
                ));
                continue;
            }
        };

        // 2) Split to chunks.
        let chunks = split_into_chunks(&full, max_lines, overlap, max_chars);

        // 3) Build records (no neighbor links here).
        let total = chunks.len().max(1);
        for (i, body) in chunks.into_iter().enumerate() {
            out.push(make_record(
                n,
                body,
                ChunkMeta {
                    index: i + 1,
                    total,
                    parent_id: n.symbol_id.clone(),
                },
            ));
        }
    }

    info!("chunking: done, records={}", out.len());
    Ok(out)
}

/// Prefer [`AstNode.snippet`]; fallback to slicing the file by span.
///
/// Returns `None` if both snippet and span are empty/unusable.
fn snippet_for_node(n: &AstNode) -> Option<String> {
    if let Some(s) = &n.snippet {
        if !s.is_empty() {
            return Some(s.clone());
        }
    }
    if n.span.end_byte > n.span.start_byte {
        if let Ok(code) = fs::read_to_string(&n.file) {
            let start = n.span.start_byte.min(code.len());
            let end = n.span.end_byte.min(code.len());
            if start < end {
                return code.get(start..end).map(|s| s.to_string());
            }
        }
    }
    None
}

/// Split text into chunks by lines with overlap.
///
/// - `max_lines`: hard cap on lines per chunk;
/// - `overlap`: number of overlapping lines shared between consecutive chunks;
/// - `max_chars`: per-chunk character cap (guards against extremely long lines).
fn split_into_chunks(
    text: &str,
    max_lines: usize,
    overlap: usize,
    max_chars: usize,
) -> Vec<String> {
    let trimmed = text.trim_end_matches('\n');
    let lines: Vec<&str> = trimmed.lines().collect();

    // No split needed.
    if lines.is_empty() {
        return vec![String::new()];
    }
    if lines.len() <= max_lines && trimmed.chars().count() <= max_chars {
        return vec![trim_to_char_cap(trimmed.to_string(), max_chars)];
    }

    let stride = max_lines.saturating_sub(overlap).max(1);
    let mut out: Vec<String> = Vec::new();

    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + max_lines).min(lines.len());
        let mut chunk_text = lines[start..end].join("\n");

        // Guard by char cap (handles very long lines).
        chunk_text = trim_to_char_cap(chunk_text, max_chars);

        out.push(chunk_text);

        if end == lines.len() {
            break;
        }
        start = start.saturating_add(stride);
    }

    out
}

/// Trim a string to `max_chars` by character boundary.
fn trim_to_char_cap(mut s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    let mut cut = 0usize;
    for (i, _) in s.char_indices() {
        if i <= max_chars {
            cut = i;
        } else {
            break;
        }
    }
    s.truncate(cut);
    s
}

/// Build a [`RagRecord`] from an [`AstNode`] and a specific chunk body.
///
/// - `id` is stable per chunk: `{node.symbol_id}#c{index}`
/// - `metrics.loc` equals `snippet.lines().count()`.
fn make_record(n: &AstNode, snippet: String, chunk: ChunkMeta) -> RagRecord {
    RagRecord {
        id: format!("{}#c{}", n.symbol_id, chunk.index),
        path: n.file.clone(),
        language: n.language.to_string(),
        kind: format!("{:?}", n.kind),
        name: n.name.clone(),
        fqn: n.fqn.clone(),
        snippet: snippet.clone(),
        doc: n.doc.clone(),
        signature: n.signature.clone(),
        owner_path: n.owner_path.clone(),
        chunk: Some(chunk),
        neighbors: Vec::new(), // graph relations are added elsewhere
        tags: Vec::new(),
        metrics: Metrics {
            loc: snippet.lines().count(),
            params: 0,
        },
        hash_content: None,
    }
}
