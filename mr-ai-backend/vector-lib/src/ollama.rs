use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Serialize)]
struct EmbedReq<'a> {
    model: &'a str,
    input: &'a str, // ВАЖНО: input, не prompt
}

#[derive(Deserialize)]
struct EmbedResp {
    embedding: Option<Vec<f32>>,
    embeddings: Option<Vec<Vec<f32>>>,
}

pub struct OllamaEmb {
    client: Client,
    base: String,
    model: String,
    sem: Arc<Semaphore>,
}

impl OllamaEmb {
    pub fn new(base: impl Into<String>, model: impl Into<String>, concurrency: usize) -> Self {
        let c = concurrency.max(1);
        Self {
            client: Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            model: model.into(),
            sem: Arc::new(Semaphore::new(c)),
        }
    }

    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let _permit = self.sem.acquire().await.expect("semaphore");
        let url = format!("{}/api/embed", self.base);
        let body = EmbedReq {
            model: &self.model,
            input: text,
        };

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {} failed", url))?
            .error_for_status()
            .with_context(|| format!("Non-2xx from {}", url))?
            .json::<EmbedResp>()
            .await
            .context("Failed to parse Ollama embed response")?;

        if let Some(v) = resp.embedding {
            return Ok(v);
        }
        if let Some(vs) = resp.embeddings {
            return vs.into_iter().next().context("empty embeddings");
        }
        Err(anyhow::anyhow!("no embedding returned"))
    }

    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let par = self.sem.available_permits().max(1);
        let results = stream::iter(texts.iter().cloned())
            .map(|t| async move { self.embed(&t).await })
            .buffer_unordered(par)
            .collect::<Vec<_>>()
            .await;

        let mut out = Vec::with_capacity(results.len());
        for r in results {
            out.push(r?);
        }
        Ok(out)
    }
}
