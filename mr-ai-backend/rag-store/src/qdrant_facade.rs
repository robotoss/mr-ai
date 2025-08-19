//! Thin adapter around `qdrant-client` to isolate API usage.
//!
//! This facade concentrates all Qdrant interactions behind a minimal API,
//! hiding away the verbose builder pattern and keeping the rest of the
//! application decoupled from `qdrant-client`.

use crate::config::{DistanceKind, RagConfig, VectorSpace};
use crate::errors::RagError;

use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, Filter, PointStruct, SearchParamsBuilder,
    SearchPointsBuilder, UpsertPointsBuilder, Value as QValue, VectorParamsBuilder,
};
use tracing::{debug, info, warn};

/// A facade over the Qdrant client to keep the rest of the code clean and stable.
///
/// This struct encapsulates:
/// - The underlying Qdrant client.
/// - The target collection name.
/// - The distance function used in the vector space.
pub struct QdrantFacade {
    pub(crate) client: Qdrant,
    pub(crate) collection: String,
    distance: DistanceKind,
}

impl QdrantFacade {
    /// Creates a new facade from the given configuration.
    ///
    /// Uses the modern builder-based API of `qdrant-client` and supports
    /// optional API key authentication.
    pub fn new(cfg: &RagConfig) -> Result<Self, RagError> {
        cfg.validate()?; // Early validation of config.

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

    /// Ensures that the collection exists in Qdrant.
    ///
    /// - If the collection already exists → no-op.
    /// - If missing → creates it with the given vector space configuration.
    pub async fn ensure_collection(&self, space: &VectorSpace) -> Result<(), RagError> {
        info!(
            "Ensuring collection '{}' with size={} distance={:?}",
            self.collection, space.size, self.distance
        );

        // Try to fetch collection info first.
        match self.client.collection_info(&self.collection).await {
            Ok(_) => {
                debug!("Collection '{}' already exists", self.collection);
                return Ok(());
            }
            Err(err) => {
                warn!(
                    "Collection '{}' not found, will be created (error={})",
                    self.collection, err
                );
            }
        }

        let distance = match self.distance {
            DistanceKind::Cosine => Distance::Cosine,
            DistanceKind::Dot => Distance::Dot,
            DistanceKind::Euclid => Distance::Euclid,
        };

        // Create collection with vector configuration.
        self.client
            .create_collection(
                CreateCollectionBuilder::new(&self.collection)
                    .vectors_config(VectorParamsBuilder::new(space.size as u64, distance)),
            )
            .await
            .map_err(|e| RagError::Qdrant(e.to_string()))?;

        info!("Collection '{}' created successfully", self.collection);
        Ok(())
    }

    /// Upserts (inserts or updates) a batch of points into the collection.
    ///
    /// Returns the number of points acknowledged by Qdrant.
    pub async fn upsert_points(&self, points: Vec<PointStruct>) -> Result<u64, RagError> {
        if points.is_empty() {
            debug!("No points provided for upsert");
            return Ok(0);
        }

        info!(
            "Upserting {} points into collection '{}'",
            points.len(),
            self.collection
        );

        let res = self
            .client
            .upsert_points(UpsertPointsBuilder::new(&self.collection, points))
            .await
            .map_err(|e| RagError::Qdrant(e.to_string()))?;

        debug!("Upsert operation result={:?}", res.result);

        Ok(res.result.and_then(|r| r.operation_id).unwrap_or(0))
    }

    /// Performs a similarity search in Qdrant.
    ///
    /// Returns `(score, payload)` tuples with results sorted by score.
    pub async fn search(
        &self,
        vector: Vec<f32>,
        top_k: u64,
        filter: Option<Filter>,
        with_payload: bool,
        exact: bool,
    ) -> Result<Vec<(f32, serde_json::Value)>, RagError> {
        info!(
            "Searching in '{}' with top_k={}, with_payload={}, exact={}",
            self.collection, top_k, with_payload, exact
        );

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

        // Convert raw Qdrant payloads into JSON.
        let mut out = Vec::with_capacity(res.result.len());
        for r in res.result.into_iter() {
            let score = r.score;
            let payload_json = qpayload_to_json(r.payload);
            out.push((score, payload_json));
        }

        debug!("Search completed: {} hits returned", out.len());
        Ok(out)
    }
}

/// Converts a Qdrant payload (`HashMap<String, qdrant::Value>`) into JSON.
///
/// Unsupported nested objects/arrays are mapped to `Null`.
fn qpayload_to_json(mut p: std::collections::HashMap<String, QValue>) -> serde_json::Value {
    use qdrant_client::qdrant::value::Kind as K;
    let mut m = serde_json::Map::new();
    for (k, v) in p.drain() {
        let j = match v.kind {
            Some(K::StringValue(s)) => serde_json::Value::String(s),
            Some(K::IntegerValue(i)) => serde_json::Value::Number(i.into()),
            Some(K::DoubleValue(f)) => serde_json::json!(f),
            Some(K::BoolValue(b)) => serde_json::Value::Bool(b),
            None => serde_json::Value::Null,
            // For unsupported nested types, fallback to Null for safety.
            _ => serde_json::Value::Null,
        };
        m.insert(k, j);
    }
    serde_json::Value::Object(m)
}
