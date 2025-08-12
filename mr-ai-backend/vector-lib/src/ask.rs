// ask.rs
use anyhow::Result;
use qdrant_client::Payload;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::{models::SearchHit, ollama::OllamaEmb, qdrant::QdrantStore};

/// Embed the natural-language question, search in Qdrant, and map to convenient hits.
/// Assumes payload keys during ingest: "text", "file", "start_line", "end_line", "kind".
pub async fn ask_question(
    qdrant: &QdrantStore,
    collection: &str,
    emb: &OllamaEmb,
    question: &str,
    top_k: u64,
) -> Result<Vec<SearchHit>> {
    // 1) Vectorize the question with Ollama
    let qvec = emb.embed(question).await?;

    // 2) Search in Qdrant (threshold 0.2 is a sane default for cosine)
    let points = qdrant
        .search(collection, qvec, top_k, Some(0.2), true)
        .await?;

    // 3) Convert each ScoredPoint payload (HashMap<String, qdrant::Value>) into serde_json
    let mut out = Vec::with_capacity(points.len());
    for p in points.into_iter() {
        let id = point_id_to_string(&p);
        let score = p.score;

        // Convert payload map -> serde_json::Map<String, serde_json::Value>
        // NOTE: Do NOT try to use &Payload::get here; convert via .into_json() first.
        let mut json_map: JsonMap<String, JsonValue> = JsonMap::new();
        for (k, v) in p.payload.into_iter() {
            json_map.insert(k, v.into_json());
        }

        // Extract common fields (do not remove them from the map; we keep full payload)
        let text = json_map
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let file = json_map
            .get("file")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let kind = json_map
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let start_line = json_map.get("start_line").and_then(|v| v.as_u64());

        let end_line = json_map.get("end_line").and_then(|v| v.as_u64());

        // Rebuild Payload (qdrant_client::Payload) from the JSON map so callers still get a Payload
        let payload: Payload = Payload::try_from(JsonValue::Object(json_map.clone()))?;

        out.push(SearchHit {
            id,
            score,
            text,
            file,
            start_line,
            end_line,
            kind,
            payload,
        });
    }

    Ok(out)
}

/// Extract a stable string ID from ScoredPoint.
/// Qdrant points commonly have UUID or numeric IDs.
fn point_id_to_string(p: &qdrant_client::qdrant::ScoredPoint) -> String {
    use qdrant_client::qdrant::point_id::PointIdOptions;
    match p.id.as_ref().and_then(|pid| pid.point_id_options.as_ref()) {
        Some(PointIdOptions::Uuid(u)) => u.clone(),
        Some(PointIdOptions::Num(n)) => n.to_string(),
        _ => String::new(),
    }
}
