//! Parsing helpers for Dart LSP responses into a normalized form.

use crate::types::Span;
use std::{collections::BTreeMap, usize};

/// Normalized LSP symbol entry for merging into CodeChunk.lsp.
#[derive(Debug, Clone)]
pub struct LspSymbolInfo {
    pub file: String,
    pub range: Span,
    pub signature: Option<String>,
    pub definition_uri: Option<String>,
    pub references_count: Option<u32>,
    pub semantic_hist: Option<BTreeMap<String, u32>>,
    pub outline_code_range: Option<(usize, usize)>,
    pub flags: Vec<String>,
}

/// Convert LSP (line, character in UTF-16 code units) to byte offset in `code`.
/// This walks the line, counting UTF-16 code units per char until we reach `character`.
pub fn lsp_pos_to_byte(code: &str, line: usize, character_utf16: usize) -> usize {
    // Clamp line
    let mut cur_line: usize = 0;
    let mut byte_index = 0usize;
    for l in code.split_inclusive('\n') {
        if cur_line == line {
            // Advance within this line by UTF-16 units
            let mut u16_count: usize = 0;
            for (idx, ch) in l.char_indices() {
                // Stop before newline char
                if ch == '\n' {
                    break;
                }
                let mut tmp = [0u16; 2];
                let enc = ch.encode_utf16(&mut tmp);
                u16_count += enc.len();
                if u16_count > character_utf16 {
                    // The target lies before the end of this char; position is at char start.
                    byte_index += idx;
                    return byte_index.min(code.len());
                }
                if u16_count == character_utf16 {
                    byte_index += idx + ch.len_utf8();
                    return byte_index.min(code.len());
                }
            }
            // If requested character beyond line end, snap to line end (before '\n')
            byte_index += l.len().saturating_sub(1); // exclude '\n' if present
            return byte_index.min(code.len());
        } else {
            byte_index += l.len();
            cur_line += 1;
        }
    }
    // If line is beyond file, snap to end
    code.len()
}

/// Convert LSP Range to our Span (byte offsets + line/col from LSP).
pub fn lsp_range_to_span(
    code: &str,
    start_line: usize,
    start_char_u16: usize,
    end_line: usize,
    end_char_u16: usize,
) -> Span {
    let s_byte = lsp_pos_to_byte(code, start_line, start_char_u16);
    let e_byte = lsp_pos_to_byte(code, end_line, end_char_u16).max(s_byte);
    Span {
        start_byte: s_byte,
        end_byte: e_byte,
        start_row: start_line,
        start_col: start_char_u16,
        end_row: end_line,
        end_col: end_char_u16,
    }
}

/// Decode semantic tokens per LSP delta encoding into absolute (line, char_u16, length, type_index).
/// Returns vector of raw tuples to be consumed into a histogram with a provided legend.
pub fn decode_semantic_tokens(data: &[u32]) -> Vec<(usize, usize, usize, usize)> {
    let mut out = Vec::new();
    let mut line: usize = 0;
    let mut col: usize = 0;
    let mut i = 0;
    while i + 4 < data.len() {
        let delta_line = data[i] as usize;
        let delta_start = data[i + 1] as usize;
        let length = data[i + 2] as usize;
        let token_type = data[i + 3] as usize;
        let _mods = data[i + 4];
        i += 5;

        if delta_line > 0 {
            line += delta_line;
            col = delta_start;
        } else {
            col += delta_start;
        }
        out.push((line, col, length, token_type));
    }
    out
}

/// Build semantic token histogram using a legend (type_index -> name).
pub fn semantic_histogram(
    decoded: &[(usize, usize, usize, usize)],
    legend: &[String],
) -> BTreeMap<String, u32> {
    let mut hist = BTreeMap::<String, u32>::new();
    for &(_, _, _len, ty) in decoded {
        let name = legend
            .get(ty as usize)
            .cloned()
            .unwrap_or_else(|| format!("type#{ty}"));
        *hist.entry(name).or_default() += 1;
    }
    hist
}
