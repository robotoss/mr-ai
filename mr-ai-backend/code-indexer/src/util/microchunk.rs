//! Micro chunking utilities for ANN search.
//!
//! Goals:
//! - Produce stable, overlapping line windows with correct file-level spans.
//! - Provide absolute byte and (row,col) ranges suitable for UI highlighting.
//! - Keep backward compatibility with the original `split_by_lines` signature.
//!
//! Notes:
//! - This module is language-agnostic; role inference can be injected via a closure.
//! - `start_row/col` in `Span` are absolute within the file (recommended).
//!   If you only know the parent symbol's starting row, pass it via `parent_start_row`.

use crate::types::{MicroChunk, Span};
use sha2::{Digest, Sha256};
use tracing::{debug, trace};

/// Split a snippet into overlapping windows by lines (extended API).
///
/// Window boundaries are aligned to source lines, and spans are absolute
/// (in file coordinates) based on `parent_start_byte` and `parent_start_row`.
///
/// # Parameters
/// - `parent_chunk_id`: Stable parent chunk id (will be embedded into micro ids).
/// - `file`: Repo-relative file path.
/// - `symbol_path`: Canonical symbol path (used for navigation).
/// - `snippet`: Source slice corresponding to the parent symbol body (as-is).
/// - `parent_start_byte`: Absolute byte offset of `snippet` within the file.
/// - `parent_start_row`: Absolute start row (0-based) of `snippet` within the file.
/// - `max_lines`: Window height in lines (must be > 0).
/// - `overlap_lines`: Overlap between consecutive windows (0..max_lines).
/// - `role_infer`: Optional closure to infer a semantic role based on a window text.
///
/// # Returns
/// Sequence of `MicroChunk`s ordered by increasing `order`.
///
/// # Panics
/// Never panics; invalid inputs (e.g., `max_lines == 0`) return an empty vector.
fn split_by_lines_ex<F>(
    parent_chunk_id: &str,
    file: &str,
    symbol_path: &str,
    snippet: &str,
    parent_start_byte: usize,
    parent_start_row: usize,
    max_lines: usize,
    overlap_lines: usize,
    mut role_infer: Option<F>,
) -> Vec<MicroChunk>
where
    F: FnMut(&str) -> Option<String>,
{
    if snippet.is_empty() || max_lines == 0 {
        trace!("split_by_lines_ex: empty snippet or zero max_lines; nothing to do");
        return Vec::new();
    }

    // We preserve line terminators by using split_inclusive so that byte math is exact.
    let lines: Vec<&str> = snippet.split_inclusive('\n').collect();
    if lines.is_empty() {
        trace!("split_by_lines_ex: split produced no lines; nothing to do");
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut start_line = 0usize;
    let mut order = 0u32;

    // Step computation with saturation: ensure forward progress.
    let step = max_lines.saturating_sub(overlap_lines).max(1);

    while start_line < lines.len() {
        let end_line = (start_line + max_lines).min(lines.len());

        // Build the text of this window and compute local byte offsets.
        let part: String = lines[start_line..end_line].iter().copied().collect();

        // Local byte offset to the start of the window within the parent snippet.
        let start_byte_local: usize = lines[..start_line].iter().map(|s| s.len()).sum();
        let end_byte_local: usize = start_byte_local + part.len();

        // Absolute byte offsets in the file.
        let abs_start_byte = parent_start_byte + start_byte_local;
        let abs_end_byte = parent_start_byte + end_byte_local;

        // Absolute rows: add the parent's absolute starting row.
        let abs_start_row = parent_start_row + start_line;
        let abs_end_row = parent_start_row + end_line;

        // Columns: start at 0; end column = visible columns of the last line (no trailing '\n').
        let end_col = if end_line > start_line {
            last_line_end_col(lines[end_line - 1])
        } else {
            0
        };

        let span = Span {
            start_byte: abs_start_byte,
            end_byte: abs_end_byte,
            start_row: abs_start_row,
            start_col: 0,
            end_row: abs_end_row,
            end_col,
        };

        let id = micro_id(parent_chunk_id, order, span.start_byte, span.end_byte);
        let content_sha256 = sha_hex(&part);
        let role = role_infer.as_mut().and_then(|f| f(&part));

        out.push(MicroChunk {
            id,
            parent_chunk_id: parent_chunk_id.to_owned(),
            file: file.to_owned(),
            symbol_path: symbol_path.to_owned(),
            order,
            span,
            snippet: part,
            content_sha256,
            // `role` field is available in the updated `MicroChunk` struct.
            role,
        });

        if end_line == lines.len() {
            break;
        }
        start_line = start_line.saturating_add(step);
        order = order.saturating_add(1);
    }

    debug!(
        "split_by_lines_ex: produced {} micro-chunks (max_lines={}, overlap={}) for {}",
        out.len(),
        max_lines,
        overlap_lines,
        symbol_path
    );
    out
}

/// Backward-compatible wrapper that keeps the original signature.
///
/// Differences from `split_by_lines_ex`:
/// - `start_row` is assumed to be 0 (rows in `Span` will be relative to the snippet).
/// - No role inference.
/// - Same absolute byte math via `parent_start_byte`.
fn split_by_lines(
    parent_chunk_id: &str,
    file: &str,
    symbol_path: &str,
    snippet: &str,
    parent_start_byte: usize,
    max_lines: usize,
    overlap_lines: usize,
) -> Vec<MicroChunk> {
    split_by_lines_ex(
        parent_chunk_id,
        file,
        symbol_path,
        snippet,
        parent_start_byte,
        /* parent_start_row */ 0,
        max_lines,
        overlap_lines,
        Option::<fn(&str) -> Option<String>>::None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_split() {
        let s = "a\nb\nc\nd\n";
        let chunks = split_by_lines_ex(
            "parent",
            "lib/a.dart",
            "File::Foo::bar",
            s,
            100, // parent_start_byte
            10,  // parent_start_row
            2,   // max_lines
            1,   // overlap_lines
            None::<fn(&str) -> Option<String>>,
        );
        assert_eq!(chunks.len(), 3);
        // First chunk rows: [10..12), last col = len("b") = 1
        assert_eq!(chunks[0].span.start_row, 10);
        assert_eq!(chunks[0].span.end_row, 12);
        assert_eq!(chunks[0].span.end_col, 1);
        // Monotonic byte ranges.
        assert!(chunks[0].span.start_byte < chunks[0].span.end_byte);
        assert!(chunks[1].span.start_byte < chunks[1].span.end_byte);
    }

    #[test]
    fn role_inference() {
        let s = "if (x) {\n  do();\n}\n";
        let chunks = split_by_lines_ex(
            "p",
            "f.ts",
            "File::Foo::bar",
            s,
            0,
            0,
            3,
            0,
            Some(|txt: &str| {
                if txt.trim_start().starts_with("if ") || txt.trim_start().starts_with("if(") {
                    return Some("if_arm".to_string());
                }
                None
            }),
        );
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].role.as_deref(), Some("if_arm"));
    }
}

/// Compute a lowercase hex SHA-256 of a string.
fn sha_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Build a stable micro-chunk ID from parent, order, and absolute span bytes.
fn micro_id(parent: &str, order: u32, start_byte: usize, end_byte: usize) -> String {
    let mut h = Sha256::new();
    h.update(parent.as_bytes());
    h.update(order.to_le_bytes());
    h.update(start_byte.to_le_bytes());
    h.update(end_byte.to_le_bytes());
    format!("{:x}", h.finalize())
}

/// Infer the end column (UTF-8 bytes) of the last line in a slice of lines.
/// The input `last_line_inclusive` **may** end with '\n'; we trim it when computing `end_col`.
fn last_line_end_col(last_line_inclusive: &str) -> usize {
    // Remove the trailing newline (if any) to get a visible column length.
    let no_nl = last_line_inclusive
        .strip_suffix('\n')
        .unwrap_or(last_line_inclusive);
    no_nl.len()
}
