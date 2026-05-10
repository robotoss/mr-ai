//! Qdrant round-trip integration test.
//!
//! Boots a real Qdrant via testcontainers, creates a tiny collection
//! through `qdrant-client`, upserts a single point, and queries it back.
//! Substitutes for the absent unit-level coverage of the vector store.
//!
//! Marked `#[ignore]` so the default Docker-free `cargo test` path stays
//! green. Run with `cargo test --workspace --tests -- --ignored` against
//! a Docker daemon.

#![allow(clippy::needless_return)]

use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, Distance, PointStruct, SearchPointsBuilder,
    UpsertPointsBuilder, VectorParamsBuilder,
};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::GenericImage;

const COLLECTION: &str = "smoke";
const DIM: u64 = 4;

#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn qdrant_create_upsert_search_round_trip() {
    let image = GenericImage::new("qdrant/qdrant", "v1.14.0")
        .with_exposed_port(6334.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Qdrant gRPC listening"));
    let container = image.start().await.expect("start qdrant container");
    let port = container.get_host_port_ipv4(6334).await.unwrap();
    let url = format!("http://127.0.0.1:{port}");

    let client = Qdrant::from_url(&url)
        .build()
        .expect("qdrant client builds");

    client
        .create_collection(
            CreateCollectionBuilder::new(COLLECTION)
                .vectors_config(VectorParamsBuilder::new(DIM, Distance::Cosine)),
        )
        .await
        .expect("create_collection");

    let payload: std::collections::HashMap<String, qdrant_client::qdrant::Value> =
        [(
            "label".to_string(),
            qdrant_client::qdrant::Value::from("hello"),
        )]
        .into_iter()
        .collect();
    let point = PointStruct::new(
        1u64,
        vec![1.0f32, 0.0, 0.0, 0.0],
        qdrant_client::Payload::from(payload),
    );
    client
        .upsert_points(UpsertPointsBuilder::new(COLLECTION, vec![point]).wait(true))
        .await
        .expect("upsert_points");

    let response = client
        .search_points(
            SearchPointsBuilder::new(COLLECTION, vec![1.0f32, 0.0, 0.0, 0.0], 1u64)
                .with_payload(true),
        )
        .await
        .expect("search_points");
    assert_eq!(response.result.len(), 1);
    let hit = &response.result[0];
    assert!(hit.score > 0.99);
    assert_eq!(
        hit.payload.get("label").and_then(|v| v.as_str()).map(|s| s.to_owned()),
        Some("hello".to_owned())
    );
}
