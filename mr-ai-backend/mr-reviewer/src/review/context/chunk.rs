//! Line-based chunking utilities for large code containers.

use crate::review::context::types::ChunkRef;

/// Line-based chunk descriptor.
///
/// Indices are 1-based to match editor displays and DIFF anchors.
#[derive(Clone, Debug)]
pub struct LineChunk {
    /// 1-based index of this chunk.
    pub index: usize,
    /// Total number of chunks.
    pub total: usize,
    /// Inclusive start line (1-based).
    pub start_line: usize,
    /// Inclusive end line (1-based).
    pub end_line: usize,
}

/// Split text into line-based chunks of at most `max_lines` lines.
///
/// # Parameters
/// - `text`: Full text to split.
/// - `max_lines`: Maximum number of lines per chunk (> 0).
/// - `parent_id`: Stable identifier of the parent entity (e.g., AST symbol id).
///
/// # Returns
/// Tuple of `LineChunk` descriptors and serializable `ChunkRef`s.
///
/// # Panics
/// Panics if `max_lines == 0`.
pub fn chunk_by_lines(
    text: &str,
    max_lines: usize,
    parent_id: &str,
) -> (Vec<LineChunk>, Vec<ChunkRef>) {
    assert!(max_lines > 0, "max_lines must be > 0");
    let total_lines = text.lines().count().max(1);
    let total = (total_lines + max_lines - 1) / max_lines;

    let mut chunks = Vec::with_capacity(total);
    let mut refs = Vec::with_capacity(total);
    for i in 0..total {
        let index = i + 1;
        let start_line = i * max_lines + 1;
        let end_line = ((i + 1) * max_lines).min(total_lines);
        chunks.push(LineChunk {
            index,
            total,
            start_line,
            end_line,
        });
        refs.push(ChunkRef {
            index,
            total,
            parent_id: parent_id.to_string(),
        });
    }
    (chunks, refs)
}

/// Extract snippet between given inclusive line range.
///
/// # Parameters
/// - `full_text`: Original text.
/// - `start_line`: Inclusive start line (1-based).
/// - `end_line`: Inclusive end line (1-based).
///
/// # Returns
/// Substring covering the selected lines joined by `\n`.
pub fn extract_lines(full_text: &str, start_line: usize, end_line: usize) -> String {
    full_text
        .lines()
        .enumerate()
        .filter(|(i, _)| {
            let ln = i + 1;
            ln >= start_line && ln <= end_line
        })
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pick the chunk that intersects with the given [start..=end] range.
///
/// Returns `1` if no chunk intersects.
pub fn pick_chunk_for_anchor(
    chunks: &[LineChunk],
    head_anchor_start: usize,
    head_anchor_end: usize,
) -> usize {
    for ch in chunks {
        if !(head_anchor_end < ch.start_line || head_anchor_start > ch.end_line) {
            return ch.index;
        }
    }
    1
}
