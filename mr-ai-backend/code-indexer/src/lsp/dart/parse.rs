//! LSP parsing helpers for documentSymbol / semanticTokens.

use crate::types::Span;
use serde_json::Value;
use std::cmp::min;
use std::collections::{BTreeMap, HashMap};

use crate::lsp::dart::util::first_line;

/// Minimal symbol info used to match/meld with chunks.
#[derive(Debug, Clone)]
pub struct LspSymbolInfo {
    pub file: String,
    pub range: Span,                                   // full span (bytes and rows)
    pub signature: Option<String>,                     // one-line signature
    pub flags: Vec<String>,                            // kind:<...> etc.
    pub selection_range_lines: Option<(usize, usize)>, // outline in lines
    pub semantic_hist: Option<BTreeMap<String, u32>>,  // optional symbol histogram
}

/// Convert LSP (UTF-16) position into byte offset.
pub fn lsp_pos_to_byte(code: &str, line: usize, col_u16: usize) -> usize {
    // naive but robust: walk line, then walk col in chars, mapping to bytes.
    let mut off = 0usize;
    for (i, l) in code.split_inclusive('\n').enumerate() {
        if i == line {
            let mut c = 0usize;
            for (byte_idx, _) in l.char_indices() {
                if c == col_u16 {
                    return off + byte_idx;
                }
                c += 1;
            }
            return off + l.len();
        }
        off += l.len();
    }
    code.len()
}

/// Convert LSP range to our Span (byte-based, with rows/cols).
pub fn lsp_range_to_span(code: &str, sl: usize, sc: usize, el: usize, ec: usize) -> Span {
    let mut sb = lsp_pos_to_byte(code, sl, sc);
    let mut eb = lsp_pos_to_byte(code, el, ec);
    if eb < sb {
        std::mem::swap(&mut sb, &mut eb);
    }
    Span {
        start_byte: sb,
        end_byte: eb,
        start_row: sl,
        start_col: sc,
        end_row: el,
        end_col: ec,
    }
}

fn usize_at(v: &Value, ptr: &str) -> usize {
    v.pointer(ptr).and_then(|x| x.as_u64()).unwrap_or(0) as usize
}

fn lsp_symbol_kind_to_str(k: u32) -> &'static str {
    match k {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        19 => "Object",
        20 => "Key",
        21 => "Null",
        22 => "EnumMember",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "TypeParameter",
        _ => "Unknown",
    }
}

/// Parse documentSymbol → flat list of LspSymbolInfo.
pub fn collect_from_document_symbol(res: &Value, code: &str, file_key: &str) -> Vec<LspSymbolInfo> {
    let mut out = Vec::<LspSymbolInfo>::new();
    if let Some(arr) = res.as_array() {
        for v in arr {
            collect_recursive(v, code, file_key, &mut out);
        }
    }
    out
}

fn collect_recursive(v: &Value, code: &str, file_key: &str, out: &mut Vec<LspSymbolInfo>) {
    let r = v.get("range");
    let sel = v.get("selectionRange");
    if let (Some(r), Some(sr)) = (r, sel) {
        let (sl, sc, el, ec) = (
            usize_at(r, "/start/line"),
            usize_at(r, "/start/character"),
            usize_at(r, "/end/line"),
            usize_at(r, "/end/character"),
        );
        let span = lsp_range_to_span(code, sl, sc, el, ec);

        // Signature from selectionRange first line (or name).
        let mut s_sl = usize_at(sr, "/start/line");
        let mut s_sc = usize_at(sr, "/start/character");
        let mut s_el = usize_at(sr, "/end/line");
        let mut s_ec = usize_at(sr, "/end/character");
        if s_el < s_sl || (s_el == s_sl && s_ec < s_sc) {
            std::mem::swap(&mut s_sl, &mut s_el);
            std::mem::swap(&mut s_sc, &mut s_ec);
        }
        let sb = min(lsp_pos_to_byte(code, s_sl, s_sc), code.len());
        let eb = min(lsp_pos_to_byte(code, s_el, s_ec), code.len());
        let mut sig = first_line(&code[sb..eb], 240);
        if sig.is_empty() {
            if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
                sig = name.to_string();
            }
        }

        let mut flags = Vec::new();
        if let Some(k) = v.get("kind").and_then(|x| x.as_u64()) {
            flags.push(format!("kind:{}", lsp_symbol_kind_to_str(k as u32)));
        }

        out.push(LspSymbolInfo {
            file: file_key.to_string(),
            range: span,
            signature: if sig.is_empty() { None } else { Some(sig) },
            flags,
            selection_range_lines: Some((sl, el)),
            semantic_hist: None,
        });
    }
    if let Some(children) = v.get("children").and_then(|c| c.as_array()) {
        for ch in children {
            collect_recursive(ch, code, file_key, out);
        }
    }
}

/// Decode `semanticTokens/full` and return a per-file histogram.
pub fn decode_semantic_tokens_hist(res: &Value, legend: &[String]) -> Option<HashMap<String, u32>> {
    let data = res.get("data")?.as_array()?;
    let mut raw = Vec::<u32>::with_capacity(data.len());
    for v in data {
        raw.push(v.as_u64().unwrap_or(0) as u32);
    }
    let decoded = decode_semantic_tokens(&raw);
    let mut hist = HashMap::<String, u32>::new();
    for (_l, _c, _len, ty) in decoded {
        let name = legend
            .get(ty)
            .cloned()
            .unwrap_or_else(|| format!("type#{ty}"));
        *hist.entry(name).or_default() += 1;
    }
    Some(hist)
}

/// Decode LSP's compact relative semantic tokens layout.
/// Each token: (line_delta, start_char_delta, length, token_type_index, token_modifiers)
pub fn decode_semantic_tokens(data: &[u32]) -> Vec<(usize, usize, usize, usize)> {
    let mut out = Vec::new();
    let mut line = 0usize;
    let mut col = 0usize;
    let mut i = 0usize;
    while i + 4 < data.len() {
        let dl = data[i] as usize;
        let dc = data[i + 1] as usize;
        let len = data[i + 2] as usize;
        let ty = data[i + 3] as usize;
        // let _mods = data[i+4] as usize;
        if dl > 0 {
            line += dl;
            col = dc;
        } else {
            col += dc;
        }
        out.push((line, col, len, ty));
        i += 5;
    }
    out
}
