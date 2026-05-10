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

use ai_llm_service::test_support::dummy_gateway;
use code_indexer::types::{ChunkFeatures, Span, SymbolKind};
use code_indexer::{CodeChunk, LanguageKind};
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

/// S2: content-sha incremental dedup pipeline.
///
/// `upsert_repo_chunks` is the worker's post-graph_persist entry point.
/// It must:
///   1) embed + upsert every chunk on a cold collection;
///   2) skip embedding and upsert entirely when the same chunks come in
///      a second time (content_sha matches);
///   3) on a content change, re-embed/upsert exactly the changed chunk
///      and leave the rest in place;
///   4) on a removed chunk, delete the orphan without re-embedding.
///
/// We drive the pipeline with the dummy gateway (8-dim zero vectors) so
/// the assertions focus on diff bookkeeping, not embedding fidelity.
#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn upsert_repo_chunks_dedup_pipeline() {
    use rag_base::structs::rag_base_config::{
        ChunkClampConfig, DistanceMetric, EmbeddingConfig, QdrantConfig, RagConfig, SearchConfig,
    };

    let image = GenericImage::new("qdrant/qdrant", "v1.14.0")
        .with_exposed_port(6334.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Qdrant gRPC listening"));
    let container = image.start().await.expect("start qdrant container");
    let port = container.get_host_port_ipv4(6334).await.unwrap();
    let url = format!("http://127.0.0.1:{port}");

    let dim = 8usize;
    let collection = "s2_dedup".to_owned();
    let cfg = RagConfig {
        project_name: "test".into(),
        code_jsonl: std::path::PathBuf::from("/tmp/unused.jsonl"),
        qdrant: QdrantConfig {
            url: url.clone(),
            collection: collection.clone(),
            distance: DistanceMetric::Cosine,
            batch_size: 4,
        },
        embedding: EmbeddingConfig { dim },
        search: SearchConfig::default(),
        clamp: ChunkClampConfig::default(),
    };

    let client = Qdrant::from_url(&url).build().expect("qdrant client builds");
    rag_base::vector_db::reset_collection(&client, &cfg)
        .await
        .expect("reset_collection");

    let gateway = dummy_gateway();
    let repo_id = uuid::Uuid::new_v4().simple().to_string();
    let project_id = uuid::Uuid::new_v4().simple().to_string();

    let chunk_a_v1 = make_chunk("lib/a.dart", "lib/a.dart::A::foo", "sha-a-v1");
    let chunk_b = make_chunk("lib/b.dart", "lib/b.dart::B::bar", "sha-b-v1");

    // Pass 1: cold collection → upsert everything.
    let report = rag_base::upsert_repo_chunks(
        &client,
        &cfg,
        &gateway,
        &repo_id,
        Some(&project_id),
        &[chunk_a_v1.clone(), chunk_b.clone()],
    )
    .await
    .expect("upsert_repo_chunks v1");
    assert_eq!(report.upserted, 2, "v1 upserted: {report:?}");
    assert_eq!(report.embedded, 2, "v1 embedded: {report:?}");
    assert_eq!(report.kept, 0, "v1 kept: {report:?}");
    assert_eq!(report.deleted, 0, "v1 deleted: {report:?}");
    let metas = rag_base::vector_db::scroll_repo_chunk_metas(&client, &cfg, &repo_id, 100)
        .await
        .expect("scroll after v1");
    assert_eq!(metas.len(), 2);

    // Pass 2: identical input → all kept, nothing embedded or upserted.
    let report = rag_base::upsert_repo_chunks(
        &client,
        &cfg,
        &gateway,
        &repo_id,
        Some(&project_id),
        &[chunk_a_v1.clone(), chunk_b.clone()],
    )
    .await
    .expect("upsert_repo_chunks v2");
    assert_eq!(report.upserted, 0, "v2 upserted: {report:?}");
    assert_eq!(report.embedded, 0, "v2 embedded: {report:?}");
    assert_eq!(report.kept, 2, "v2 kept: {report:?}");
    assert_eq!(report.deleted, 0, "v2 deleted: {report:?}");

    // Pass 3: a.dart changes (new sha) → exactly the changed chunk is
    // re-embedded; the old version is deleted by id.
    let chunk_a_v2 = make_chunk("lib/a.dart", "lib/a.dart::A::foo", "sha-a-v2");
    let report = rag_base::upsert_repo_chunks(
        &client,
        &cfg,
        &gateway,
        &repo_id,
        Some(&project_id),
        &[chunk_a_v2.clone(), chunk_b.clone()],
    )
    .await
    .expect("upsert_repo_chunks v3");
    assert_eq!(report.upserted, 1, "v3 upserted: {report:?}");
    assert_eq!(report.embedded, 1, "v3 embedded: {report:?}");
    assert_eq!(report.kept, 1, "v3 kept: {report:?}");
    assert_eq!(report.deleted, 1, "v3 deleted: {report:?}");
    let metas = rag_base::vector_db::scroll_repo_chunk_metas(&client, &cfg, &repo_id, 100)
        .await
        .expect("scroll after v3");
    assert_eq!(metas.len(), 2);
    assert!(metas
        .iter()
        .any(|m| m.file == "lib/a.dart" && m.content_sha256 == "sha-a-v2"));

    // Pass 4: drop a.dart entirely → the orphan is deleted, no work on b.dart.
    let report = rag_base::upsert_repo_chunks(
        &client,
        &cfg,
        &gateway,
        &repo_id,
        Some(&project_id),
        &[chunk_b.clone()],
    )
    .await
    .expect("upsert_repo_chunks v4");
    assert_eq!(report.upserted, 0, "v4 upserted: {report:?}");
    assert_eq!(report.embedded, 0, "v4 embedded: {report:?}");
    assert_eq!(report.kept, 1, "v4 kept: {report:?}");
    assert_eq!(report.deleted, 1, "v4 deleted: {report:?}");
    let metas = rag_base::vector_db::scroll_repo_chunk_metas(&client, &cfg, &repo_id, 100)
        .await
        .expect("scroll after v4");
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].file, "lib/b.dart");
}

fn make_chunk(file: &str, symbol_path: &str, sha: &str) -> CodeChunk {
    CodeChunk {
        id: format!("legacy-{file}-{symbol_path}-{sha}"),
        language: LanguageKind::Dart,
        file: file.to_owned(),
        symbol: symbol_path.rsplit("::").next().unwrap_or(symbol_path).to_owned(),
        symbol_path: symbol_path.to_owned(),
        kind: SymbolKind::Method,
        span: Span {
            start_byte: 0,
            end_byte: 1,
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 1,
        },
        owner_path: Vec::new(),
        doc: None,
        annotations: Vec::new(),
        imports: Vec::new(),
        signature: Some(format!("fn {symbol_path}()")),
        is_definition: true,
        is_generated: false,
        snippet: Some(format!("// chunk for {symbol_path}")),
        features: ChunkFeatures::default(),
        content_sha256: sha.to_owned(),
        neighbors: None,
        identifiers: Vec::new(),
        anchors: Vec::new(),
        graph: None,
        hints: None,
        lsp: None,
        extras: None,
        parent_symbol_id: None,
        chunk_kind: None,
    }
}
