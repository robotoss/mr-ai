//! Thin, SDK-agnostic types for Qdrant upserts.
//!
//! We avoid pulling a client SDK into the library. Instead, we provide small
//! serializable structs that match Qdrant's expected wire shapes. The caller
//! (CLI/service) can then post these via any HTTP client or use a dedicated SDK.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Qdrant "point" object suitable for upsert.
/// See: https://qdrant.tech/documentation/concepts/points/
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantPoint {
    /// Unique identifier. We prefer string ids (UUID) for portability.
    pub id: String,
    /// Optional vector; can be omitted if only payload is updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f32>>,
    /// Arbitrary JSON payload (our `RagRecord` serialized as JSON or a subset).
    pub payload: Value,
}

impl QdrantPoint {
    /// Helper to build a point from a record and an optional vector.
    pub fn from_payload_json(
        id: impl Into<String>,
        payload: Value,
        vector: Option<&[f32]>,
    ) -> Self {
        Self {
            id: id.into(),
            vector: vector.map(|v| v.to_vec()),
            payload,
        }
    }
}

/// Batch upsert payload.
/// See: https://qdrant.tech/documentation/concepts/operations/#upsert
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QdrantUpsert {
    pub points: Vec<QdrantPoint>,
}

impl QdrantUpsert {
    pub fn new(points: Vec<QdrantPoint>) -> Self {
        Self { points }
    }
}
