use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;

pub fn ensure_collection(qdrant_url: &str, collection: &str, dim: usize) -> Result<()> {
    let cli = Client::new();
    let url = format!("{}/collections/{}", qdrant_url, collection);
    let body = json!({ "vectors": { "size": dim, "distance": "Cosine" } });
    let _ = cli.put(&url).json(&body).send().context("qdrant create")?;
    Ok(())
}

#[derive(Clone)]
pub struct PointData {
    pub id: String,
    pub vector: Vec<f32>,
    pub payload: serde_json::Value,
}

pub fn upsert_points_batched(
    qdrant_url: &str,
    collection: &str,
    points: &[PointData],
    batch_size: usize, // 0 = single request
) -> Result<()> {
    let cli = Client::new();
    let url = format!("{}/collections/{}/points", qdrant_url, collection);

    if batch_size == 0 {
        let body = to_upsert(points)?;
        let resp = cli
            .put(&url)
            .json(&body)
            .send()
            .context("qdrant upsert send")?;
        if !resp.status().is_success() {
            let txt = resp.text().unwrap_or_default();
            anyhow::bail!("qdrant upsert failed: {}", txt);
        }
        return Ok(());
    }

    for chunk in points.chunks(batch_size) {
        let body = to_upsert(chunk)?;
        let resp = cli
            .put(&url)
            .json(&body)
            .send()
            .context("qdrant upsert send")?;
        if !resp.status().is_success() {
            let txt = resp.text().unwrap_or_default();
            anyhow::bail!("qdrant upsert failed: {}", txt);
        }
    }
    Ok(())
}

fn to_upsert(slice: &[PointData]) -> Result<serde_json::Value> {
    let pts: Vec<serde_json::Value> = slice
        .iter()
        .map(|p| json!({ "id": p.id, "vector": p.vector, "payload": p.payload }))
        .collect();
    Ok(json!({ "points": pts }))
}
