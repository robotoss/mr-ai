//! Thin adapter around `qdrant-client` to isolate API usage.
//!
//! This facade concentrates all Qdrant interactions behind a small API,
//! using the modern builder-based client (`qdrant_client::Qdrant`).

use crate::config::{DistanceKind, RagConfig, VectorSpace};
use crate::errors::RagError;

use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, Filter, PointStruct, SearchParamsBuilder,
    SearchPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
};
use tracing::trace;

/// A minimal facade over the Qdrant client to keep the rest of the code decoupled.
pub struct QdrantFacade {
    client: Qdrant,
    collection: String,
    distance: DistanceKind,
}

impl QdrantFacade {
    /// Creates a new facade from the given configuration.
    ///
    /// Uses the new builder API:
    /// `Qdrant::from_url(cfg.qdrant_url).api_key(...).build()?`
    ///
    /// # Errors
    /// Returns `RagError::Config` for invalid cfg or wraps client init failures as `RagError::Qdrant`.
    pub fn new(cfg: &RagConfig) -> Result<Self, RagError> {
        cfg.validate()?;

        // Build client via modern API (no deprecated configs).
        let mut builder = Qdrant::from_url(&cfg.qdrant_url);
        if let Some(key) = &cfg.qdrant_api_key {
            builder = builder.api_key(key.clone());
        }
        let client = builder
            .build()
            .map_err(|e| RagError::Qdrant(e.to_string()))?;

        Ok(Self {
            client,
            collection: cfg.collection.clone(),
            distance: cfg.distance,
        })
    }

    /// Ensures the collection exists with the provided vector space.
    ///
    /// If the collection is absent, it will be created using the selected distance function.
    pub async fn ensure_collection(&self, space: &VectorSpace) -> Result<(), RagError> {
        trace!(
            "qdrant_facade::ensure_collection name={} size={} distance={:?}",
            self.collection, space.size, self.distance
        );

        // Check existence via collection_info; if it exists - return Ok.
        match self.client.collection_info(&self.collection).await {
            Ok(_) => {
                trace!("qdrant_facade::ensure_collection already exists");
                return Ok(());
            }
            Err(err) => {
                // Proceed to create; log the original error (likely NotFound).
                trace!("qdrant_facade::collection_info miss: {}", err);
            }
        }

        let distance = match self.distance {
            DistanceKind::Cosine => Distance::Cosine,
            DistanceKind::Dot => Distance::Dot,
            DistanceKind::Euclid => Distance::Euclid,
        };

        // Create collection using builders.
        self.client
            .create_collection(
                CreateCollectionBuilder::new(&self.collection)
                    .vectors_config(VectorParamsBuilder::new(space.size as u64, distance)),
            )
            .await
            .map_err(|e| RagError::Qdrant(e.to_string()))?;

        trace!("qdrant_facade::ensure_collection created");
        Ok(())
    }

    /// Upserts a batch of points into the collection. Returns number of points upserted.
    ///
    /// Uses `UpsertPointsBuilder` and waits for completion.
    ///
    /// # Errors
    /// Wraps client errors as `RagError::Qdrant`.
    pub async fn upsert_points(&self, points: Vec<PointStruct>) -> Result<usize, RagError> {
        trace!("qdrant_facade::upsert_points count={}", points.len());
        if points.is_empty() {
            return Ok(0);
        }

        let res = self
            .client
            .upsert_points(UpsertPointsBuilder::new(&self.collection, points))
            .await
            .map_err(|e| RagError::Qdrant(e.to_string()))?;

        // The Upsert result may not always contain an operation id; return 0 then.
        trace!("qdrant_facade::upsert_points status={:?}", res.result);
        Ok(res
            .result
            .map(|r| r.operation_id.unwrap() as usize)
            .unwrap_or(0))
    }

    /// Performs a similarity search and returns `(score, payload)` tuples.
    ///
    /// Uses `SearchPointsBuilder`, optional `Filter`, and `SearchParamsBuilder` for `exact`.
    ///
    /// # Errors
    /// Wraps client errors as `RagError::Qdrant`.
    pub async fn search(
        &self,
        vector: Vec<f32>,
        top_k: u64,
        filter: Option<Filter>,
        with_payload: bool,
        exact: bool,
    ) -> Result<Vec<(f32, serde_json::Value)>, RagError> {
        trace!("qdrant_facade::search top_k={top_k} with_payload={with_payload} exact={exact}");

        let mut builder =
            SearchPointsBuilder::new(&self.collection, vector, top_k).with_payload(with_payload);

        if let Some(f) = filter {
            builder = builder.filter(f);
        }
        if exact {
            builder = builder.params(SearchParamsBuilder::default().exact(true));
        }

        let res = self
            .client
            .search_points(builder)
            .await
            .map_err(|e| RagError::Qdrant(e.to_string()))?;

        let mut out = Vec::with_capacity(res.result.len());
        for r in res.result.into_iter() {
            let score = r.score;
            // `payload` here is `HashMap<String, qdrant::Value>`; we convert it to JSON
            // since the rest of our code expects `serde_json::Value`.
            let payload = r
                .payload
                .into_iter()
                .map(|(k, v)| (k, v.into_json()))
                .collect::<serde_json::Map<_, _>>();
            out.push((score, serde_json::Value::Object(payload)));
        }
        trace!("qdrant_facade::search hits={}", out.len());
        Ok(out)
    }
}
