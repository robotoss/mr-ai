//! Merge minimal LSP enrichment into `chunk.lsp` (hover/defs/refs + diag counts).
//!
//! Retrieval-first fields only. Paths are normalized to repo-relative keys.

use crate::errors::Result;
use crate::lsp::dart::client::{LspProcess, RpcMessage};
use crate::lsp::dart::parse::LspSymbolInfo;
use crate::lsp::dart::util::{
    abs_canonical, best_overlap_index, file_uri_abs, first_line, normalize_to_repo_key,
    repo_rel_key, truncate,
};
use crate::types::{CodeChunk, DefLocation, LspEnrichment, OriginKind};
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tracing::{debug, info, trace, warn};
use url::Url;

const MAX_REFS_SAMPLE: usize = 32;

/// Merge enrichment per file into chunks.
pub fn merge_file_enrichment_into_chunks(
    client: &mut LspProcess,
    repo_root_abs: &Path,
    chunks: &mut [CodeChunk],
    per_file_syms: &HashMap<String, Vec<LspSymbolInfo>>,
    per_file_diag_counts: &HashMap<String, (u32, u32)>, // (errors, warnings)
) -> Result<()> {
    // Telemetry
    let (mut chunks_total, mut chunks_matched, mut chunks_no_match) = (0usize, 0usize, 0usize);
    let (mut set_hover, mut set_def, mut set_refs) = (0usize, 0usize, 0usize);

    // Warn early for missing per-file symbol tables
    for c in chunks.iter() {
        if let Some((key, _)) = normalize_to_repo_key(repo_root_abs, &c.file) {
            if key.ends_with(".dart") && !per_file_syms.contains_key(&key) {
                warn!(file=%c.file, key=%key, "no per_file_syms for this Dart file");
            }
        } else {
            debug!(file=%c.file, "file not under repo root; skipping LSP for it");
        }
    }

    for c in chunks.iter_mut() {
        chunks_total += 1;

        let Some((file_key, abs)) = normalize_to_repo_key(repo_root_abs, &c.file) else {
            trace!(chunk_id=%c.id, file=%c.file, "outside repo; skip");
            continue;
        };

        // Safety: span bounds vs current file size (best-effort)
        if let Some(len) = file_len_bytes(&abs) {
            if c.span.end_byte > len {
                warn!(
                    chunk_id=%c.id,
                    file=%file_key,
                    chunk_range=%format!("{}..{}", c.span.start_byte, c.span.end_byte),
                    file_len=len,
                    "chunk span out-of-bounds for current file"
                );
            }
        }

        // Resolve best-overlapping LSP symbol
        let syms = per_file_syms.get(&file_key).cloned().unwrap_or_default();
        let best_idx = best_overlap_index(&c.span, &syms);

        // Prepare LSP object
        let mut lsp = c.lsp.take().unwrap_or_default();

        // Ensure FQN/tags once
        if lsp.fqn.is_none() {
            lsp.fqn = Some(format!("{}::{}", file_key, c.symbol_path));
        }
        if lsp.tags.is_empty() {
            let mut tags = BTreeSet::new();
            let kind = format!("{:?}", c.kind).to_lowercase();
            tags.insert(format!("file:{}", file_key));
            tags.insert(format!("kind:{}", kind));
            lsp.tags = tags;
        }

        // Diagnostics counters per-file (errors, warnings)
        if let Some((errs, warns)) = per_file_diag_counts.get(&file_key) {
            lsp.diagnostics_errors = *errs;
            lsp.diagnostics_warnings = *warns;
        }

        // Enrich with hover/def/refs if we have a symbol candidate
        if let Some(i) = best_idx {
            let _sym = &syms[i];

            let (hov_set, sig_set) = enrich_hover_min(
                client,
                repo_root_abs,
                &file_key,
                c.span.start_row,
                c.span.start_col,
                &mut lsp,
            )?;
            if hov_set || sig_set {
                set_hover += 1;
            }

            let def_added = enrich_definition(
                client,
                repo_root_abs,
                &file_key,
                c.span.start_row,
                c.span.start_col,
                &mut lsp,
            )?;
            if def_added {
                set_def += 1;
            }

            let refs_added = enrich_references(
                client,
                repo_root_abs,
                &file_key,
                c.span.start_row,
                c.span.start_col,
                &mut lsp,
            )?;
            if refs_added {
                set_refs += 1;
            }

            chunks_matched += 1;
        } else {
            warn!(chunk_id=%c.id, file=%file_key, "no LSP symbol overlap for chunk");
            chunks_no_match += 1;
        }

        c.lsp = Some(lsp);

        debug!(
            chunk_id=%c.id,
            file_key=%file_key,
            has_hover=c.lsp.as_ref().map(|l| l.hover_one_liner.is_some()).unwrap_or(false),
            has_def=c.lsp.as_ref().map(|l| l.definition.is_some()).unwrap_or(false),
            refs=c.lsp.as_ref().map(|l| l.references_count).unwrap_or(0),
            errs=c.lsp.as_ref().map(|l| l.diagnostics_errors).unwrap_or(0),
            warns=c.lsp.as_ref().map(|l| l.diagnostics_warnings).unwrap_or(0),
            "chunk.lsp updated"
        );
    }

    info!(
        chunks_total,
        chunks_matched, chunks_no_match, set_hover, set_def, set_refs, "merge pass summary"
    );

    Ok(())
}

/* ─────────────── helpers ─────────────── */

fn file_len_bytes(abs: &PathBuf) -> Option<usize> {
    std::fs::metadata(abs).ok().map(|m| m.len() as usize)
}

fn enrich_hover_min(
    client: &mut LspProcess,
    repo_root_abs: &Path,
    file_key: &str,
    line: usize,
    col: usize,
    lsp: &mut LspEnrichment,
) -> Result<(bool, bool)> {
    let abs = abs_canonical(&repo_root_abs.join(file_key));
    let uri = file_uri_abs(&abs);

    let id = client.next_id();
    client.send(&json!({
        "jsonrpc":"2.0","id":id,"method":"textDocument/hover",
        "params":{"textDocument":{"uri":uri},"position":{"line":line,"character":col}}
    }))?;

    let mut hover_set = false;
    let mut sig_set = false;

    loop {
        match client.recv()? {
            RpcMessage::Response {
                id: rid,
                result,
                error,
            } if rid == id => {
                if let Some(e) = error {
                    warn!(%e, "hover error");
                    break;
                }
                if let Some(v) = result {
                    let raw = v
                        .pointer("/contents/value")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            v.pointer("/contents")?.as_array().and_then(|arr| {
                                let mut buf = String::new();
                                for item in arr {
                                    if let Some(s) = item.as_str() {
                                        if !buf.is_empty() {
                                            buf.push_str("\n\n");
                                        }
                                        buf.push_str(s);
                                    } else if let Some(vs) =
                                        item.pointer("/value").and_then(|x| x.as_str())
                                    {
                                        if !buf.is_empty() {
                                            buf.push_str("\n\n");
                                        }
                                        buf.push_str(vs);
                                    }
                                }
                                if buf.is_empty() { None } else { Some(buf) }
                            })
                        });

                    if let Some(full) = raw {
                        let one = clean_hover_first_line(&full, 256);
                        if lsp.hover_one_liner.is_none() && !one.is_empty() {
                            lsp.hover_one_liner = Some(one.clone());
                            hover_set = true;
                        }
                        if lsp.signature.is_none() && !one.is_empty() {
                            lsp.signature = Some(truncate(one, 256));
                            sig_set = true;
                        }
                    }
                }
                break;
            }
            RpcMessage::Notification { .. } => {}
            _ => {}
        }
    }

    Ok((hover_set, sig_set))
}

fn enrich_definition(
    client: &mut LspProcess,
    repo_root_abs: &Path,
    file_key: &str,
    line: usize,
    col: usize,
    lsp: &mut LspEnrichment,
) -> Result<bool> {
    let abs = abs_canonical(&repo_root_abs.join(file_key));
    let uri = file_uri_abs(&abs);

    let id = client.next_id();
    client.send(&json!({
        "jsonrpc":"2.0","id":id,"method":"textDocument/definition",
        "params":{"textDocument":{"uri":uri},"position":{"line":line,"character":col}}
    }))?;

    let mut def_added = false;
    loop {
        match client.recv()? {
            RpcMessage::Response {
                id: rid,
                result,
                error,
            } if rid == id => {
                if let Some(e) = error {
                    warn!(%e, "definition error");
                    break;
                }
                if let Some(v) = result {
                    let mut defs: Vec<DefLocation> = Vec::new();
                    if v.is_array() {
                        for item in v.as_array().unwrap() {
                            append_def_location(item, repo_root_abs, &mut defs);
                        }
                    } else {
                        append_def_location(&v, repo_root_abs, &mut defs);
                    }
                    if let Some(primary) = defs.into_iter().next() {
                        if lsp.definition.is_none() {
                            lsp.definition = Some(primary);
                            def_added = true;
                        }
                    }
                }
                break;
            }
            RpcMessage::Notification { .. } => {}
            _ => {}
        }
    }
    Ok(def_added)
}

fn enrich_references(
    client: &mut LspProcess,
    repo_root_abs: &Path,
    file_key: &str,
    line: usize,
    col: usize,
    lsp: &mut LspEnrichment,
) -> Result<bool> {
    let abs = abs_canonical(&repo_root_abs.join(file_key));
    let uri = file_uri_abs(&abs);

    let id = client.next_id();
    client.send(&json!({
        "jsonrpc":"2.0","id":id,"method":"textDocument/references",
        "params":{
            "textDocument":{"uri":uri},
            "position":{"line":line,"character":col},
            "context":{"includeDeclaration": false}
        }
    }))?;

    let mut added = false;
    loop {
        match client.recv()? {
            RpcMessage::Response {
                id: rid,
                result,
                error,
            } if rid == id => {
                if let Some(e) = error {
                    warn!(%e, "references error");
                    break;
                }
                if let Some(v) = result {
                    let mut refs: Vec<DefLocation> = Vec::new();
                    if let Some(arr) = v.as_array() {
                        for item in arr {
                            append_def_location(item, repo_root_abs, &mut refs);
                        }
                    }
                    if !refs.is_empty() {
                        let total = refs.len() as u32;
                        refs.truncate(MAX_REFS_SAMPLE);
                        lsp.references_count = total;
                        lsp.references_sample = refs;
                        added = true;
                    }
                }
                break;
            }
            RpcMessage::Notification { .. } => {}
            _ => {}
        }
    }
    Ok(added)
}

// Convert a Location/LocationLink to DefLocation and normalize target path to repo-relative when possible.
fn append_def_location(loc: &serde_json::Value, repo_root_abs: &Path, out: &mut Vec<DefLocation>) {
    if let Some(turi) = loc.pointer("/uri").and_then(|x| x.as_str()) {
        let range = extract_range(loc.get("range"));
        out.push(DefLocation {
            origin: classify_origin(turi),
            target: normalize_target_rel(turi, repo_root_abs),
            range,
            byte_range: None,
        });
        return;
    }
    if let Some(turi) = loc.pointer("/targetUri").and_then(|x| x.as_str()) {
        let range = extract_range(loc.get("targetRange"));
        out.push(DefLocation {
            origin: classify_origin(turi),
            target: normalize_target_rel(turi, repo_root_abs),
            range,
            byte_range: None,
        });
    }
}

fn extract_range(rr: Option<&serde_json::Value>) -> Option<(usize, usize, usize, usize)> {
    let r = rr?;
    Some((
        r.pointer("/start/line")?.as_u64()? as usize,
        r.pointer("/start/character")?.as_u64()? as usize,
        r.pointer("/end/line")?.as_u64()? as usize,
        r.pointer("/end/character")?.as_u64()? as usize,
    ))
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

// Assumes `repo_rel_key(path, repo_root_abs)` exists elsewhere.
// Assumes `repo_rel_key(path, repo_root_abs)` exists elsewhere.
fn normalize_target_rel(uri: &str, repo_root_abs: &Path) -> String {
    // Helper: stringify a path with forward slashes on all platforms
    fn norm_slashes(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    if let Ok(u) = Url::parse(uri) {
        if u.scheme() == "file" {
            if let Ok(abs) = u.to_file_path() {
                // Case 1: path is inside the repo -> keep the original behavior
                if abs.starts_with(repo_root_abs) {
                    return repo_rel_key(&abs, repo_root_abs);
                }

                // Case 2: Flutter SDK special cases
                // If the absolute path contains ".../flutter/bin/..." or ".../flutter/packages/...",
                // drop everything before that segment and prefix with "flutter_sdk/".
                let parts: Vec<&OsStr> = abs.iter().collect();
                for i in 0..parts.len().saturating_sub(1) {
                    if parts[i] == OsStr::new("flutter") {
                        let next = parts[i + 1];
                        if next == OsStr::new("bin") || next == OsStr::new("packages") {
                            // Build "flutter_sdk/<next>/..." from the "<next>/..." suffix
                            let suffix: PathBuf = parts[i + 1..].iter().collect();
                            let mut new_rel = PathBuf::from("flutter_sdk");
                            new_rel.push(suffix);
                            return norm_slashes(&new_rel);
                        }
                    }
                }
            }
        }
    }

    // Fallback: return the original URI as-is
    uri.to_string()
}

/// Strip markdown fences and return the first non-empty trimmed line.
fn clean_hover_first_line(s: &str, max_chars: usize) -> String {
    let mut in_fence = false;
    for raw_line in s.lines() {
        let line = raw_line.trim();
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let mut out = line.to_string();
        if out.len() > max_chars {
            out.truncate(max_chars);
        }
        return out;
    }
    first_line(s, max_chars)
}
