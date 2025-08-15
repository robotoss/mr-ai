use rag_store::{DistanceKind, EmbeddingPolicy, EmbeddingsProvider, RagConfig, RagStore};

pub async fn prepare_qdrant() -> &'static str {
    // 1) Configure store
    let cfg = RagConfig {
        qdrant_url: "http://localhost:6334".to_string(),
        qdrant_api_key: None,
        collection: "project_x_latest".to_string(),
        distance: DistanceKind::Cosine,
        upsert_batch: 256,
        exact_search: false,
    };
    let store = RagStore::new(cfg);

    let store = match store {
        Ok(store) => store,
        Err(ex) => {
            println!("FAILED store: {}", ex);
            return "Hello, World!";
        }
    };

    // 2) Ingest only `rag_records.jsonl` from the latest timestamp directory
    //    under: code_data/project_x/graphs_data/<YYYYMMDD_HHMMSS>/rag_records.jsonl
    let count = store
        .ingest_latest_from("code_data", EmbeddingPolicy::PrecomputedOr(&NoopEmbedder))
        .await;

    let count = match count {
        Ok(count) => count,
        Err(ex) => {
            println!("FAILED count: {}", ex);
            return "Hello, World!";
        }
    };

    println!("Ingested points: {count}");

    "Hello, World!"
}

struct NoopEmbedder;
impl EmbeddingsProvider for NoopEmbedder {
    fn embed(&self, _text: &str) -> Result<Vec<f32>, rag_store::RagError> {
        // This should never be called if rag_records.jsonl has precomputed embeddings.
        Err(rag_store::RagError::MissingEmbedding)
    }
}
