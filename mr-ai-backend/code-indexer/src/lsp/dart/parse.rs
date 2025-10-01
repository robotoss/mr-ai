//! LSP parsers: DocumentSymbol → LspSymbolInfo, SemanticTokens → histogram.

use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, trace};

#[derive(Debug, Clone)]
pub struct ByteRange {
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct LspSymbolInfo {
    pub name: String,
    pub signature: Option<String>,
    pub range: ByteRange,                              // byte range in the file
    pub selection_range_lines: Option<(usize, usize)>, // outline as (start_line, end_line)
    pub semantic_hist: Option<HashMap<String, u32>>,   // unused here
    pub flags: Vec<String>,
}

fn line_col_to_byte_offset(text: &str, line: usize, col: usize) -> usize {
    // Convert (line, character) to byte offset (defensive over UTF-8)
    let mut offs = 0usize;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == line {
            let mut cbytes = 0usize;
            for (ci, ch) in l.chars().enumerate() {
                if ci == col {
                    break;
                }
                cbytes += ch.len_utf8();
            }
            offs += cbytes;
            return offs;
        } else {
            offs += l.as_bytes().len();
        }
    }
    text.len()
}

/// Flatten DocumentSymbol result into a simple list.
pub fn collect_from_document_symbol(res: &Value, text: &str, file_key: &str) -> Vec<LspSymbolInfo> {
    let mut out = Vec::<LspSymbolInfo>::new();
    fn walk(node: &Value, text: &str, out: &mut Vec<LspSymbolInfo>) {
        if let Some(arr) = node.as_array() {
            for n in arr {
                walk(n, text, out);
            }
            return;
        }
        if !node.is_object() {
            return;
        }

        let name = node
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let detail = node.get("detail").and_then(|v| v.as_str());
        let selection = node.get("selectionRange");
        let full = node.get("range");

        let sig = detail.map(|d| crate::lsp::dart::util::first_line(d, 240));

        // outline lines from selectionRange
        let sel_lines = selection.and_then(|r| {
            let sl = r.pointer("/start/line")?.as_u64()? as usize;
            let el = r.pointer("/end/line")?.as_u64()? as usize;
            Some((sl, el))
        });

        // byte range from range (line/character → byte offset)
        let (start_b, end_b) = if let Some(rr) = full {
            let sl = rr
                .pointer("/start/line")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let sc = rr
                .pointer("/start/character")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let el = rr
                .pointer("/end/line")
                .and_then(|v| v.as_u64())
                .unwrap_or(sl) as usize;
            let ec = rr
                .pointer("/end/character")
                .and_then(|v| v.as_u64())
                .unwrap_or(sc);
            let sb = line_col_to_byte_offset(text, sl as usize, sc as usize);
            let eb = line_col_to_byte_offset(text, el, ec as usize);
            (sb, eb)
        } else {
            (0, 0)
        };

        out.push(LspSymbolInfo {
            name,
            signature: sig,
            range: ByteRange {
                start_byte: start_b,
                end_byte: end_b,
            },
            selection_range_lines: sel_lines,
            semantic_hist: None,
            flags: Vec::new(),
        });

        if let Some(children) = node.get("children") {
            walk(children, text, out);
        }
    }

    if res.is_array() || res.is_object() {
        walk(res, text, &mut out);
    }
    trace!(file=%file_key, collected = out.len(), "documentSymbol flatten done");
    out
}

/// Decode semanticTokens/full into { tokenKindName: count } histogram.
///
/// LSP returns "data": [deltaLine, deltaStart, length, tokenType, tokenModifiersBitset].
/// We count 1 per token by `tokenType` using names from the legend.
pub fn decode_semantic_tokens_hist(res: &Value, legend: &[String]) -> Option<HashMap<String, u32>> {
    let data = res.get("data").and_then(|d| d.as_array())?;
    let mut hist: HashMap<String, u32> = HashMap::new();

    let mut i = 0usize;
    while i + 4 < data.len() {
        let token_type_idx = data[i + 3].as_u64().unwrap_or(0) as usize;
        let kind = legend
            .get(token_type_idx)
            .cloned()
            .unwrap_or_else(|| format!("kind#{token_type_idx}"));
        *hist.entry(kind).or_default() += 1;
        i += 5;
    }

    debug!(kinds = hist.len(), "semanticTokens hist built");
    Some(hist)
}
