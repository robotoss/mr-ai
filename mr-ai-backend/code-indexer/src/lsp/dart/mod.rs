//! Dart LSP enricher (Dart ≥ 3.8.1), no custom AST.
//!
//! Pipeline (per file, only files referenced by chunks):
//! 1) Collect unique `.dart` files from chunks and ensure they exist.
//! 2) Discover workspaces (folders with `pubspec.yaml`) and run `pub get`.
//! 3) Start Dart Analysis Server, initialize with `rootUri` + `workspaceFolders`.
//! 4) For each file: `didOpen` → `documentSymbol` + `semanticTokens/full`.
//!    While waiting for responses, collect `publishDiagnostics` notifications.
//! 5) Parse responses into per-file symbols, semantic-token histograms,
//!    collect per-file diagnostics.
//! 6) Merge into `chunk.lsp`: signature, outline, semantic histogram/top-k,
//!    plus `hover`, `definition`, `references`, and nearby diagnostics at the chunk head.
//!
//! No custom AST layer: imports/uses/tags are not produced here.

mod client;
mod merge;
mod parse;
mod util;

use crate::errors::{Error, Result};
use crate::lsp::interface::LspProvider;
use crate::types::{CodeChunk, LspDiagnostic};

use client::LspProcess;
use merge::merge_file_enrichment_into_chunks;
use parse::{LspSymbolInfo, collect_from_document_symbol, decode_semantic_tokens_hist};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use util::{
    abs_path, build_workspace_folders_json_abs, common_parent_dir, file_uri_abs, parent_folder_set,
    uri_to_abs_path,
};

pub struct DartLsp;

impl LspProvider for DartLsp {
    fn enrich(_root: &Path, chunks: &mut [CodeChunk]) -> Result<()> {
        // 1) Unique `.dart` files from chunks
        let mut files: Vec<String> = chunks.iter().map(|c| c.file.clone()).collect();
        files.sort();
        files.dedup();

        let requested = files.len();
        let mut files_abs: Vec<PathBuf> = Vec::with_capacity(files.len());
        files.retain(|file| {
            if !file.ends_with(".dart") {
                debug!(%file, "skip non-dart file");
                return false;
            }
            let p = abs_path(Path::new(file));
            if p.exists() {
                files_abs.push(p);
                true
            } else {
                warn!(%file, "file not found, skipping");
                false
            }
        });
        if files_abs.is_empty() {
            warn!(
                requested_files = requested,
                "no existing .dart files among chunk references"
            );
            return Ok(());
        }
        info!(
            unique_files = files_abs.len(),
            requested_files = requested,
            "DartLsp: files ready"
        );

        // 2) Discover workspaces and run `pub get`
        let workspaces = discover_workspaces_from_files(&files_abs);
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
        let root_abs = common_parent_dir(&files_abs);
        let root_uri = file_uri_abs(&root_abs);
        info!(root=%root_abs.display(), %root_uri, "LSP root");
        for f in &workspaces {
            info!(ws=%f.display(), ws_uri=%file_uri_abs(f), "workspace folder");
        }
        let ws_folders = build_workspace_folders_json_abs(&workspaces);

        let mut client = LspProcess::start()?;
        let legend = lsp_initialize_and_get_legend(&mut client, Some(root_uri), Some(ws_folders))?;
        info!(
            legend_items = legend.len(),
            "LSP initialized; semanticTokens legend ready"
        );

        // 4) didOpen + requests per file
        let mut per_file_syms: HashMap<String, Vec<LspSymbolInfo>> = HashMap::new();
        let mut per_file_hist: HashMap<String, HashMap<String, u32>> = HashMap::new();
        let mut per_file_diags: HashMap<String, Vec<LspDiagnostic>> = HashMap::new();

        // Build file <-> uri maps to match incoming diagnostics to canonical file keys.
        let mut path_map: HashMap<String, PathBuf> = HashMap::new();
        let mut uri_for_file: HashMap<String, String> = HashMap::new();
        let mut file_for_uri: HashMap<String, String> = HashMap::new();

        for file in &files {
            let abs = abs_path(Path::new(file));
            let uri = file_uri_abs(&abs);
            path_map.insert(file.clone(), abs.clone());
            uri_for_file.insert(file.clone(), uri.clone());
            file_for_uri.insert(uri.clone(), file.clone());
        }

        for file in &files {
            let Some(path) = path_map.get(file) else {
                continue;
            };
            let uri = uri_for_file
                .get(file)
                .cloned()
                .unwrap_or_else(|| file_uri_abs(path));
            let text = fs::read_to_string(path).map_err(Error::from)?;

            debug!(%file, uri = %uri, len = text.len(), "didOpen");
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

            // semanticTokens/full
            let sem_id = client.next_id();
            client.send(&json!({
                "jsonrpc":"2.0","id":sem_id,"method":"textDocument/semanticTokens/full",
                "params":{"textDocument":{"uri":uri}}
            }))?;

            let (mut got_doc, mut got_sem) = (false, false);
            let (mut doc_payload, mut sem_payload): (Option<Value>, Option<Value>) = (None, None);

            // Read messages until both responses arrive. Collect diagnostics along the way.
            while !(got_doc && got_sem) {
                match client.recv()? {
                    client::RpcMessage::Response { id, result, error } if id == doc_id => {
                        got_doc = true;
                        if let Some(e) = error {
                            warn!(%file, error=?e, "documentSymbol error");
                        }
                        doc_payload = result;
                        if doc_payload.is_some() {
                            debug!(%file, "documentSymbol received");
                        }
                    }
                    client::RpcMessage::Response { id, result, error } if id == sem_id => {
                        got_sem = true;
                        if let Some(e) = error {
                            warn!(%file, error=?e, "semanticTokens error");
                        }
                        sem_payload = result;
                        if sem_payload.is_some() {
                            debug!(%file, "semanticTokens received");
                        }
                    }
                    client::RpcMessage::Notification { method, params } => {
                        if method == "textDocument/publishDiagnostics" {
                            if let Some((target_file, diags)) =
                                decode_publish_diagnostics(&params, &file_for_uri)
                            {
                                let e = per_file_diags.entry(target_file).or_default();
                                e.extend(diags);
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Parse Document Symbols → per-file list (used for signature/outline + matching)
            if let Some(res) = &doc_payload {
                // NOTE: collect_from_document_symbol(res: &Value, text: &str, file_key: &str) -> Vec<LspSymbolInfo>
                let list = collect_from_document_symbol(res, &text, file);
                if !list.is_empty() {
                    debug!(%file, symbols = list.len(), "documentSymbol parsed");
                    per_file_syms.insert(file.clone(), list);
                } else {
                    debug!(%file, "documentSymbol empty");
                }
            } else {
                debug!(%file, "documentSymbol response missing or empty");
            }

            // Parse Semantic Tokens → histogram
            if let Some(res) = &sem_payload {
                if let Some(hist) = decode_semantic_tokens_hist(res, &legend) {
                    debug!(%file, token_kinds = hist.len(), "semanticTokens histogram parsed");
                    per_file_hist.insert(file.clone(), hist);
                } else {
                    debug!(%file, "semanticTokens histogram empty");
                }
            } else {
                debug!(%file, "semanticTokens response missing or empty");
            }
        }

        // 5) Merge into chunks (inject hover/defs/refs + diagnostics)
        merge_file_enrichment_into_chunks(
            &mut client,
            chunks,
            &per_file_syms,
            &per_file_hist,
            &per_file_diags,
            &legend,
        )?;
        info!("merge pass completed");

        // 6) Shutdown
        let _ = client.shutdown();
        info!("DartLsp enrichment finished");
        Ok(())
    }
}

/* ===== Local helpers ====================================================== */

fn discover_workspaces_from_files(files_abs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: BTreeSet<PathBuf> = BTreeSet::new();
    for f in files_abs {
        if let Some(mut cur) = f.parent().map(|p| abs_path(p)) {
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
        let dir = abs_path(dir);
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

fn lsp_initialize_and_get_legend(
    client: &mut LspProcess,
    root_uri: Option<String>,
    workspace_folders: Option<Vec<Value>>,
) -> Result<Vec<String>> {
    let caps = json!({
        "workspace": { "workspaceFolders": true },
        "textDocument": {
            "hover": { "contentFormat": ["markdown","plaintext"] },
            "definition": { "dynamicRegistration": false },
            "references": { "dynamicRegistration": false },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "semanticTokens": {
                "dynamicRegistration": false,
                "requests": { "range": false, "full": true },
                "tokenTypes": [],
                "tokenModifiers": [],
                "formats": ["relative"],
                "overlappingTokenSupport": false,
                "multilineTokenSupport": true
            },
            // Some servers also support `textDocument/diagnostic`; Dart DAS usually uses publishDiagnostics.
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

    let mut legend: Vec<String> = Vec::new();
    loop {
        match client.recv()? {
            client::RpcMessage::Response { id, result, error } if id == init_id => {
                if let Some(e) = error {
                    warn!(error=?e, "LSP initialize error");
                    return Err(Error::LspProtocol("initialize failed"));
                }
                if let Some(res) = result {
                    if let Some(arr) = res
                        .pointer("/capabilities/semanticTokensProvider/legend/tokenTypes")
                        .and_then(|x| x.as_array())
                    {
                        legend = arr
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect();
                    }
                }
                break;
            }
            client::RpcMessage::Notification { .. } => {}
            _ => {}
        }
    }

    client.send(&json!({"jsonrpc":"2.0","method":"initialized","params":{}}))?;
    info!(legend_items = legend.len(), "LSP initialized");
    Ok(legend)
}

/// Decodes `publishDiagnostics` params to a (file, Vec<LspDiagnostic>) pair.
/// Uses `file_for_uri` to resolve a canonical file key used in `chunks`.
fn decode_publish_diagnostics(
    params: &Value,
    file_for_uri: &HashMap<String, String>,
) -> Option<(String, Vec<LspDiagnostic>)> {
    let uri = params.pointer("/uri").and_then(|x| x.as_str())?;
    let file = file_for_uri.get(uri).cloned().or_else(|| {
        // Fallback: convert URI to abs path and stringify it.
        uri_to_abs_path(uri).map(|p| p.to_string_lossy().to_string())
    })?;

    let mut out: Vec<LspDiagnostic> = Vec::new();
    if let Some(diags) = params.pointer("/diagnostics").and_then(|x| x.as_array()) {
        for d in diags {
            let severity = d.get("severity").and_then(|x| x.as_u64()).map(|v| v as u8);
            // `code` may be number|string|object
            let code = if let Some(s) = d.get("code").and_then(|x| x.as_str()) {
                Some(s.to_string())
            } else if let Some(n) = d.get("code").and_then(|x| x.as_i64()) {
                Some(n.to_string())
            } else if d.get("code").is_some() {
                Some("obj".to_string())
            } else {
                None
            };
            let message = d
                .get("message")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let source = d
                .get("source")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());

            let range = d.get("range").and_then(|rr| {
                Some((
                    rr.pointer("/start/line")?.as_u64()? as usize,
                    rr.pointer("/start/character")?.as_u64()? as usize,
                    rr.pointer("/end/line")?.as_u64()? as usize,
                    rr.pointer("/end/character")?.as_u64()? as usize,
                ))
            });

            out.push(LspDiagnostic {
                severity,
                code,
                message,
                range,
                source,
            });
        }
    }
    Some((file, out))
}
