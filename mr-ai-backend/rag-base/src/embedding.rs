//! Text helpers and **Ollama-based** embedding utilities.
//!
//! This module keeps the embedding concern isolated from storage and parsing:
//! - Build compact, high-signal text inputs for embedding.
//! - Call **Ollama** `/api/embeddings` endpoint and return dense vectors.
//! - Validate vector dimensionality against `RagConfig.embedding.dim`.
//!
//! ## Environment
//! - `OLLAMA_URL` (default: `http://localhost:11434`)
//!
//! ## Notes
//! - Requests are executed **sequentially** for simplicity. If you need higher
//!   throughput, call `embed_texts_ollama` from multiple tasks or extend it to
//!   run multiple in-flight requests capped by `cfg.embedding.concurrency`.
//! - Errors are mapped to `RagBaseError::Embedding` with descriptive messages.

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::errors::rag_base_error::RagBaseError;
use crate::structs::rag_base_config::RagConfig;

/// Returns a clamped copy of `s` limited by `max_chars` and `max_lines`.
///
/// Rules:
/// - Stops at the earliest limit (lines or chars).
/// - Preserves line boundaries up to the limit.
/// - Appends an ellipsis `…` if truncation occurred and `add_ellipsis` is true.
/// - Keeps UTF-8 boundary correctness when trimming.
///
/// This helper is safe to use for preview snippets and embedding inputs.
pub fn clamp_snippet_ex(s: &str, max_chars: usize, max_lines: usize, add_ellipsis: bool) -> String {
    if s.is_empty() || max_chars == 0 {
        return String::new();
    }

    let mut out = String::new();
    let mut total = 0usize;
    let mut lines = 0usize;
    let mut truncated = false;

    for (i, line) in s.lines().enumerate() {
        if max_lines > 0 && lines >= max_lines {
            truncated = true;
            break;
        }
        let need = line.len() + if i > 0 { 1 } else { 0 };
        if total + need > max_chars {
            truncated = true;
            break;
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
        total += need;
        lines += 1;
    }

    if truncated && add_ellipsis {
        let ell = '…';
        let ell_len = ell.len_utf8();
        if total + ell_len <= max_chars {
            out.push(ell);
        } else {
            // Trim until we can safely place the ellipsis and keep UTF-8 intact.
            while out.len() + ell_len > max_chars && !out.is_empty() {
                out.pop();
                // Pop continuation bytes (10xxxxxx) until reaching a char boundary.
                while !out.is_empty()
                    && (out.as_bytes()[out.len() - 1] & 0b1100_0000) == 0b1000_0000
                {
                    out.pop();
                }
            }
            if !out.is_empty() {
                out.push(ell);
            }
        }
    }

    out
}

/// Build a compact, high-signal text for embeddings.
///
/// The intent is to keep the representation short yet informative for k-NN search.
/// Include language, kind, symbol path, optional signature/doc, a clamped snippet,
/// and imports (normalized as comma-separated list).
///
/// `max_chars` controls the budget for the snippet part (before ellipsis).
pub fn build_embedding_text(
    language: &str,
    kind: &str,
    symbol_path: &str,
    signature: Option<&str>,
    doc: Option<&str>,
    snippet: Option<&str>,
    imports: &[String],
    max_chars: usize,
) -> String {
    let mut parts = Vec::with_capacity(6);
    parts.push(format!("{language} | {kind} | {symbol_path}"));

    if let Some(sig) = signature {
        if !sig.is_empty() {
            parts.push(format!("Signature: {sig}"));
        }
    }
    if let Some(d) = doc {
        if !d.is_empty() {
            let first = d.lines().next().unwrap_or(d);
            parts.push(format!("Doc: {first}"));
        }
    }
    if !imports.is_empty() {
        parts.push(format!("Imports: {}", imports.join(", ")));
    }
    if let Some(sn) = snippet {
        let clamp = clamp_snippet_ex(sn, max_chars, 50, true);
        if !clamp.is_empty() {
            parts.push("Snippet:".into());
            parts.push(clamp);
        }
    }
    parts.join("\n")
}

/// Request shape for Ollama embeddings API.
#[derive(Debug, Serialize)]
struct OllamaEmbedRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    // Ollama also supports options; omitted for simplicity.
}

/// Response shape for Ollama embeddings API.
#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embedding: Vec<f32>,
}

/// Embed a batch of texts using **Ollama** `/api/embeddings`.
///
/// Each text is sent as an individual request. This keeps memory stable and
/// simplifies error handling. To scale throughput, call this function from
/// multiple async tasks or adapt it to dispatch multiple in-flight requests
/// up to `cfg.embedding.concurrency`.
///
/// # Errors
/// - Returns `RagBaseError::Embedding` if the HTTP call fails, server returns
///   non-success status, JSON parsing fails, or the resulting vector dimension
///   does not match `cfg.embedding.dim`.
pub async fn embed_texts_ollama(
    cfg: &RagConfig,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, RagBaseError> {
    let base = std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".into());
    let url = format!("{base}/api/embeddings");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| RagBaseError::Embedding(format!("http client build: {e}")))?;

    let mut out = Vec::with_capacity(texts.len());

    for text in texts {
        let req = OllamaEmbedRequest {
            model: &cfg.embedding.model,
            prompt: text,
        };

        let resp = client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| RagBaseError::Embedding(format!("POST {url}: {e}")))?;

        if resp.status() != StatusCode::OK {
            let code = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".into());
            return Err(RagBaseError::Embedding(format!(
                "ollama embeddings non-200: {code}; body: {body}"
            )));
        }

        let parsed: OllamaEmbedResponse = resp
            .json()
            .await
            .map_err(|e| RagBaseError::Embedding(format!("parse embeddings json: {e}")))?;

        if parsed.embedding.len() != cfg.embedding.dim {
            return Err(RagBaseError::Embedding(format!(
                "embedding dim {} != expected {} (model: {})",
                parsed.embedding.len(),
                cfg.embedding.dim,
                cfg.embedding.model
            )));
        }

        out.push(parsed.embedding);
    }

    Ok(out)
}
