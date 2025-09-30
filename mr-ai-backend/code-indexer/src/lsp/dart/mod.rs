//! Dart LSP enricher with lightweight AST.
//!
//! Pipeline (file-scoped; chunks-only):
//! 1) Collect unique `.dart` files referenced by chunks.
//! 2) Discover workspaces (folders with `pubspec.yaml`) from those files; run pub get.
//! 3) Start Dart Analysis Server; initialize with root/workspace folders.
//! 4) For each file: didOpen → documentSymbol + semanticTokens/full.
//! 5) Parse LSP payloads, build per-file AST (imports, aliasing, usage).
//! 6) Merge LSP + AST into chunk.lsp (signatures, outline, hovers/defs, hist, imports_used, tags).
//!
//! NOTE: We *only* touch files referenced by incoming chunks; no AST persistence.

mod ast;
mod client;
mod merge;
mod parse;
mod util;

use crate::errors::{Error, Result};
use crate::lsp::interface::LspProvider;
use crate::types::CodeChunk;

use client::LspProcess;
use merge::merge_file_enrichment_into_chunks;
use parse::{LspSymbolInfo, collect_from_document_symbol, decode_semantic_tokens_hist};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};
use util::{
    abs_path, build_workspace_folders_json_abs, classify_pub_origin, common_parent_dir,
    file_uri_abs, parent_folder_set,
};

/// Public Dart LSP provider (workspace-aware, chunk-focused).
pub struct DartLsp;

impl LspProvider for DartLsp {
    /// Enrich chunks using LSP responses (only for files present in the chunks) and a lightweight AST.
    ///
    /// Steps:
    /// - Resolve and validate file set from chunks (only `.dart` files that exist).
    /// - Discover workspaces (pubspec.yaml) and run `pub get`.
    /// - Start LSP, initialize with rootUri + workspaceFolders, then `initialized`.
    /// - For each file: send `didOpen`, then request `documentSymbol` and `semanticTokens/full`.
    /// - Parse, build per-file AST, and merge into `chunk.lsp`. No disk writes.
    fn enrich(_root: &Path, chunks: &mut [CodeChunk]) -> Result<()> {
        // Step 1: collect unique .dart files from chunks, keep only existing
        let mut files: Vec<String> = chunks.iter().map(|c| c.file.clone()).collect();
        files.sort();
        files.dedup();

        let mut files_abs: Vec<PathBuf> = Vec::with_capacity(files.len());
        let before_cnt = files.len();
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
                requested_files = before_cnt,
                "no existing .dart files among chunk references"
            );
            return Ok(());
        }
        info!(
            unique_files = files_abs.len(),
            requested_files = before_cnt,
            "DartLsp: files ready"
        );

        // Step 2: discover workspaces
        let workspaces = discover_workspaces_from_files(&files_abs);
        if workspaces.is_empty() {
            warn!("No `pubspec.yaml` found near chunk files; LSP may lack full context");
        } else {
            info!(workspaces = workspaces.len(), "workspaces discovered");
            for ws in &workspaces {
                debug!(workspace = %ws.display(), "workspace");
            }
        }

        // Step 3: pub get per workspace (best-effort; fail if both flutter/dart get fail)
        run_pub_get_all(&workspaces)?;
        info!(workspaces = workspaces.len(), "pub get finished");

        // LSP root/workspaceFolders
        let root_abs = common_parent_dir(&files_abs);
        let root_uri = file_uri_abs(&root_abs);
        let ws_folders = build_workspace_folders_json_abs(&workspaces);

        // Step 4: start LSP and initialize
        let mut client = LspProcess::start()?;
        let legend = lsp_initialize_and_get_legend(&mut client, Some(root_uri), Some(ws_folders))?;
        info!(
            legend_items = legend.len(),
            "LSP initialized; semanticTokens legend ready"
        );

        // Accumulators keyed by the exact file string used inside `chunks`.
        let mut per_file_syms: HashMap<String, Vec<LspSymbolInfo>> = HashMap::new();
        let mut per_file_hist: HashMap<String, HashMap<String, u32>> = HashMap::new();

        // Map chunk path string → absolute path for reuse.
        let mut path_map: HashMap<String, PathBuf> = HashMap::new();
        for file in &files {
            path_map.insert(file.clone(), abs_path(Path::new(file)));
        }

        // Step 5: didOpen + requests per file
        for file in &files {
            let Some(path) = path_map.get(file) else {
                continue;
            };
            let uri = file_uri_abs(path);
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

            // Await both responses
            let mut got_doc = false;
            let mut got_sem = false;
            let mut doc_payload: Option<Value> = None;
            let mut sem_payload: Option<Value> = None;
            while !(got_doc && got_sem) {
                match client.recv()? {
                    client::RpcMessage::Response { id, result, error } if id == doc_id => {
                        got_doc = true;
                        if let Some(e) = error {
                            warn!(%file, error=?e, "documentSymbol error");
                        }
                        if let Some(r) = result {
                            debug!(%file, "documentSymbol received");
                            doc_payload = Some(r);
                        } else {
                            debug!(%file, "documentSymbol empty");
                        }
                    }
                    client::RpcMessage::Response { id, result, error } if id == sem_id => {
                        got_sem = true;
                        if let Some(e) = error {
                            warn!(%file, error=?e, "semanticTokens error");
                        }
                        if let Some(r) = result {
                            debug!(%file, "semanticTokens received");
                            sem_payload = Some(r);
                        } else {
                            debug!(%file, "semanticTokens empty");
                        }
                    }
                    client::RpcMessage::Notification { .. } => {}
                    _ => {}
                }
            }

            // Parse
            if let Some(res) = &doc_payload {
                let mut infos = collect_from_document_symbol(res, &text, file);
                // Backfill signature from "detail" first line when available and signature empty
                if let Some(arr) = res.as_array() {
                    for (i, n) in arr.iter().enumerate() {
                        if let Some(detail) = n.get("detail").and_then(|x| x.as_str()) {
                            let line = util::first_line(detail, 240);
                            if !line.is_empty() && i < infos.len() {
                                if infos[i]
                                    .signature
                                    .as_ref()
                                    .map(|s| s.is_empty())
                                    .unwrap_or(true)
                                {
                                    infos[i].signature = Some(line);
                                }
                            }
                        }
                    }
                }
                debug!(%file, symbols = infos.len(), "documentSymbol parsed");
                per_file_syms.insert(file.clone(), infos);
            }

            if let Some(res) = &sem_payload {
                if let Some(hist) = decode_semantic_tokens_hist(res, &legend) {
                    debug!(%file, token_kinds = hist.len(), "semanticTokens histogram parsed");
                    per_file_hist.insert(file.clone(), hist);
                } else {
                    debug!(%file, "semanticTokens histogram empty");
                }
            }
        }

        // Step 5b: build file AST (imports/aliases/usage) for each file we touched
        let mut per_file_ast = HashMap::new();
        for file in &files {
            if let Some(path) = path_map.get(file) {
                let code = fs::read_to_string(path).map_err(Error::from)?;
                let syms = per_file_syms.get(file).cloned().unwrap_or_default();
                let ast = ast::build_file_ast(file.clone(), &code, &syms);
                debug!(%file, imports = ast.imports.len(), uses = ast.uses.len(), "AST built");
                per_file_ast.insert(file.clone(), ast);
            }
        }

        // Step 6: merge into chunks
        merge_file_enrichment_into_chunks(
            &mut client,
            chunks,
            &per_file_syms,
            &per_file_hist,
            &per_file_ast,
        )?;
        info!("merge pass completed");

        // Shutdown (best-effort)
        let _ = client.shutdown();
        info!("DartLsp enrichment finished");
        Ok(())
    }
}

/* ===== Workspace helpers (local) ========================================== */

/// Discover Dart workspaces (folders with `pubspec.yaml`) based on the given absolute files.
///
/// Strategy:
/// - For each file, walk up from its parent to the filesystem root and record any folder
///   that contains `pubspec.yaml`.
/// - Deduplicate and sort the results.
/// - If nothing found, return a set of distinct parents of the files as a fallback.
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

/// Run `flutter pub get` per workspace directory; fallback to `dart pub get` on failure.
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

/// Initialize LSP and read semantic tokens legend.
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
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "semanticTokens": {
                "dynamicRegistration": false,
                "requests": { "range": false, "full": true },
                "tokenTypes": [],
                "tokenModifiers": [],
                "formats": ["relative"],
                "overlappingTokenSupport": false,
                "multilineTokenSupport": true
            }
        }
    });

    let init_id = client.next_id();
    client.send(&json!({
        "jsonrpc":"2.0","id":init_id,"method":"initialize",
        "params":{
            "processId": std::process::id(),
            "clientInfo": { "name": "code-indexer", "version": "1.0" },
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
