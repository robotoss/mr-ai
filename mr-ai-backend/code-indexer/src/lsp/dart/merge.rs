//! Merge LSP enrichment into `chunk.lsp` (no custom AST).
//!
//! This pass:
//! - attaches signature/outline/semantic histogram from per-file data,
//! - fetches hover/definition/references at the chunk head,
//! - injects diagnostics intersecting a line window around the chunk head,
//! - computes top-k semantic tokens for ranking.

use crate::errors::Result;
use crate::lsp::dart::client::{LspProcess, RpcMessage};
use crate::lsp::dart::parse::LspSymbolInfo;
use crate::lsp::dart::util::{best_overlap_index, file_uri_abs, first_line, truncate};
use crate::types::{CodeChunk, DefLocation, LspEnrichment, OriginKind, SemanticTopToken};
use serde_json::json;
use std::collections::HashMap;
use std::fs::metadata;
use std::path::Path;
use tracing::{debug, info, trace, warn};
use url::Url;

/// We apply diagnostics to chunks using a sliding line window anchored at the chunk head.
/// This keeps the logic simple even if we don't know the chunk's ending line.
const DIAG_LINE_WINDOW: usize = 400;
/// Cap the number of references attached to a chunk to avoid JSON bloat.
const MAX_REFS_PER_CHUNK: usize = 32;
/// Cap diagnostics attached to a chunk.
const MAX_DIAGS_PER_CHUNK: usize = 50;

fn is_dart_path(p: &str) -> bool {
    p.ends_with(".dart")
}

pub fn merge_file_enrichment_into_chunks(
    client: &mut LspProcess,
    chunks: &mut [CodeChunk],
    per_file_syms: &HashMap<String, Vec<LspSymbolInfo>>,
    per_file_hist: &HashMap<String, HashMap<String, u32>>,
    per_file_diags: &HashMap<String, Vec<crate::types::LspDiagnostic>>,
    semantic_legend: &[String],
) -> Result<()> {
    // Quick diagnostics for missing per-file entries
    for c in chunks.iter() {
        if is_dart_path(&c.file) && !per_file_syms.contains_key(&c.file) {
            let nearest = nearest_keys(&c.file, per_file_syms.keys());
            warn!(file=%c.file, nearest_keys=%nearest, "no per_file_syms entry for Dart file — possible path/key mismatch");
        } else if !is_dart_path(&c.file) && !per_file_syms.contains_key(&c.file) {
            // Non-Dart files in chunks are expected; keep it quiet.
            debug!(file=%c.file, "non-Dart file has no LSP symbols (expected)");
        }
    }

    // Index chunks by file
    let mut by_file = HashMap::<String, Vec<usize>>::new();
    for (i, c) in chunks.iter().enumerate() {
        by_file.entry(c.file.clone()).or_default().push(i);
    }
    for (file, v) in by_file.iter_mut() {
        v.sort_by_key(|&i| chunks[i].span.start_byte);
        trace!(%file, chunk_count = v.len(), "indexed chunks by file");
    }

    // Telemetry counters
    let (mut chunks_total, mut chunks_matched, mut chunks_hist_only, mut chunks_no_match) =
        (0usize, 0usize, 0usize, 0usize);
    let (mut set_signature, mut set_outline, mut set_hover, mut set_def, mut set_refs) =
        (0usize, 0usize, 0usize, 0usize, 0usize);

    for (file, idxs) in &by_file {
        let syms = per_file_syms.get(file).cloned().unwrap_or_default();
        let file_hist = per_file_hist.get(file);
        let file_diags = per_file_diags.get(file).cloned().unwrap_or_default();

        debug!(
            %file,
            symbols = syms.len(),
            file_hist_kinds = file_hist.map(|m| m.len()).unwrap_or(0),
            diags_total = file_diags.len(),
            chunks_for_file = idxs.len(),
            "merging file enrichment into chunks"
        );

        for &i in idxs {
            chunks_total += 1;
            let c = &mut chunks[i];

            if let Some(len) = file_len_bytes(&c.file) {
                if c.span.end_byte > len {
                    warn!(
                        chunk_id=%c.id,
                        file=%c.file,
                        chunk_range=%format!("{}..{}", c.span.start_byte, c.span.end_byte),
                        file_len=len,
                        "chunk span out-of-bounds for current file; likely text mismatch"
                    );
                }
            }

            // Match best-overlapping LSP symbol
            let best = best_overlap_index(&c.span, &syms);
            trace!(
                chunk_id = %c.id,
                span_start = c.span.start_byte,
                span_end = c.span.end_byte,
                best_symbol_idx = ?best,
                "matching chunk to LSP symbol"
            );

            // Prepare/merge LSP object
            let mut lsp = c.lsp.take().unwrap_or_default();
            // Always set col_unit to "utf16" when we use raw LSP coordinates.
            if lsp.col_unit.is_none() {
                lsp.col_unit = Some("utf16".to_string());
            }
            // Propagate semantic legend once (for consumers that need token names).
            if lsp.semantic_legend.is_none() && !semantic_legend.is_empty() {
                lsp.semantic_legend = Some(semantic_legend.to_vec());
            }

            if let Some(best_idx) = best {
                chunks_matched += 1;
                let sym = &syms[best_idx];

                // Signature (once)
                if lsp.signature_lsp.is_none() {
                    if let Some(sig) = &sym.signature {
                        lsp.signature_lsp = Some(sig.clone());
                        set_signature += 1;
                        trace!(chunk_id = %c.id, "set signature_lsp from LSP symbol");
                    }
                }

                // Outline lines from selectionRange
                if lsp.outline_code_range.is_none() {
                    if let Some((sl, el)) = sym.selection_range_lines {
                        lsp.outline_code_range = Some((sl, el));
                        set_outline += 1;
                        trace!(chunk_id = %c.id, start_line = sl, end_line = el, "set outline_code_range");
                    }
                }

                // Merge semantic histogram: symbol-level (if any) + file-level
                if let Some(h) = &sym.semantic_hist {
                    let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                    for (k, v) in h {
                        *m.entry(k.clone()).or_default() += *v;
                    }
                    lsp.semantic_token_hist = Some(m);
                }
                if let Some(h) = file_hist {
                    let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                    for (k, v) in h {
                        *m.entry(k.clone()).or_default() += *v;
                    }
                    lsp.semantic_token_hist = Some(m);
                }

                // Hover, Definitions & References at chunk head
                let before_hover = lsp.hover_type.is_some() || lsp.hover_doc_md.is_some();
                let before_defs = lsp.definition.is_some() || !lsp.definitions.is_empty();
                let before_refs = !lsp.references.is_empty() || lsp.references_count.is_some();

                enrich_hover_defs_refs(
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
                if (!lsp.references.is_empty() || lsp.references_count.is_some()) && !before_refs {
                    set_refs += 1;
                }
            } else {
                // No symbol overlap: attach file-level histogram if available
                let mut attached = false;
                if let Some(h) = file_hist {
                    let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                    for (k, v) in h {
                        *m.entry(k.clone()).or_default() += *v;
                    }
                    lsp.semantic_token_hist = Some(m);
                    attached = true;
                }
                if attached {
                    warn!(chunk_id = %c.id, "no LSP symbol overlap; attached file-level hist");
                    chunks_hist_only += 1;
                } else {
                    warn!(chunk_id = %c.id, "no LSP symbol overlap nor file-level enrichment; noop");
                    chunks_no_match += 1;
                }
            }

            // Attach diagnostics (filtered by line window around the chunk head)
            attach_diagnostics_for_chunk(&mut lsp, &file_diags, c.span.start_row);

            // Compute top-k tokens
            normalize_top_k(&mut lsp);

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
                refs = c.lsp.as_ref().map(|l| l.references.len()).unwrap_or(0),
                diags = c.lsp.as_ref().map(|l| l.diagnostics.len()).unwrap_or(0),
                hist_kinds = c.lsp.as_ref().and_then(|l| l.semantic_token_hist.as_ref()).map(|m| m.len()).unwrap_or(0),
                "chunk.lsp updated"
            );
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
        set_refs,
        "merge pass summary"
    );

    Ok(())
}

fn nearest_keys<'a, I: Iterator<Item = &'a String>>(k: &str, keys: I) -> String {
    // Super-light LCP-based "nearest keys" preview (for logging only)
    let mut v: Vec<(usize, String)> = keys.map(|s| (common_prefix_len(k, s), s.clone())).collect();
    v.sort_by_key(|(l, _)| std::cmp::Reverse(*l));
    v.into_iter()
        .take(3)
        .map(|(l, s)| format!("{s} (d={})", k.len().saturating_sub(l)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn file_len_bytes(path: &str) -> Option<usize> {
    metadata(path).ok().map(|m| m.len() as usize)
}

/// Build top-k tokens (top 8) from histogram.
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

/// Request hover/definition/references at `(line, col)` and merge into the chunk LSP data.
fn enrich_hover_defs_refs(
    client: &mut LspProcess,
    lsp: &mut LspEnrichment,
    file_path: &str,
    line: usize,
    col: usize,
) -> Result<()> {
    let abs = crate::lsp::dart::util::abs_path(Path::new(file_path));
    let uri = file_uri_abs(&abs);

    // Hover
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
                        let md = v
                            .pointer("/contents/value")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string())
                            .or_else(|| {
                                v.pointer("/contents/language")
                                    .and_then(|_| v.pointer("/contents/value"))
                                    .and_then(|x| x.as_str())
                                    .map(|s| s.to_string())
                            })
                            .or_else(|| {
                                v.pointer("/contents")?.as_array().and_then(|arr| {
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

    // Definitions (Location | Location[] | LocationLink[])
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

    // References (Location[])
    {
        let id = client.next_id();
        trace!("send textDocument/references");
        client.send(&json!({
            "jsonrpc":"2.0","id":id,"method":"textDocument/references",
            "params":{
                "textDocument":{"uri":uri},
                "position":{"line":line,"character":col},
                "context":{"includeDeclaration": false}
            }
        }))?;

        let mut refs: Vec<DefLocation> = Vec::new();
        loop {
            match client.recv()? {
                RpcMessage::Response {
                    id: rid,
                    result,
                    error,
                } if rid == id => {
                    if let Some(err) = error {
                        warn!(%err, "references returned error");
                        break;
                    }
                    if let Some(v) = result {
                        if let Some(arr) = v.as_array() {
                            for item in arr {
                                append_def_location(item, &mut refs);
                            }
                        }
                        debug!(count = refs.len(), "references results parsed");
                    } else {
                        trace!("references: no result");
                    }
                    break;
                }
                RpcMessage::Notification { .. } => {}
                _ => {}
            }
        }

        if !refs.is_empty() {
            let total = refs.len() as u32;
            // Cap the list to avoid bloat
            refs.truncate(MAX_REFS_PER_CHUNK);
            lsp.references_count = Some(total);
            lsp.references.extend(refs);
        }
    }

    Ok(())
}

fn append_def_location(loc: &serde_json::Value, out: &mut Vec<DefLocation>) {
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
            byte_range: None,
        });
        return;
    }
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
            byte_range: None,
        });
    }
}

fn classify_origin(uri: &str) -> OriginKind {
    if uri.starts_with("dart:") {
        return OriginKind::Sdk;
    }
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

fn normalize_target(uri: &str) -> String {
    uri.to_string()
}

/// Attach diagnostics relevant to the chunk using a simple line window filter.
fn attach_diagnostics_for_chunk(
    lsp: &mut LspEnrichment,
    file_diags: &[crate::types::LspDiagnostic],
    chunk_start_line: usize,
) {
    let total = file_diags.len() as u32;
    let has_err = file_diags.iter().any(|d| d.severity == Some(1));
    lsp.diagnostics_count = Some(total);
    lsp.has_errors = Some(has_err);

    let start = chunk_start_line;
    let end = start.saturating_add(DIAG_LINE_WINDOW);
    let mut filtered: Vec<_> = file_diags
        .iter()
        .filter(|d| {
            if let Some((sl, _, _, _)) = d.range {
                sl >= start && sl <= end
            } else {
                false
            }
        })
        .cloned()
        .collect();

    // Prefer higher severity first (1..4), then earlier lines
    filtered.sort_by_key(|d| {
        (
            d.severity.unwrap_or(4),
            d.range.map(|r| r.0).unwrap_or(usize::MAX),
        )
    });
    filtered.truncate(MAX_DIAGS_PER_CHUNK);
    lsp.diagnostics.extend(filtered);
}
