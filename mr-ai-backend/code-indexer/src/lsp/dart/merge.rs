//! Merge LSP symbols/semantic histograms and AST info into `chunk.lsp`.
//!
//! Responsibilities:
//! - For each file, match the best-overlapping LSP symbol to each chunk.
//! - Fill `signature_lsp`, `outline_code_range` (lines), semantic histograms.
//! - Attach hover/definitions (queried once at the chunk head).
//! - Merge AST-driven `imports_used`, tags, and generic metrics.
//! - Produce FQN and keep legacy flags/tags sorted & deduped.
//!
//! Language-agnostic:
//! - Language-specific AST providers should populate `FileAstExtras` in
//!   `crate::ast::extras`, and this module will merge those extras.
//!
//! Performance notes:
//! - Exactly one hover + one definition query per chunk (at head position).
//! - Overlap matching is linear in symbols per file; typical counts are small.

use super::client::{LspProcess, RpcMessage};
use super::parse::LspSymbolInfo;
use super::util::{best_overlap_index, file_uri_abs, first_line, truncate};

use crate::errors::Result;

use crate::lsp::dart::extras::FileAstExtras;
use crate::types::{
    CodeChunk, DefLocation, LspEnrichment, OriginKind, SemanticTopToken, SymbolMetrics,
};

use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use tracing::{debug, info, trace, warn};
use url::Url;

/// Attach per-file enrichment into chunks.
///
/// Assumptions:
/// - `per_file_syms`, `per_file_hist`, `per_file_ast` are keyed by exact `chunks[i].file`.
/// - LSP symbol spans and chunk spans refer to the same file and byte space.
///
/// Side effects:
/// - Mutates `chunks[i].lsp` in-place; preserves existing fields and merges additively.
pub fn merge_file_enrichment_into_chunks(
    client: &mut LspProcess,
    chunks: &mut [CodeChunk],
    per_file_syms: &HashMap<String, Vec<LspSymbolInfo>>,
    per_file_hist: &HashMap<String, HashMap<String, u32>>,
    per_file_ast: &HashMap<String, FileAstExtras>,
) -> Result<()> {
    // Index: file -> sorted chunk indices by start_byte
    let mut by_file = HashMap::<String, Vec<usize>>::new();
    for (i, c) in chunks.iter().enumerate() {
        by_file.entry(c.file.clone()).or_default().push(i);
    }
    for (file, v) in by_file.iter_mut() {
        v.sort_by_key(|&i| chunks[i].span.start_byte);
        trace!(%file, chunk_count = v.len(), "indexed chunks by file");
    }

    // Telemetry
    let mut chunks_total = 0usize;
    let mut chunks_matched = 0usize;
    let mut chunks_hist_only = 0usize;
    let mut chunks_no_match = 0usize;

    let mut set_signature = 0usize;
    let mut set_outline = 0usize;
    let mut set_hover = 0usize;
    let mut set_def = 0usize;

    // Per-file pass
    for (file, idxs) in &by_file {
        let syms = per_file_syms.get(file).cloned().unwrap_or_default();
        let file_hist = per_file_hist.get(file);
        let file_ast = per_file_ast.get(file);

        debug!(
            %file,
            symbols = syms.len(),
            file_hist_kinds = file_hist.map(|m| m.len()).unwrap_or(0),
            ast_imports = file_ast.map(|a| a.imports.len()).unwrap_or(0),
            ast_uses = file_ast.map(|a| a.uses.len()).unwrap_or(0),
            ast_tags = file_ast.map(|a| a.tags.len()).unwrap_or(0),
            chunks_for_file = idxs.len(),
            "merging file enrichment into chunks"
        );

        for &i in idxs {
            chunks_total += 1;
            let c = &mut chunks[i];

            // Best matching symbol by overlap.
            let best = best_overlap_index(&c.span, &syms);
            trace!(
                chunk_id = %c.id,
                span_start = c.span.start_byte,
                span_end = c.span.end_byte,
                best_symbol_idx = ?best,
                "matching chunk to LSP symbol"
            );

            if let Some(best_idx) = best {
                chunks_matched += 1;
                let sym = &syms[best_idx];
                let mut lsp = c.lsp.take().unwrap_or_default();

                // Signature (one-liner from LSP).
                if lsp.signature_lsp.is_none() {
                    if let Some(sig) = &sym.signature {
                        lsp.signature_lsp = Some(sig.clone());
                        set_signature += 1;
                        trace!(chunk_id = %c.id, "set signature_lsp from LSP symbol");
                    }
                }

                // Outline range (lines) from LSP selection if available.
                if lsp.outline_code_range.is_none() {
                    if let Some((sl, el)) = sym.selection_range_lines {
                        lsp.outline_code_range = Some((sl, el));
                        set_outline += 1;
                        trace!(chunk_id = %c.id, start_line = sl, end_line = el, "set outline_code_range");
                    }
                }

                // Merge symbol-level semantic histogram.
                if let Some(h) = &sym.semantic_hist {
                    let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                    for (k, v) in h {
                        *m.entry(k.clone()).or_default() += *v;
                    }
                    lsp.semantic_token_hist = Some(m);
                }

                // Merge flags from symbol (sorted & deduped later).
                if !sym.flags.is_empty() {
                    lsp.flags.extend(sym.flags.clone());
                }

                // Query hover & definitions at chunk head.
                let before_hover = lsp.hover_type.is_some() || lsp.hover_doc_md.is_some();
                let before_defs = lsp.definition.is_some() || !lsp.definitions.is_empty();
                enrich_hover_and_defs(
                    client,
                    &mut lsp,
                    &c.file,
                    c.span.start_row,
                    c.span.start_col,
                )?;
                if (lsp.hover_type.is_some() || lsp.hover_doc_md.is_some()) && !before_hover {
                    set_hover += 1;
                }
                if (lsp.definition.is_some() || !lsp.definitions.is_empty()) && !before_defs {
                    set_def += 1;
                }

                // Merge file-level histogram (complement).
                if let Some(h) = file_hist {
                    let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                    for (k, v) in h {
                        *m.entry(k.clone()).or_default() += *v;
                    }
                    lsp.semantic_token_hist = Some(m);
                }

                // Compute normalized top-K.
                normalize_top_k(&mut lsp);

                // Merge AST extras (imports/uses/tags).
                if let Some(ast) = file_ast {
                    lsp.imports_used.extend(ast.imports.clone());
                    lsp.tags.extend(ast.tags.clone());
                    for u in &ast.uses {
                        lsp.tags.push(format!("uses:{u}"));
                    }
                    // Tag normalized import origins
                    for iu in &ast.imports {
                        match iu.origin {
                            OriginKind::Sdk => lsp.tags.push(format!("sdk:{}", iu.label)),
                            OriginKind::Package => lsp.tags.push(format!("pkg:{}", iu.label)),
                            OriginKind::Local => lsp.tags.push(format!("local:{}", iu.label)),
                            OriginKind::Unknown => {}
                        }
                        if iu.identifier != "*" && !iu.identifier.is_empty() {
                            lsp.tags
                                .push(format!("uses:{}:{}", iu.label, iu.identifier));
                        }
                    }
                }

                // FQN from file + owner_path + symbol.
                if lsp.fqn.is_none() {
                    let fqn = if c.owner_path.is_empty() {
                        format!("{}::{}", c.file, c.symbol)
                    } else {
                        format!("{}::{}::{}", c.file, c.owner_path.join("::"), c.symbol)
                    };
                    lsp.fqn = Some(fqn);
                }

                // Generic metrics (language-neutral).
                lsp.metrics = Some(derive_metrics(
                    lsp.signature_lsp.as_deref(),
                    lsp.hover_type.as_deref(),
                    c.span.end_row.saturating_sub(c.span.start_row) as u32,
                ));

                // Final tidy.
                lsp.flags.sort();
                lsp.flags.dedup();
                lsp.tags.sort();
                lsp.tags.dedup();
                c.lsp = Some(lsp);

                debug!(
                    chunk_id = %c.id,
                    has_sig = c.lsp.as_ref().and_then(|l| l.signature_lsp.as_ref()).is_some(),
                    has_outline = c.lsp.as_ref().and_then(|l| l.outline_code_range.as_ref()).is_some(),
                    has_hover = c.lsp.as_ref().map(|l| l.hover_type.is_some() || l.hover_doc_md.is_some()).unwrap_or(false),
                    has_def = c.lsp.as_ref().map(|l| l.definition.is_some() || !l.definitions.is_empty()).unwrap_or(false),
                    hist_kinds = c.lsp.as_ref().and_then(|l| l.semantic_token_hist.as_ref()).map(|m| m.len()).unwrap_or(0),
                    "chunk.lsp updated"
                );
            } else {
                // No symbol overlap — still attach file-level hist/AST if present.
                let mut attached = false;
                let mut lsp = c.lsp.take().unwrap_or_default();

                if let Some(h) = file_hist {
                    let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                    for (k, v) in h {
                        *m.entry(k.clone()).or_default() += *v;
                    }
                    lsp.semantic_token_hist = Some(m);
                    attached = true;
                }
                if let Some(ast) = file_ast {
                    let before = lsp.imports_used.len();
                    lsp.imports_used.extend(ast.imports.clone());
                    lsp.tags.extend(ast.tags.clone());
                    for u in &ast.uses {
                        lsp.tags.push(format!("uses:{u}"));
                    }
                    let after = lsp.imports_used.len();
                    if after > before {
                        attached = true;
                    }
                }
                if attached {
                    normalize_top_k(&mut lsp);
                    lsp.tags.sort();
                    lsp.tags.dedup();
                    c.lsp = Some(lsp);
                    warn!(chunk_id = %c.id, "no LSP symbol overlap; attached file-level hist/AST");
                    chunks_hist_only += 1;
                } else {
                    warn!(chunk_id = %c.id, "no LSP symbol overlap nor file-level enrichment; noop");
                    chunks_no_match += 1;
                }
            }
        }
    }

    info!(
        chunks_total,
        chunks_matched,
        chunks_hist_only,
        chunks_no_match,
        set_signature,
        set_outline,
        set_hover,
        set_def,
        "merge pass summary"
    );

    Ok(())
}

/// Normalize `semantic_top_k` from `semantic_token_hist` (keep top 8).
fn normalize_top_k(lsp: &mut LspEnrichment) {
    if let Some(hist) = &lsp.semantic_token_hist {
        let total: u32 = hist.values().copied().sum();
        if total > 0 {
            let mut top: Vec<(String, u32)> = hist.iter().map(|(k, v)| (k.clone(), *v)).collect();
            top.sort_by_key(|(_, v)| std::cmp::Reverse(*v));
            top.truncate(8);
            lsp.semantic_top_k = top
                .into_iter()
                .map(|(name, v)| SemanticTopToken {
                    name,
                    ratio: (v as f32) / (total as f32),
                })
                .collect();
        }
    }
}

/// Query hover/definitions at `(line, col)` and fill structured fields on `lsp`.
///
/// Hover handling:
/// - Supports `MarkupContent` and legacy `MarkedString` / `MarkedString[]`.
/// - `hover_type` — first line of markdown; `hover_doc_md` — truncated body (2 KB).
///
/// Definition handling:
/// - Accepts `Location | Location[] | LocationLink[]`.
/// - Stores first as `definition`, full list as `definitions`.
///
/// One position per chunk (head) keeps latency under control.
fn enrich_hover_and_defs(
    client: &mut LspProcess,
    lsp: &mut LspEnrichment,
    file_path: &str,
    line: usize,
    col: usize,
) -> Result<()> {
    let uri = file_uri_abs(Path::new(file_path));

    // --- Hover
    {
        let id = client.next_id();
        trace!("send textDocument/hover");
        client.send(&json!({
            "jsonrpc":"2.0","id":id,"method":"textDocument/hover",
            "params":{"textDocument":{"uri":uri},"position":{"line":line,"character":col}}
        }))?;
        loop {
            match client.recv()? {
                RpcMessage::Response {
                    id: rid,
                    result,
                    error,
                } if rid == id => {
                    if let Some(err) = error {
                        warn!(%err, "hover returned error");
                        break;
                    }
                    if let Some(v) = result {
                        // Try MarkupContent: { contents: { kind, value } }
                        let md = v
                            .pointer("/contents/value")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string())
                            // Or MarkedString single: { contents: { language, value } }
                            .or_else(|| {
                                v.pointer("/contents/language")
                                    .and_then(|_| v.pointer("/contents/value"))
                                    .and_then(|x| x.as_str())
                                    .map(|s| s.to_string())
                            })
                            // Or MarkedString[]: { contents: [ {value? or string}, ... ] }
                            .or_else(|| {
                                v.pointer("/contents")?
                                    .as_array()
                                    .map(|arr| {
                                        let mut buf = String::new();
                                        for item in arr {
                                            if let Some(s) = item.as_str() {
                                                if !buf.is_empty() {
                                                    buf.push_str("\n\n");
                                                }
                                                buf.push_str(s);
                                            } else if let Some(v) =
                                                item.pointer("/value").and_then(|x| x.as_str())
                                            {
                                                if !buf.is_empty() {
                                                    buf.push_str("\n\n");
                                                }
                                                buf.push_str(v);
                                            }
                                        }
                                        if buf.is_empty() { None } else { Some(buf) }
                                    })
                                    .flatten()
                            });

                        if let Some(full) = md {
                            if lsp.hover_doc_md.is_none() {
                                lsp.hover_doc_md = Some(truncate(full.clone(), 2048));
                                trace!("set hover_doc_md");
                            }
                            if lsp.hover_type.is_none() {
                                lsp.hover_type = Some(first_line(&full, 256));
                                trace!("set hover_type");
                            }
                        } else {
                            trace!("hover: empty contents");
                        }
                    } else {
                        trace!("hover: no result");
                    }
                    break;
                }
                RpcMessage::Notification { .. } => {}
                _ => {}
            }
        }
    }

    // --- Definition(s)
    {
        let id = client.next_id();
        trace!("send textDocument/definition");
        client.send(&json!({
            "jsonrpc":"2.0","id":id,"method":"textDocument/definition",
            "params":{"textDocument":{"uri":uri},"position":{"line":line,"character":col}}
        }))?;

        let mut defs: Vec<DefLocation> = Vec::new();
        loop {
            match client.recv()? {
                RpcMessage::Response {
                    id: rid,
                    result,
                    error,
                } if rid == id => {
                    if let Some(err) = error {
                        warn!(%err, "definition returned error");
                        break;
                    }
                    if let Some(v) = result {
                        // Accept single Location, Location[], or LocationLink[]
                        if v.is_array() {
                            for item in v.as_array().unwrap() {
                                append_def_location(item, &mut defs);
                            }
                        } else {
                            append_def_location(&v, &mut defs);
                        }
                        debug!(count = defs.len(), "definition results parsed");
                    } else {
                        trace!("definition: no result");
                    }
                    break;
                }
                RpcMessage::Notification { .. } => {}
                _ => {}
            }
        }

        if !defs.is_empty() {
            if lsp.definition.is_none() {
                lsp.definition = Some(defs[0].clone());
                trace!("set primary definition");
            }
            lsp.definitions.extend(defs);
        } else {
            trace!("no definitions to merge");
        }
    }

    Ok(())
}

/// Append a single `DefLocation` from a generic LSP location JSON node.
/// Supports both `Location` and `LocationLink`.
fn append_def_location(loc: &serde_json::Value, out: &mut Vec<DefLocation>) {
    // Location: { uri, range }
    if let Some(turi) = loc.pointer("/uri").and_then(|x| x.as_str()) {
        let range = loc.get("range").and_then(|rr| {
            Some((
                rr.pointer("/start/line")?.as_u64()? as usize,
                rr.pointer("/start/character")?.as_u64()? as usize,
                rr.pointer("/end/line")?.as_u64()? as usize,
                rr.pointer("/end/character")?.as_u64()? as usize,
            ))
        });
        out.push(DefLocation {
            origin: classify_origin(turi),
            target: normalize_target(turi),
            range,
        });
        return;
    }

    // LocationLink: { targetUri, targetRange }
    if let Some(turi) = loc.pointer("/targetUri").and_then(|x| x.as_str()) {
        let range = loc.get("targetRange").and_then(|rr| {
            Some((
                rr.pointer("/start/line")?.as_u64()? as usize,
                rr.pointer("/start/character")?.as_u64()? as usize,
                rr.pointer("/end/line")?.as_u64()? as usize,
                rr.pointer("/end/character")?.as_u64()? as usize,
            ))
        });
        out.push(DefLocation {
            origin: classify_origin(turi),
            target: normalize_target(turi),
            range,
        });
    }
}

/// Classify origin from a generic URI/specifier.
/// SDK/Package are language-agnostic concepts; file:// is Local; the rest Unknown.
fn classify_origin(uri: &str) -> OriginKind {
    // Common SDK-like schemes (providers may add others upstream).
    if uri.starts_with("dart:") || uri.starts_with("rust:") || uri.starts_with("go:") {
        return OriginKind::Sdk;
    }
    // Package-like scheme.
    if uri.starts_with("package:") {
        return OriginKind::Package;
    }
    if let Ok(u) = Url::parse(uri) {
        if u.scheme() == "file" {
            return OriginKind::Local;
        }
    }
    OriginKind::Unknown
}

/// Keep canonical target as provided by the server/AST.
/// For file URIs we keep the raw URI; higher layers can map to repo-relative.
fn normalize_target(uri: &str) -> String {
    uri.to_string()
}

/// Derive simple, language-agnostic metrics from signature/hover and rough LOC.
///
/// - `is_async`: looks for common async markers (async/await/Future/Promise/Stream).
/// - `params_count`: counts comma-separated params inside the first pair of parens.
/// - Language/framework-specific flags belong in `metrics.custom` (namespaced).
fn derive_metrics(sig: Option<&str>, hover: Option<&str>, loc: u32) -> SymbolMetrics {
    let s = sig.unwrap_or_default();
    let h = hover.unwrap_or_default();
    let text = if s.is_empty() { h } else { s };

    let is_async = ["async", "await", "Future", "Promise", "Stream"]
        .iter()
        .any(|kw| text.contains(kw));

    let params_count = s
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(inside, _)| inside.split(',').filter(|t| !t.trim().is_empty()).count())
        .and_then(|n| u8::try_from(n).ok());

    SymbolMetrics {
        is_async,
        loc: Some(loc),
        params_count,
        custom: BTreeMap::new(),
    }
}
