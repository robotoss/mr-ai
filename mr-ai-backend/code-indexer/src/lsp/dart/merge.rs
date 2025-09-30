//! Merge LSP symbols/semantic histograms and AST info into `chunk.lsp`.
//!
//! Responsibilities:
//! - For each file, match the best-overlapping LSP symbol to each chunk.
//! - Fill `signature_lsp`, `outline_code_range` (lines), semantic histograms.
//! - Attach hover/definition (one position at chunk head).
//! - Add AST-driven `imports_used`, tags, and simple metrics.
//! - Produce FQN and keep legacy flags sorted/deduped.

use super::client::{LspProcess, RpcMessage};
use super::parse::LspSymbolInfo;
use super::util::{best_overlap_index, file_uri_abs, first_line, truncate};

use crate::errors::Result;
use crate::lsp::dart::ast::AstFile;
use crate::types::{
    CodeChunk, DefLocation, LspEnrichment, OriginKind, SemanticTopToken, SymbolMetrics,
};

use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, trace, warn};
use url::Url;

/// Attach per-file enrichment into chunks:
///
/// The function assumes `per_file_syms` / `per_file_hist` / `per_file_ast` are keyed
/// by exact `chunks[i].file` values.
pub fn merge_file_enrichment_into_chunks(
    client: &mut LspProcess,
    chunks: &mut [CodeChunk],
    per_file_syms: &HashMap<String, Vec<LspSymbolInfo>>,
    per_file_hist: &HashMap<String, HashMap<String, u32>>,
    per_file_ast: &HashMap<String, AstFile>,
) -> Result<()> {
    // Build file → chunk indices for quick lookup.
    let mut by_file = HashMap::<String, Vec<usize>>::new();
    for (i, c) in chunks.iter().enumerate() {
        by_file.entry(c.file.clone()).or_default().push(i);
    }
    for (file, v) in by_file.iter_mut() {
        v.sort_by_key(|&i| chunks[i].span.start_byte);
        trace!(%file, chunk_count = v.len(), "indexed chunks by file");
    }

    // Merge stats
    let mut chunks_total = 0usize;
    let mut chunks_matched = 0usize;
    let mut chunks_hist_only = 0usize;
    let mut chunks_no_match = 0usize;

    let mut set_signature = 0usize;
    let mut set_outline = 0usize;
    let mut set_hover = 0usize;
    let mut set_def = 0usize;

    // For each file, match symbols to its chunk(s) and merge.
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
                "matching chunk to symbol"
            );

            if let Some(best) = best {
                chunks_matched += 1;
                let sym = &syms[best];

                // Prepare/borrow LSP block.
                let mut lsp = c.lsp.take().unwrap_or_default();

                // signature_lsp
                if lsp.signature_lsp.is_none() {
                    if let Some(sig) = &sym.signature {
                        lsp.signature_lsp = Some(sig.clone());
                        set_signature += 1;
                        trace!(chunk_id = %c.id, "set signature_lsp from symbol");
                    }
                }

                // outline_code_range — use lines from LSP selection range
                if lsp.outline_code_range.is_none() {
                    if let Some((sl, el)) = sym.selection_range_lines {
                        lsp.outline_code_range = Some((sl, el));
                        set_outline += 1;
                        trace!(chunk_id = %c.id, start_line = sl, end_line = el, "set outline_code_range(lines)");
                    }
                }

                // Merge symbol-level semantic histogram (if present).
                if let Some(h) = &sym.semantic_hist {
                    let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                    for (k, v) in h {
                        *m.entry(k.clone()).or_default() += *v;
                    }
                    lsp.semantic_token_hist = Some(m);
                }

                // Merge flags (kind/short hover lines/etc.) if any.
                if !sym.flags.is_empty() {
                    let before = lsp.flags.len();
                    lsp.flags.extend(sym.flags.clone());
                    lsp.flags.sort();
                    lsp.flags.dedup();
                    let _after = lsp.flags.len();
                    trace!(chunk_id = %c.id, flags_added = _after.saturating_sub(before), flags_total = _after, "merged symbol flags");
                }

                // Targeted hover/definition on the chunk head.
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

                // Merge file-level semantic histogram (if present).
                if let Some(h) = file_hist {
                    let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                    for (k, v) in h {
                        *m.entry(k.clone()).or_default() += *v;
                    }
                    lsp.semantic_token_hist = Some(m);
                }

                // Normalize top-K from histogram for convenience.
                if let Some(hist) = &lsp.semantic_token_hist {
                    let total: u32 = hist.values().copied().sum();
                    if total > 0 {
                        let mut top: Vec<(String, u32)> =
                            hist.iter().map(|(k, v)| (k.clone(), *v)).collect();
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

                // Attach AST imports/uses to enrichment.
                if let Some(ast) = file_ast {
                    lsp.imports_used.extend(ast.imports.clone());
                    // Tags: origin, package names, used identifiers
                    for iu in &ast.imports {
                        match iu.origin {
                            OriginKind::Sdk => lsp.tags.push(format!("sdk:{}", iu.label)),
                            OriginKind::Package => lsp.tags.push(format!("pkg:{}", iu.label)),
                            OriginKind::Local => lsp.tags.push(format!("local:{}", iu.label)),
                            OriginKind::Unknown => {}
                        }
                        if iu.identifier != "*" {
                            lsp.tags
                                .push(format!("uses:{}:{}", iu.label, iu.identifier));
                        }
                    }
                    for u in &ast.uses {
                        lsp.tags.push(format!("uses:{}", u));
                    }
                }

                // FQN from file + owner_path + symbol head.
                if lsp.fqn.is_none() {
                    let mut fqn = format!("{}::{}", c.file, c.symbol);
                    if !c.owner_path.is_empty() {
                        fqn = format!("{}::{}", c.file, c.owner_path.join("::"));
                        fqn.push_str(&format!("::{}", c.symbol));
                    }
                    lsp.fqn = Some(fqn);
                }

                // Simple metrics.
                lsp.metrics = Some(derive_metrics(
                    lsp.signature_lsp.as_deref(),
                    lsp.hover_type.as_deref(),
                    c.span.end_row.saturating_sub(c.span.start_row + 1) as u32,
                ));

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
                // No matching symbol — still attach file-level histogram and AST imports if any.
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
                    let after = lsp.imports_used.len();
                    if after > before {
                        attached = true;
                    }
                }
                if attached {
                    c.lsp = Some(lsp);
                    warn!(chunk_id = %c.id, "no symbol overlap; attached file-level hist/AST");
                    chunks_hist_only += 1;
                } else {
                    warn!(chunk_id = %c.id, "no symbol overlap nor file-level enrichment; noop");
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

/// Query hover/definition at `(line, col)` and fill structured fields on `lsp`.
///
/// For hover:
/// - `hover_type` is the first line of the markdown.
/// - `hover_doc_md` stores a truncated (2KB) markdown body.
///
/// For definitions:
/// - `definition` stores the first location with origin classification.
/// - `definitions` stores the full list.
///
/// The function is intentionally conservative (one position per chunk head) to keep
/// latency under control; detailed per-symbol calls are handled earlier in the pipeline.
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
                        let md = v
                            .pointer("/contents/value")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string());
                        if let Some(ref full) = md {
                            if lsp.hover_doc_md.is_none() {
                                lsp.hover_doc_md = Some(truncate(full.clone(), 2048));
                                trace!("set hover_doc_md");
                            }
                            if lsp.hover_type.is_none() {
                                lsp.hover_type = Some(first_line(full, 256));
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
                        let arr = v.as_array().cloned().unwrap_or_else(|| vec![v.clone()]);
                        for loc in arr {
                            if let Some(turi) = loc.pointer("/uri").and_then(|x| x.as_str()) {
                                let range = loc.get("range").and_then(|rr| {
                                    Some((
                                        rr.pointer("/start/line")?.as_u64()? as usize,
                                        rr.pointer("/start/character")?.as_u64()? as usize,
                                        rr.pointer("/end/line")?.as_u64()? as usize,
                                        rr.pointer("/end/character")?.as_u64()? as usize,
                                    ))
                                });
                                defs.push(DefLocation {
                                    origin: classify_origin(turi),
                                    target: normalize_target(turi),
                                    range,
                                });
                            }
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
    // Keep SDK/package URIs as-is; for file URIs keep the raw URI (downstream can map to repo-relative).
    uri.to_string()
}

fn derive_metrics(sig: Option<&str>, hover: Option<&str>, loc: u32) -> SymbolMetrics {
    let s = sig.unwrap_or_default();
    let h = hover.unwrap_or_default();
    let text = format!("{s}\n{h}");

    let is_async = text.contains("Future")
        || text.contains("Stream")
        || text.contains("async")
        || text.contains("await")
        || text.contains("Timer");
    let is_widget = text.contains(" Widget")
        || text.contains("StatelessWidget")
        || text.contains("StatefulWidget")
        || text.contains(" extends Widget");

    let params_count = s
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(inside, _)| inside.split(',').filter(|t| !t.trim().is_empty()).count())
        .and_then(|n| u8::try_from(n).ok());

    SymbolMetrics {
        is_async,
        is_widget,
        loc: Some(loc),
        params_count,
    }
}
