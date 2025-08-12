// qdrant.rs
use anyhow::{Result, anyhow};
use qdrant_client::{
    Payload, Qdrant,
    qdrant::{
        CreateCollectionBuilder, Distance, PointStruct, ScoredPoint, SearchPointsBuilder,
        VectorParamsBuilder,
    },
};

pub struct QdrantStore {
    pub client: Qdrant,
    pub url: String,
}

impl QdrantStore {
    /// Build Qdrant client. Prefer gRPC port 6334 for the Rust client.
    pub fn new(url: &str) -> Result<Self> {
        // e.g. "http://localhost:6334"
        let client = Qdrant::from_url(url).build()?;
        Ok(Self {
            client,
            url: url.to_string(),
        })
    }

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
        self.client
            .create_collection(CreateCollectionBuilder::new(collection).vectors_config(vectors_cfg))
            .await?;
        Ok(())
    }

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

    /// Upsert a batch of points and wait for persistence.
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

        self.client
            .upsert_points(
                qdrant_client::qdrant::UpsertPointsBuilder::new(collection, points).wait(true),
            )
            .await?;
        Ok(())
    }

    /// KNN search with optional score threshold and payload switch.
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
        // Use a sane default threshold if caller passed Some(1.0) which is usually too strict.
        if let Some(t) = score_threshold {
            let t = if (t - 1.0).abs() < f32::EPSILON {
                0.2
            } else {
                t
            };
            builder = builder.score_threshold(t);
        }

        let resp = self.client.search_points(builder).await?;
        Ok(resp.result)
    }
}

fn parse_distance(s: &str) -> Result<Distance> {
    match s.to_ascii_lowercase().as_str() {
        "cosine" => Ok(Distance::Cosine),
        "dot" | "dotproduct" => Ok(Distance::Dot),
        "euclid" | "euclidean" | "l2" => Ok(Distance::Euclid),
        other => Err(anyhow!("unknown distance: {}", other)),
    }
}
