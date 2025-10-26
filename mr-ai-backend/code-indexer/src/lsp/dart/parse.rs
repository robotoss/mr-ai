//! LSP parsers: DocumentSymbol → LspSymbolInfo.

use serde_json::Value;
use tracing::trace;

#[derive(Debug, Clone)]
pub struct ByteRange {
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone)]
pub struct LspSymbolInfo {
    pub name: String,
    pub signature: Option<String>,
    pub range: ByteRange, // absolute byte range in the file
}

// Convert (line, UTF-16 column) coming from LSP into a UTF-8 byte offset.
fn line_col_utf16_to_byte_offset(text: &str, line: usize, col_utf16: usize) -> usize {
    let mut byte_offs = 0usize;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i == line {
            let line_str = if l.ends_with('\n') {
                &l[..l.len() - 1]
            } else {
                l
            };
            let mut u16_count = 0usize;
            for (byte_idx, ch) in line_str.char_indices() {
                if u16_count >= col_utf16 {
                    return byte_offs + byte_idx;
                }
                u16_count += ch.len_utf16();
            }
            return byte_offs + line_str.len();
        } else {
            byte_offs += l.as_bytes().len();
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
        let full = node.get("range");

        let sig = detail.map(|d| crate::lsp::dart::util::first_line(d, 240));

        // byte range from range (line/character → byte offset)
        let (start_b, end_b) = if let Some(rr) = full {
            let sl = rr
                .pointer("/start/line")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let sc = rr
                .pointer("/start/character")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let el = rr
                .pointer("/end/line")
                .and_then(|v| v.as_u64())
                .unwrap_or(sl as u64) as usize;
            let ec = rr
                .pointer("/end/character")
                .and_then(|v| v.as_u64())
                .unwrap_or(sc as u64) as usize;
            let sb = line_col_utf16_to_byte_offset(text, sl, sc);
            let eb = line_col_utf16_to_byte_offset(text, el, ec);
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
