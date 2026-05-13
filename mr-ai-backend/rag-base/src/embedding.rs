//! Text helpers used to build inputs for the embedding gateway.
//!
//! Direct embedding HTTP calls have moved to `ai-llm-service`. This module
//! now only assembles compact text payloads suitable for embedding and
//! exposes a thin batch helper that delegates to the gateway.

use std::sync::Arc;

use ai_llm_service::{EmbeddingRequest, EmbeddingTier, LlmGateway};

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
    let mut parts: Vec<String> = vec![format!("{language} | {kind} | {symbol_path}")];

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

    if !imports_top.is_empty() {
        parts.push(format!("Imports: {}", imports_top.join(", ")));
    }

    if !routes.is_empty() {
        parts.push(format!("Routes: {}", routes.join(", ")));
    }

    if !keywords.is_empty() {
        let keep = keywords
            .iter()
            .take(16)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("Keywords: {keep}"));
    }

    if let Some(sn) = snippet {
        let clamp = clamp_snippet_ex(sn, max_snippet_chars, 50, true);
        if !clamp.is_empty() {
            parts.push("Snippet:".into());
            parts.push(clamp);
        }
    }

    parts.join("\n")
}

/// Embed a batch of texts via the LLM Gateway and validate dimensions
/// against `cfg.embedding.dim`.
pub async fn embed_texts(
    gateway: &Arc<LlmGateway>,
    cfg: &RagConfig,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, RagBaseError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let resp = gateway
        .embed_batch(EmbeddingTier::Default, EmbeddingRequest::new(texts.to_vec()))
        .await?;

    if resp.vectors.len() != texts.len() {
        return Err(RagBaseError::Embedding(format!(
            "embedding count mismatch: got {}, expected {}",
            resp.vectors.len(),
            texts.len()
        )));
    }

    for v in &resp.vectors {
        if v.len() != cfg.embedding.dim {
            return Err(RagBaseError::Embedding(format!(
                "embedding dim {} != expected {} (model: {})",
                v.len(),
                cfg.embedding.dim,
                resp.model
            )));
        }
    }

    Ok(resp.vectors)
}
