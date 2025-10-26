//! Dart LSP enricher (Dart ≥ 3.8.1), minimal fields for retrieval.
//!
//! Strict path normalization: convert `chunk.file` → ABS → repo-relative key.

mod client;
mod merge;
mod parse;
mod util;

use crate::errors::{Error, Result};
use crate::lsp::dart::client::{LspProcess, RpcMessage};
use crate::lsp::dart::merge::merge_file_enrichment_into_chunks;
use crate::lsp::dart::parse::{LspSymbolInfo, collect_from_document_symbol};
use crate::lsp::dart::util::{
    abs_canonical, build_workspace_folders_json_abs, file_uri_abs, normalize_to_repo_key,
    parent_folder_set, repo_rel_key, uri_to_abs_path,
};
use crate::lsp::interface::LspProvider;
use crate::types::CodeChunk;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Minimal diagnostic point per file: (severity, start_line).
/// severity: 1 = Error, 2 = Warning (per LSP spec).
type DiagPoint = (u8, usize);

pub struct DartLsp;

impl LspProvider for DartLsp {
    fn enrich(root: &Path, chunks: &mut [CodeChunk]) -> Result<()> {
        // 0) Resolve repo root
        let repo_root_abs = abs_canonical(root);

        // 1) Normalize chunk files to (repo-key, abs); keep existing .dart files only
        let mut keys: Vec<String> = Vec::new();
        let mut files_abs: Vec<PathBuf> = Vec::new();

        for c in chunks.iter() {
            if !c.file.ends_with(".dart") {
                debug!(file=%c.file, "skip non-dart file");
                continue;
            }
            if let Some((key, abs)) = normalize_to_repo_key(&repo_root_abs, &c.file) {
                if abs.exists() {
                    keys.push(key);
                    files_abs.push(abs);
                } else {
                    warn!(file=%c.file, "normalized path does not exist, skipping");
                }
            } else {
                warn!(file=%c.file, root=%repo_root_abs.display(), "file not under repo root, skipping");
            }
        }

        let mut dedup_map: HashMap<String, PathBuf> = HashMap::new();
        for (i, k) in keys.into_iter().enumerate() {
            dedup_map.entry(k).or_insert_with(|| files_abs[i].clone());
        }

        if dedup_map.is_empty() {
            warn!("no existing .dart files among chunk references");
            return Ok(());
        }

        let mut files_keys: Vec<String> = dedup_map.keys().cloned().collect();
        files_keys.sort();
        let files_abs_sorted: Vec<PathBuf> =
            files_keys.iter().map(|k| dedup_map[k].clone()).collect();

        info!(unique_files = files_keys.len(), "DartLsp: files ready");

        // 2) Discover workspaces and run `pub get`
        let workspaces = discover_workspaces_from_files(&files_abs_sorted);
        if workspaces.is_empty() {
            warn!("No `pubspec.yaml` found near chunk files; LSP may lack full context");
        } else {
            info!(count = workspaces.len(), "workspaces discovered");
            for ws in &workspaces {
                debug!(workspace = %ws.display(), "workspace");
            }
        }
        run_pub_get_all(&workspaces)?;
        info!(workspaces = workspaces.len(), "pub get finished");

        // 3) Initialize LSP
        let root_uri = file_uri_abs(&repo_root_abs);
        info!(root=%repo_root_abs.display(), %root_uri, "LSP root");
        for f in &workspaces {
            info!(ws=%f.display(), ws_uri=%file_uri_abs(f), "workspace folder");
        }
        let ws_folders = build_workspace_folders_json_abs(&workspaces);

        let mut client = LspProcess::start()?;
        lsp_initialize(&mut client, Some(root_uri), Some(ws_folders))?;
        info!("LSP initialized");

        // 4) didOpen + documentSymbol; collect per-file symbols and diagnostics
        let mut per_file_syms: HashMap<String, Vec<LspSymbolInfo>> = HashMap::new();
        let mut per_file_diags: HashMap<String, Vec<DiagPoint>> = HashMap::new();

        // Build file <-> uri maps for diagnostics
        let mut uri_for_file: HashMap<String, String> = HashMap::new();
        let mut file_for_uri: HashMap<String, String> = HashMap::new();

        for (i, key) in files_keys.iter().enumerate() {
            let abs = &files_abs_sorted[i];
            let uri = file_uri_abs(abs);
            uri_for_file.insert(key.clone(), uri.clone());
            file_for_uri.insert(uri, key.clone());
        }

        for (i, key) in files_keys.iter().enumerate() {
            let abs = &files_abs_sorted[i];
            let uri = uri_for_file
                .get(key)
                .cloned()
                .unwrap_or_else(|| file_uri_abs(abs));
            let text = fs::read_to_string(abs).map_err(Error::from)?;

            debug!(file=%key, uri = %uri, len = text.len(), "didOpen");
            client.send(&json!({
                "jsonrpc":"2.0","method":"textDocument/didOpen","params":{
                    "textDocument":{"uri":uri,"languageId":"dart","version":1,"text":text}
                }
            }))?;

            // documentSymbol
            let doc_id = client.next_id();
            client.send(&json!({
                "jsonrpc":"2.0","id":doc_id,"method":"textDocument/documentSymbol",
                "params":{"textDocument":{"uri":uri}}
            }))?;

            let mut got_doc = false;
            let mut doc_payload: Option<Value> = None;

            while !got_doc {
                match client.recv()? {
                    RpcMessage::Response { id, result, error } if id == doc_id => {
                        got_doc = true;
                        if let Some(e) = error {
                            warn!(file=%key, error=?e, "documentSymbol error");
                        }
                        doc_payload = result;
                        if doc_payload.is_some() {
                            debug!(file=%key, "documentSymbol received");
                        }
                    }
                    RpcMessage::Notification { method, params } => {
                        if method == "textDocument/publishDiagnostics" {
                            if let Some((target_file, diags)) =
                                decode_publish_diagnostics(&params, &file_for_uri, &repo_root_abs)
                            {
                                per_file_diags.entry(target_file).or_default().extend(diags);
                            }
                        }
                    }
                    _ => {}
                }
            }

            if let Some(res) = &doc_payload {
                let list = collect_from_document_symbol(res, &text, key);
                if !list.is_empty() {
                    debug!(file=%key, symbols = list.len(), "documentSymbol parsed");
                    per_file_syms.insert(key.clone(), list);
                } else {
                    debug!(file=%key, "documentSymbol empty");
                }
            }
        }

        // 5) Aggregate diagnostics to (errors, warnings)
        let mut per_file_diag_counts: HashMap<String, (u32, u32)> = HashMap::new();
        for (file, points) in per_file_diags.into_iter() {
            let mut errs = 0u32;
            let mut warns = 0u32;
            for (sev, _) in points {
                if sev == 1 {
                    errs += 1;
                } else if sev == 2 {
                    warns += 1;
                }
            }
            per_file_diag_counts.insert(file, (errs, warns));
        }

        // 6) Merge into chunks (hover/defs/refs + diag aggregates)
        merge_file_enrichment_into_chunks(
            &mut client,
            &repo_root_abs,
            chunks,
            &per_file_syms,
            &per_file_diag_counts,
        )?;
        info!("merge pass completed");

        // 7) Shutdown
        let _ = client.shutdown();
        info!("DartLsp enrichment finished");
        Ok(())
    }
}

/* ===== Local helpers ====================================================== */

fn discover_workspaces_from_files(files_abs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for f in files_abs {
        if let Some(mut cur) = f.parent().map(|p| util::abs_path(p)) {
            loop {
                let pubspec = cur.join("pubspec.yaml");
                if pubspec.exists() {
                    found.insert(cur.clone());
                }
                if !cur.pop() {
                    break;
                }
            }
        }
    }
    let mut out: Vec<PathBuf> = found.into_iter().collect();
    if out.is_empty() {
        out = parent_folder_set(files_abs);
    }
    out.truncate(64);
    out
}

fn run_pub_get_all(workspaces: &[PathBuf]) -> Result<()> {
    if workspaces.is_empty() {
        return Ok(());
    }
    for dir in workspaces {
        let dir = util::abs_path(dir);
        info!("pub get: {}", dir.display());

        let flutter_ok = std::process::Command::new("flutter")
            .arg("pub")
            .arg("get")
            .current_dir(&dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !flutter_ok {
            warn!(
                "flutter pub get failed in {}, trying `dart pub get`",
                dir.display()
            );
            let dart_ok = std::process::Command::new("dart")
                .arg("pub")
                .arg("get")
                .current_dir(&dir)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

            if !dart_ok {
                return Err(Error::LspProtocol("pub get failed"));
            }
        }
    }
    Ok(())
}

fn lsp_initialize(
    client: &mut LspProcess,
    root_uri: Option<String>,
    workspace_folders: Option<Vec<Value>>,
) -> Result<()> {
    let caps = json!({
        "workspace": { "workspaceFolders": true },
        "textDocument": {
            "hover": { "contentFormat": ["markdown","plaintext"] },
            "definition": { "dynamicRegistration": false },
            "references": { "dynamicRegistration": false },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true }
        }
    });

    let init_id = client.next_id();
    client.send(&json!({
        "jsonrpc":"2.0","id":init_id,"method":"initialize",
        "params":{
            "processId": std::process::id(),
            "clientInfo": { "name": "mr-reviewer", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri,
            "capabilities": caps,
            "workspaceFolders": workspace_folders,
            "initializationOptions":{
                "outline": true,
                "flutterOutline": true,
                "onlyAnalyzeProjectsWithOpenFiles": false
            }
        }
    }))?;

    loop {
        match client.recv()? {
            RpcMessage::Response { id, result, error } if id == init_id => {
                if let Some(e) = error {
                    warn!(error=?e, "LSP initialize error");
                    return Err(Error::LspProtocol("initialize failed"));
                }
                if result.is_none() {
                    return Err(Error::LspProtocol("initialize: empty result"));
                }
                break;
            }
            RpcMessage::Notification { .. } => {}
            _ => {}
        }
    }

    client.send(&json!({"jsonrpc":"2.0","method":"initialized","params":{}}))?;
    Ok(())
}

/// Decode `publishDiagnostics` and map its URI to repo-relative key.
/// Returns minimal info for aggregation: Vec<(severity, start_line)>.
fn decode_publish_diagnostics(
    params: &Value,
    file_for_uri: &HashMap<String, String>,
    repo_root_abs: &Path,
) -> Option<(String, Vec<DiagPoint>)> {
    let uri = params.pointer("/uri").and_then(|x| x.as_str())?;
    let file = if let Some(k) = file_for_uri.get(uri) {
        k.clone()
    } else {
        let abs = uri_to_abs_path(uri)?;
        if !abs.starts_with(repo_root_abs) {
            return None;
        }
        repo_rel_key(&abs, repo_root_abs)
    };

    let mut out: Vec<DiagPoint> = Vec::new();
    if let Some(diags) = params.pointer("/diagnostics").and_then(|x| x.as_array()) {
        for d in diags {
            let severity = d.get("severity").and_then(|x| x.as_u64()).map(|v| v as u8);
            let start_line = d
                .get("range")
                .and_then(|rr| rr.pointer("/start/line"))
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            if let (Some(sev), Some(sl)) = (severity, start_line) {
                out.push((sev, sl));
            }
        }
    }
    Some((file, out))
}
