//! Micro chunking utilities for ANN search.

use crate::types::{MicroChunk, Span};
use sha2::{Digest, Sha256};

/// Split a snippet into overlapping windows by lines.
pub fn split_by_lines(
    parent_chunk_id: &str,
    file: &str,
    symbol_path: &str,
    snippet: &str,
    parent_start_byte: usize,
    max_lines: usize,
    overlap_lines: usize,
) -> Vec<MicroChunk> {
    let mut out = Vec::new();
    let lines: Vec<&str> = snippet.split_inclusive('\n').collect();
    if lines.is_empty() {
        return out;
    }
    let mut start_line = 0usize;
    let mut order = 0u32;

    while start_line < lines.len() {
        let end_line = (start_line + max_lines).min(lines.len());
        let part: String = lines[start_line..end_line].iter().copied().collect();
        let start_byte_local: usize = lines[..start_line].iter().map(|s| s.len()).sum();
        let end_byte_local: usize = start_byte_local + part.len();

        let span = Span {
            start_byte: parent_start_byte + start_byte_local,
            end_byte: parent_start_byte + end_byte_local,
            start_row: start_line,
            start_col: 0,
            end_row: end_line,
            end_col: 0,
        };

        let id = micro_id(parent_chunk_id, order, span.start_byte, span.end_byte);
        let content_sha256 = sha(&part);

        out.push(MicroChunk {
            id,
            parent_chunk_id: parent_chunk_id.to_string(),
            file: file.to_string(),
            symbol_path: symbol_path.to_string(),
            order,
            span,
            snippet: part,
            content_sha256,
        });

        if end_line == lines.len() {
            break;
        }
        let step = max_lines.saturating_sub(overlap_lines).max(1);
        start_line += step;
        order += 1;
    }
    out
}

fn sha(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

fn micro_id(parent: &str, order: u32, s: usize, e: usize) -> String {
    let mut h = Sha256::new();
    h.update(parent.as_bytes());
    h.update(order.to_le_bytes());
    h.update(s.to_le_bytes());
    h.update(e.to_le_bytes());
    format!("{:x}", h.finalize())
}
