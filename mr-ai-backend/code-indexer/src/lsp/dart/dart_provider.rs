//! Dart LSP provider using stdio transport to `dart language-server`.
//!
//! Flow:
//! 1) initialize → initialized
//! 2) for each .dart: didOpen → documentSymbol + semanticTokens/full
//! 3) build per-symbol semantic histograms
//! 4) merge into `CodeChunk.lsp`, then graceful LSP shutdown
//!
//! Notes:
//! - JSON-RPC is modeled as `RpcMessage` (Response | Notification).
//! - `error` responses are logged and converted to `Error` where critical.
//! - All positions/lengths/offsets are `usize` (UTF-16 → bytes conversion is handled).

use crate::errors::{Error, Result};
use crate::lsp::interface::LspProvider;
use crate::types::{CodeChunk, Span};

use crate::lsp::dart::dart_parse::{
    LspSymbolInfo, decode_semantic_tokens, lsp_pos_to_byte, lsp_range_to_span, semantic_histogram,
};

use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use walkdir::WalkDir;

/* =============================== JSON-RPC ================================ */

/// JSON-RPC message (either response or notification).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RpcMessage {
    /// Regular response for a request.
    Response {
        id: Value,
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<Value>,
    },
    /// Notification without id.
    Notification {
        method: String,
        #[serde(default)]
        params: Value,
    },
}

/* ============================== Entry point ============================= */

/// Dart LSP client.
pub struct DartLsp;

impl LspProvider for DartLsp {
    /// Enrich chunks in-place using Dart LSP (document symbols + semantic tokens).
    fn enrich(root: &Path, chunks: &mut [CodeChunk]) -> Result<()> {
        let files = collect_dart_files(root);
        if files.is_empty() {
            return Ok(());
        }

        let mut client = LspProcess::start()?;

        /* ------------------------------- initialize ------------------------------- */

        let init_id = client.next_id();
        client.send(&json!({
            "jsonrpc":"2.0","id":init_id,"method":"initialize",
            "params":{
                "processId":null,
                "rootUri":format!("file://{}", root.to_string_lossy()),
                "capabilities":{"textDocument":{"semanticTokens":{}}},
                "initializationOptions":{"outline":true,"flutterOutline":true}
            }
        }))?;

        let mut legend: Vec<String> = Vec::new();

        // Wait for initialize result and read legend if provided.
        loop {
            match client.recv()? {
                RpcMessage::Response { id, result, error } if id == init_id => {
                    if let Some(err) = error {
                        // tracing::error!(?err, "LSP initialize error");
                        eprintln!("LSP initialize error: {err}");
                        return Err(Error::LspProtocol("initialize failed"));
                    }
                    if let Some(res) = result {
                        if let Some(legend_val) =
                            res.pointer("/capabilities/semanticTokensProvider/legend/tokenTypes")
                        {
                            if let Some(arr) = legend_val.as_array() {
                                legend = arr
                                    .iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect();
                            }
                        }
                    }
                    break;
                }
                RpcMessage::Response { .. } => {
                    // some other response: ignore
                }
                RpcMessage::Notification { method, .. } => {
                    // diagnostics or progress: ignore for now
                    // tracing::debug!(%method, "LSP notification during init");
                    let _ = method;
                }
            }
        }

        client.send(&json!({"jsonrpc":"2.0","method":"initialized","params":{}}))?;

        /* ---------------- didOpen + documentSymbol + semanticTokens ---------------- */

        let mut per_file_syms: HashMap<String, Vec<LspSymbolInfo>> = HashMap::new();
        let mut per_file_hist: HashMap<String, BTreeMap<String, u32>> = HashMap::new();

        for path in &files {
            let uri = file_uri(path);
            let text = std::fs::read_to_string(path).map_err(Error::from)?;
            let file_key = norm_path(path);

            // didOpen
            client.send(&json!({
                "jsonrpc":"2.0","method":"textDocument/didOpen","params":{
                    "textDocument":{"uri":uri,"languageId":"dart","version":1,"text":text}
                }
            }))?;

            // documentSymbol
            let docsym_id = client.next_id();
            client.send(&json!({
                "jsonrpc":"2.0","id":docsym_id,"method":"textDocument/documentSymbol",
                "params":{"textDocument":{"uri":uri}}
            }))?;

            // semanticTokens/full
            let sem_id = client.next_id();
            client.send(&json!({
                "jsonrpc":"2.0","id":sem_id,"method":"textDocument/semanticTokens/full",
                "params":{"textDocument":{"uri":uri}}
            }))?;

            let mut got_doc = false;
            let mut got_sem = false;
            let mut decoded_sem: Option<Vec<(usize, usize, usize, usize)>> = None;

            while !(got_doc && got_sem) {
                match client.recv()? {
                    RpcMessage::Response { id, result, error } if id == docsym_id => {
                        if let Some(err) = error {
                            // tracing::warn!(?err, file=%file_key, "documentSymbol error");
                            eprintln!("documentSymbol error for {file_key}: {err}");
                            got_doc = true; // treat as received to avoid deadlock
                            continue;
                        }
                        if let Some(res) = result {
                            let infos = parse_document_symbols(&res, &text, &file_key);
                            per_file_syms
                                .entry(file_key.clone())
                                .or_default()
                                .extend(infos);
                        }
                        got_doc = true;
                    }
                    RpcMessage::Response { id, result, error } if id == sem_id => {
                        if let Some(err) = error {
                            // tracing::warn!(?err, file=%file_key, "semanticTokens error");
                            eprintln!("semanticTokens error for {file_key}: {err}");
                            got_sem = true;
                            continue;
                        }
                        if let Some(res) = result {
                            if let Some(hist) = parse_semantic_tokens(&res, &legend) {
                                per_file_hist.insert(file_key.clone(), hist);
                            }
                            decoded_sem = decode_sem_for_file(&res);
                        }
                        got_sem = true;
                    }
                    RpcMessage::Response { .. } => {
                        // unrelated response: ignore
                    }
                    RpcMessage::Notification { method, .. } => {
                        // diagnostics/progress: ignore or log
                        let _ = method;
                    }
                }
            }

            // Build per-symbol semantic histograms for this file.
            if let Some(decoded) = decoded_sem {
                if let Some(infos) = per_file_syms.get_mut(&file_key) {
                    build_symbol_semantic_hists(infos, &decoded, &legend, &text);
                }
            }
        }

        /* ---------------------------------- merge ---------------------------------- */

        merge_lsp_into_chunks(chunks, &per_file_syms, &per_file_hist)?;

        /* ------------------------------- shutdown ---------------------------------- */

        // Try graceful shutdown; do not fail indexing if server rejects it.
        if let Err(e) = client.shutdown() {
            // tracing::warn!(error=?e, "LSP shutdown failed");
            eprintln!("LSP shutdown failed: {e}");
        }

        Ok(())
    }
}

/* =============================== Parsing ================================= */

/// Parse `textDocument/documentSymbol` result into `LspSymbolInfo` list.
fn parse_document_symbols(res: &Value, code: &str, file_key: &str) -> Vec<LspSymbolInfo> {
    let mut out = Vec::<LspSymbolInfo>::new();
    if let Some(arr) = res.as_array() {
        for v in arr {
            collect_doc_symbol_recursive(v, code, file_key, &mut out);
        }
    }
    out
}

/// Recursively collect `DocumentSymbol` entries and convert to `LspSymbolInfo`.
fn collect_doc_symbol_recursive(
    v: &Value,
    code: &str,
    file_key: &str,
    out: &mut Vec<LspSymbolInfo>,
) {
    let range = v.get("range");
    let sel = v.get("selectionRange");

    if let (Some(r), Some(sr)) = (range, sel) {
        if let (Some(sl), Some(sc), Some(el), Some(ec)) = (
            json_get_usize(r, "/start/line"),
            json_get_usize(r, "/start/character"),
            json_get_usize(r, "/end/line"),
            json_get_usize(r, "/end/character"),
        ) {
            let span = lsp_range_to_span(code, sl, sc, el, ec);

            // Build a trimmed signature from selectionRange first line.
            let sig_line_s = json_get_usize(sr, "/start/line").unwrap_or(sl);
            let sig_char_s = json_get_usize(sr, "/start/character").unwrap_or(0);
            let sig_line_e = json_get_usize(sr, "/end/line").unwrap_or(sig_line_s);
            let sig_char_e = json_get_usize(sr, "/end/character").unwrap_or(0);

            let mut start_b = lsp_pos_to_byte(code, sig_line_s, sig_char_s);
            let mut end_b = lsp_pos_to_byte(code, sig_line_e, sig_char_e);
            if end_b < start_b {
                std::mem::swap(&mut start_b, &mut end_b);
            }
            start_b = start_b.min(code.len());
            end_b = end_b.min(code.len());
            let sig = Some(first_line(&code[start_b..end_b], 240));

            out.push(LspSymbolInfo {
                file: file_key.to_string(),
                range: span,
                signature: sig,
                definition_uri: None,
                references_count: None,
                semantic_hist: None,
                outline_code_range: None,
                flags: Vec::new(),
            });
        }
    }

    if let Some(children) = v.get("children").and_then(|c| c.as_array()) {
        for ch in children {
            collect_doc_symbol_recursive(ch, code, file_key, out);
        }
    }
}

/// Parse `textDocument/semanticTokens/full` result to a per-file histogram.
fn parse_semantic_tokens(res: &Value, legend: &[String]) -> Option<BTreeMap<String, u32>> {
    let data = res.get("data")?.as_array()?;
    let mut raw = Vec::<u32>::with_capacity(data.len());
    for v in data {
        raw.push(v.as_u64().unwrap_or(0) as u32);
    }
    let decoded = decode_semantic_tokens(&raw);
    let hist = semantic_histogram(&decoded, legend);
    Some(hist)
}

/// Decode semantic tokens array from LSP result for per-symbol distribution.
fn decode_sem_for_file(res: &Value) -> Option<Vec<(usize, usize, usize, usize)>> {
    let data = res.get("data")?.as_array()?;
    let mut raw = Vec::<u32>::with_capacity(data.len());
    for v in data {
        raw.push(v.as_u64().unwrap_or(0) as u32);
    }
    Some(decode_semantic_tokens(&raw))
}

/// Assign semantic tokens to symbols using byte-overlap and build per-symbol histograms.
fn build_symbol_semantic_hists(
    infos: &mut [LspSymbolInfo],
    decoded: &[(usize, usize, usize, usize)], // (line, col_u16, len_u16, type_idx)
    legend: &[String],
    code: &str,
) {
    // Convert tokens to byte spans for comparison with symbol spans.
    let mut token_spans: Vec<(usize, usize, usize)> = Vec::with_capacity(decoded.len());
    for &(line, col_u16, len_u16, ty) in decoded {
        let s = lsp_pos_to_byte(code, line, col_u16).min(code.len());
        let e = lsp_pos_to_byte(code, line, col_u16 + len_u16).min(code.len());
        let sb = s.min(e);
        let eb = e.max(s);
        token_spans.push((sb, eb, ty));
    }

    for info in infos.iter_mut() {
        let mut hist = BTreeMap::<String, u32>::new();
        for &(s, e, ty) in &token_spans {
            if byte_overlap(&info.range, s, e) {
                let name = legend
                    .get(ty)
                    .cloned()
                    .unwrap_or_else(|| format!("type#{ty}"));
                *hist.entry(name).or_default() += 1;
            }
        }
        if !hist.is_empty() {
            info.semantic_hist = Some(hist);
        }
    }
}

/* ================================ Merge ================================== */

/// Merge LSP symbol info and semantic histograms into chunks (in-place).
fn merge_lsp_into_chunks(
    chunks: &mut [CodeChunk],
    per_file_syms: &HashMap<String, Vec<LspSymbolInfo>>,
    per_file_hist: &HashMap<String, BTreeMap<String, u32>>,
) -> Result<()> {
    // Build an index that OWNS file keys to avoid borrowing from `chunks`.
    let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        by_file.entry(c.file.clone()).or_default().push(i);
    }
    for idxs in by_file.values_mut() {
        idxs.sort_by_key(|&i| chunks[i].span.start_byte);
    }

    // Immutable search for best-overlapping chunk in a file.
    let find_best = |file: &str,
                     s: &Span,
                     chunks_ro: &[CodeChunk],
                     index: &HashMap<String, Vec<usize>>|
     -> Option<usize> {
        let Some(candidates) = index.get(file) else {
            return None;
        };
        let pos = candidates
            .binary_search_by_key(&s.start_byte, |&i| chunks_ro[i].span.start_byte)
            .unwrap_or_else(|p| p);
        let window = candidates.iter().skip(pos.saturating_sub(4)).take(9);
        let mut best: Option<(usize, usize)> = None;
        for &i in window {
            let ov = overlap_len(&chunks_ro[i].span, s);
            if ov > 0 {
                match best {
                    Some((_, b)) if ov > b => best = Some((i, ov)),
                    None => best = Some((i, ov)),
                    _ => {}
                }
            }
        }
        best.map(|x| x.0)
    };

    // Pass 1: collect symbol→chunk matches without mutating chunks.
    let mut matches: Vec<(usize, LspSymbolInfo)> = Vec::new();
    for (file, infos) in per_file_syms {
        for info in infos {
            if info.file != *file {
                continue; // sanity check
            }
            if let Some(i) = find_best(file, &info.range, &*chunks, &by_file) {
                matches.push((i, info.clone()));
            }
        }
    }

    // Pass 2: apply per-symbol merges to matched chunks.
    for (i, info) in matches {
        let c = &mut chunks[i];
        let mut lsp = c.lsp.take().unwrap_or_default();

        if lsp.signature_lsp.is_none() {
            if let Some(sig) = &info.signature {
                if !sig.is_empty() {
                    lsp.signature_lsp = Some(sig.clone());
                }
            }
        }
        if lsp.definition_uri.is_none() {
            if let Some(u) = &info.definition_uri {
                lsp.definition_uri = Some(u.clone());
            }
        }
        if lsp.references_count.is_none() {
            if let Some(n) = info.references_count {
                lsp.references_count = Some(n);
            }
        }
        if lsp.outline_code_range.is_none() {
            if let Some(r) = info.outline_code_range {
                lsp.outline_code_range = Some(r);
            }
        }
        if !info.flags.is_empty() {
            lsp.flags.extend(info.flags);
            lsp.flags.sort();
            lsp.flags.dedup();
        }
        // Merge per-symbol semantic histogram.
        if let Some(sym_hist) = info.semantic_hist.clone() {
            let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
            for (k, v) in sym_hist {
                *m.entry(k).or_default() += v;
            }
            lsp.semantic_token_hist = Some(m);
        }

        c.lsp = Some(lsp);
    }

    // Pass 3: merge per-file semantic histograms across all chunks in the same file.
    for (file, hist) in per_file_hist {
        if let Some(idxs) = by_file.get(file) {
            for &i in idxs {
                let c = &mut chunks[i];
                let mut lsp = c.lsp.take().unwrap_or_default();
                let mut m = lsp.semantic_token_hist.take().unwrap_or_default();
                for (k, v) in hist {
                    *m.entry(k.clone()).or_default() += *v;
                }
                lsp.semantic_token_hist = Some(m);
                c.lsp = Some(lsp);
            }
        }
    }

    Ok(())
}

/* ============================== LSP process ============================== */

/// Thin stdio wrapper over `dart language-server`.
struct LspProcess {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    next_id: u64,
}

impl LspProcess {
    /// Start the language server and open stdio channels.
    fn start() -> Result<Self> {
        let mut child = Command::new("dart")
            .arg("language-server")
            .arg("--client-id")
            .arg("code-indexer")
            .arg("--client-version")
            .arg("0.2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| Error::Spawn("failed to start dart language-server"))?;

        let stdin = child.stdin.take().ok_or(Error::Spawn("no stdin"))?;
        let stdout = child.stdout.take().ok_or(Error::Spawn("no stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// Allocate the next JSON-RPC id (as `serde_json::Value`).
    fn next_id(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        Value::from(id)
    }

    /// Send a JSON-RPC message with `Content-Length` header.
    fn send(&mut self, json: &Value) -> Result<()> {
        let body = serde_json::to_vec(json)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin
            .write_all(header.as_bytes())
            .map_err(Error::from)?;
        self.stdin.write_all(&body).map_err(Error::from)?;
        self.stdin.flush().map_err(Error::from)
    }

    /// Receive one JSON-RPC message (blocking) by reading header and body.
    fn recv(&mut self) -> Result<RpcMessage> {
        // Read header up to CRLFCRLF.
        let mut header = Vec::<u8>::new();
        let mut last4 = [0u8; 4];
        let mut buf = [0u8; 1];

        loop {
            self.stdout.read_exact(&mut buf).map_err(Error::from)?;
            header.push(buf[0]);
            last4.rotate_left(1);
            last4[3] = buf[0];
            if &last4 == b"\r\n\r\n" {
                break;
            }
            if header.len() > 8192 {
                return Err(Error::LspProtocol("header too large"));
            }
        }

        // Parse content length.
        let header_s = String::from_utf8(header).map_err(Error::from)?;
        let mut content_len: usize = 0;
        for line in header_s.split("\r\n") {
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                content_len = v.trim().parse().unwrap_or(0);
            }
        }
        if content_len == 0 {
            return Err(Error::LspProtocol("missing content length"));
        }

        // Read body and deserialize.
        let mut body = vec![0u8; content_len];
        self.stdout.read_exact(&mut body).map_err(Error::from)?;
        let msg: RpcMessage = serde_json::from_slice(&body)?;
        Ok(msg)
    }

    /// Send `shutdown` then `exit`, then wait for the process.
    fn shutdown(&mut self) -> Result<()> {
        let shutdown_id = self.next_id();
        self.send(&json!({"jsonrpc":"2.0","id":shutdown_id,"method":"shutdown"}))?;

        // Wait a little for shutdown response (best effort).
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            if let Ok(msg) = self.recv() {
                match msg {
                    RpcMessage::Response { id, .. } if id == shutdown_id => break,
                    _ => {} // ignore others
                }
            } else {
                break;
            }
        }

        // Exit notification.
        self.send(&json!({"jsonrpc":"2.0","method":"exit","params":{}}))?;

        // Try graceful wait.
        let _ = self.child.wait();
        Ok(())
    }

    /// Try to terminate the process if still running.
    fn try_terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        // Safety net: if caller forgot to call shutdown, try to terminate.
        self.try_terminate();
    }
}

/* =============================== Utilities =============================== */

/// Collect `.dart` files under `root`, skipping common build/metadata folders.
fn collect_dart_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("dart") {
            continue;
        }
        let s = p.to_string_lossy().replace('\\', "/");
        if s.contains("/.git/") || s.contains("/build/") || s.contains("/.dart_tool/") {
            continue;
        }
        out.push(p.to_path_buf());
    }
    out
}

/// Normalize a path to forward-slash representation.
fn norm_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Build `file://` URI string for a path.
fn file_uri(p: &Path) -> String {
    format!("file://{}", p.to_string_lossy())
}

/// Byte overlap length between two spans.
fn overlap_len(a: &Span, b: &Span) -> usize {
    let s = a.start_byte.max(b.start_byte);
    let e = a.end_byte.min(b.end_byte);
    e.saturating_sub(s)
}

/// First line of the string truncated to `max_chars`.
fn first_line(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch == '\n' {
            break;
        }
        out.push(ch);
        if out.len() >= max_chars {
            break;
        }
    }
    out.trim().to_string()
}

/// Safe getter of `usize` from a JSON pointer.
fn json_get_usize(v: &Value, ptr: &str) -> Option<usize> {
    v.pointer(ptr)?.as_u64().map(|x| x as usize)
}

/// True if byte range [s, e) overlaps with the chunk span.
fn byte_overlap(span: &Span, s: usize, e: usize) -> bool {
    let ss = span.start_byte.max(s);
    let ee = span.end_byte.min(e);
    ee.saturating_sub(ss) > 0
}
