use qdrant_client::Payload;
use serde::{Deserialize, Serialize};

/// Generic document we embed and store in Qdrant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorDoc {
    pub id: String,       // stable point id
    pub text: String,     // text used for embedding
    pub payload: Payload, // arbitrary metadata (also indexed in Qdrant payload)
}

/// Raw embedding holder (internal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    pub id: String,
    pub vector: Vec<f32>,
    pub payload: Payload,
}

/// Search hit (post-processed Qdrant result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    pub score: f32,
    pub text: String,
    pub file: Option<String>,
    pub start_line: Option<u64>,
    pub end_line: Option<u64>,
    pub kind: Option<String>,
    pub payload: Payload,
}
