use anyhow::{Context, Result};
use rayon::prelude::*;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

/// Minimal Ollama embeddings client.
#[derive(Serialize)]
struct EmbReq<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Deserialize)]
struct EmbResp {
    embedding: Vec<f32>,
}

pub fn embed_ollama(ollama_url: &str, model: &str, text: &str) -> Result<Vec<f32>> {
    let cli = Client::new();
    let req = EmbReq { model, input: text };
    let resp = cli
        .post(format!("{}/api/embeddings", ollama_url))
        .json(&req)
        .send()
        .context("ollama request failed")?
        .error_for_status()
        .context("ollama non-200")?;
    let body: EmbResp = resp.json().context("invalid ollama response")?;
    Ok(body.embedding)
}

/// Compute embeddings in parallel using Rayon.
pub fn compute_embeddings_parallel(
    ollama_url: &str,
    model: &str,
    texts: &[String],
) -> Result<Vec<Vec<f32>>> {
    let results: Vec<Result<Vec<f32>>> = texts
        .par_iter()
        .map(|t| embed_ollama(ollama_url, model, t))
        .collect();

    let mut out = Vec::with_capacity(texts.len());
    for r in results {
        out.push(r?);
    }
    Ok(out)
}
