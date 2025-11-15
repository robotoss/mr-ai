//! Text helpers and Ollama-based embedding utilities.

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::errors::rag_base_error::RagBaseError;
use crate::structs::rag_base_config::RagConfig;

/// Returns a clamped copy of `s` limited by `max_chars` and `max_lines`.
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
            while out.len() + ell_len > max_chars && !out.is_empty() {
                out.pop();
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

/// Build compact, high-signal text for embeddings.
pub fn build_embedding_text(
    language: &str,
    kind: &str,
    symbol_path: &str,
    signature: Option<&str>,
    doc: Option<&str>,
    snippet: Option<&str>,
    imports_top: &[String],
    routes: &[String],
    keywords: &[String],
    max_snippet_chars: usize,
) -> String {
    // 1) Structural header
    let mut parts: Vec<String> = vec![format!("{language} | {kind} | {symbol_path}")];

    // 2) Signature and first doc line
    if let Some(sig) = signature {
        if !sig.is_empty() {
            parts.push(format!("Signature: {sig}"));
        }
    }
    if let Some(d) = doc {
        if !d.is_empty() {
            if let Some(first) = d.lines().next() {
                parts.push(format!("Doc: {first}"));
            }
        }
    }

    // 3) Top imports
    if !imports_top.is_empty() {
        parts.push(format!("Imports: {}", imports_top.join(", ")));
    }

    // 4) Routes
    if !routes.is_empty() {
        parts.push(format!("Routes: {}", routes.join(", ")));
    }

    // 5) Keywords (small top slice)
    if !keywords.is_empty() {
        let keep = keywords
            .iter()
            .take(16)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("Keywords: {keep}"));
    }

    // 6) Clamped snippet
    if let Some(sn) = snippet {
        let clamp = clamp_snippet_ex(sn, max_snippet_chars, 50, true);
        if !clamp.is_empty() {
            parts.push("Snippet:".into());
            parts.push(clamp);
        }
    }

    parts.join("\n")
}

#[derive(Debug, Serialize)]
struct OllamaEmbedRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embedding: Vec<f32>,
}

/// Embed texts via Ollama `/api/embeddings`.
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
