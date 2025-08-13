use anyhow::{Result, anyhow};
use qdrant_client::{
    Payload, Qdrant,
    qdrant::{
        CreateCollectionBuilder, Distance, PointStruct, ScoredPoint, SearchPointsBuilder,
        UpsertPointsBuilder, VectorParamsBuilder,
    },
};

/// Thin wrapper around the official Qdrant Rust client (crate `qdrant-client = "1.15"`).
/// Uses only stable, existing APIs:
/// - `Qdrant::from_url(...).build()`
/// - `collection_exists`, `create_collection`, `delete_collection`
/// - `upsert_points` (with `.wait(true)`) and `SearchPointsBuilder`
pub struct QdrantStore {
    pub client: Qdrant,
    pub url: String,
}

impl QdrantStore {
    /// Build a client for a given base URL, e.g. "http://localhost:6333".
    /// For Qdrant Cloud add `.api_key(...)` before `.build()`.
    pub fn new(url: &str) -> Result<Self> {
        let client = Qdrant::from_url(url).build()?;
        Ok(Self {
            client,
            url: url.to_string(),
        })
    }

    /// Ensure the collection exists. If it doesn't, create it with single-vector config.
    /// `dim` is the vector dimension; `distance` ∈ {"Cosine","Dot","Euclid"} (case-insensitive).
    /// Note: Quantization config is optional and omitted here to keep the API strictly-valid across versions.
    pub async fn ensure_collection(
        &self,
        collection: &str,
        dim: u64,
        distance: &str,
        on_disk: bool,
    ) -> Result<()> {
        if self.client.collection_exists(collection).await? {
            return Ok(());
        }

        let dist = parse_distance(distance)?;
        let vectors_cfg = VectorParamsBuilder::new(dim, dist).on_disk(on_disk);

        // Create the collection with the vector params; no quantization by default.
        self.client
            .create_collection(CreateCollectionBuilder::new(collection).vectors_config(vectors_cfg))
            .await?;
        Ok(())
    }

    /// Convenience: drop & recreate the collection (useful for repeatable tests).
    pub async fn recreate_collection(
        &self,
        collection: &str,
        dim: u64,
        distance: &str,
        on_disk: bool,
    ) -> Result<()> {
        if self.client.collection_exists(collection).await? {
            self.client.delete_collection(collection).await?;
        }
        self.ensure_collection(collection, dim, distance, on_disk)
            .await
    }

    /// Upsert a batch of points with string IDs, vectors and JSON payloads.
    /// Uses `.wait(true)` to make the call synchronous/durable (CI-friendly).
    pub async fn upsert_points_wait(
        &self,
        collection: &str,
        ids: Vec<String>,
        vectors: Vec<Vec<f32>>,
        payloads: Vec<Payload>,
    ) -> Result<()> {
        if ids.len() != vectors.len() || ids.len() != payloads.len() {
            return Err(anyhow!(
                "length mismatch: ids={}, vectors={}, payloads={}",
                ids.len(),
                vectors.len(),
                payloads.len()
            ));
        }

        let points: Vec<PointStruct> = ids
            .into_iter()
            .zip(vectors.into_iter())
            .zip(payloads.into_iter())
            .map(|((id, vec), payload)| PointStruct::new(id, vec, payload))
            .collect();

        // `upsert_points` + `.wait(true)` replaces the removed `upsert_points_blocking`.
        self.client
            .upsert_points(UpsertPointsBuilder::new(collection, points).wait(true))
            .await?;
        Ok(())
    }

    /// Semantic search for top-k neighbors.
    /// If `score_threshold` is `Some(t)`, points with score < t are filtered out.
    /// `with_payload` toggles returning payloads in results.
    pub async fn search(
        &self,
        collection: &str,
        query_vector: Vec<f32>,
        top_k: u64,
        score_threshold: Option<f32>,
        with_payload: bool,
    ) -> Result<Vec<ScoredPoint>> {
        let mut builder =
            SearchPointsBuilder::new(collection, query_vector, top_k).with_vectors(false);
        if with_payload {
            builder = builder.with_payload(true);
        }
        if let Some(t) = score_threshold {
            builder = builder.score_threshold(t);
        }

        let resp = self.client.search_points(builder).await?;
        Ok(resp.result)
    }
}

/// Map string distance to Qdrant `Distance`.
fn parse_distance(s: &str) -> Result<Distance> {
    match s.to_ascii_lowercase().as_str() {
        "cosine" => Ok(Distance::Cosine),
        "dot" | "dotproduct" => Ok(Distance::Dot),
        "euclid" | "euclidean" | "l2" => Ok(Distance::Euclid),
        other => Err(anyhow!("unknown distance: {}", other)),
    }
}
