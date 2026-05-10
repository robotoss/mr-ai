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
    Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, Distance, FieldType,
    Filter, PointStruct, SearchPointsBuilder, UpsertPointsBuilder, VectorParamsBuilder,
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

/// S1: delete_by_filter / delete_by_repo / scroll_repo_chunk_metas round-trip
/// covering the incremental-dedup pipeline that S2 wires into the worker.
///
/// Strategy: create a tiny collection with `repo_id` payload index, upsert
/// three points across two repos, exercise scroll-by-repo + delete-by-repo
/// + delete-by-file via the public helpers, and assert the remaining set.
#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn qdrant_delete_and_scroll_helpers() {
    use rag_base::structs::rag_base_config::{
        ChunkClampConfig, DistanceMetric, EmbeddingConfig, QdrantConfig, RagConfig, SearchConfig,
    };

    let image = GenericImage::new("qdrant/qdrant", "v1.14.0")
        .with_exposed_port(6334.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Qdrant gRPC listening"));
    let container = image.start().await.expect("start qdrant container");
    let port = container.get_host_port_ipv4(6334).await.unwrap();
    let url = format!("http://127.0.0.1:{port}");

    let collection = "s1_dedup".to_owned();
    let dim = 4u64;
    let client = Qdrant::from_url(&url).build().expect("qdrant client builds");

    client
        .create_collection(
            CreateCollectionBuilder::new(&collection)
                .vectors_config(VectorParamsBuilder::new(dim, Distance::Cosine)),
        )
        .await
        .expect("create_collection");
    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(&collection, "repo_id", FieldType::Keyword),
        )
        .await
        .expect("repo_id index");
    client
        .create_field_index(
            CreateFieldIndexCollectionBuilder::new(&collection, "file", FieldType::Keyword),
        )
        .await
        .expect("file index");

    let cfg = RagConfig {
        project_name: "test".into(),
        code_jsonl: std::path::PathBuf::from("/tmp/unused.jsonl"),
        qdrant: QdrantConfig {
            url: url.clone(),
            collection: collection.clone(),
            distance: DistanceMetric::Cosine,
            batch_size: 64,
        },
        embedding: EmbeddingConfig { dim: dim as usize },
        search: SearchConfig::default(),
        clamp: ChunkClampConfig::default(),
    };

    let make_point = |id: u64, repo: &str, file: &str, sha: &str| -> PointStruct {
        let payload: std::collections::HashMap<String, qdrant_client::qdrant::Value> = [
            ("id".into(), qdrant_client::qdrant::Value::from(format!("chunk-{id}"))),
            ("repo_id".into(), qdrant_client::qdrant::Value::from(repo)),
            ("file".into(), qdrant_client::qdrant::Value::from(file)),
            ("content_sha256".into(), qdrant_client::qdrant::Value::from(sha)),
        ]
        .into_iter()
        .collect();
        PointStruct::new(id, vec![1.0, 0.0, 0.0, 0.0], qdrant_client::Payload::from(payload))
    };

    client
        .upsert_points(
            UpsertPointsBuilder::new(
                &collection,
                vec![
                    make_point(1, "repo-A", "lib/a.dart", "sha1"),
                    make_point(2, "repo-A", "lib/b.dart", "sha2"),
                    make_point(3, "repo-B", "lib/c.dart", "sha3"),
                ],
            )
            .wait(true),
        )
        .await
        .expect("upsert_points");

    let metas_a = rag_base::vector_db::scroll_repo_chunk_metas(&client, &cfg, "repo-A", 100)
        .await
        .expect("scroll repo-A");
    assert_eq!(metas_a.len(), 2);
    assert!(metas_a.iter().any(|m| m.file == "lib/a.dart" && m.content_sha256 == "sha1"));
    assert!(metas_a.iter().any(|m| m.file == "lib/b.dart" && m.content_sha256 == "sha2"));

    rag_base::vector_db::delete_by_file(&client, &cfg, "repo-A", "lib/a.dart")
        .await
        .expect("delete_by_file");
    let metas_a_after = rag_base::vector_db::scroll_repo_chunk_metas(&client, &cfg, "repo-A", 100)
        .await
        .expect("scroll repo-A after delete_by_file");
    assert_eq!(metas_a_after.len(), 1);
    assert_eq!(metas_a_after[0].file, "lib/b.dart");

    rag_base::vector_db::delete_by_repo(&client, &cfg, "repo-A")
        .await
        .expect("delete_by_repo");
    let metas_a_final =
        rag_base::vector_db::scroll_repo_chunk_metas(&client, &cfg, "repo-A", 100)
            .await
            .expect("scroll repo-A after delete_by_repo");
    assert!(metas_a_final.is_empty());

    // repo-B should remain untouched.
    let metas_b = rag_base::vector_db::scroll_repo_chunk_metas(&client, &cfg, "repo-B", 100)
        .await
        .expect("scroll repo-B");
    assert_eq!(metas_b.len(), 1);
    assert_eq!(metas_b[0].file, "lib/c.dart");

    // delete_by_filter with a custom Filter.
    let filter = Filter::must([Condition::matches("repo_id", "repo-B".to_owned())]);
    rag_base::vector_db::delete_by_filter(&client, &cfg, filter)
        .await
        .expect("delete_by_filter");
    let metas_b_final =
        rag_base::vector_db::scroll_repo_chunk_metas(&client, &cfg, "repo-B", 100)
            .await
            .expect("scroll repo-B final");
    assert!(metas_b_final.is_empty());
}
