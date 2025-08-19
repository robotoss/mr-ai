use rag_store::{EmbeddingPolicy, OllamaConfig, OllamaEmbedder, RagConfig, RagStore};

pub async fn prepare_qdrant() -> &'static str {
    // 1) Configure store
    let cfg = RagConfig::from_env();

    let cfg = match cfg {
        Ok(cfg) => cfg,
        Err(ex) => {
            println!("FAILED cfg: {}", ex);
            return "Hello, World!";
        }
    };

    let store = RagStore::new(cfg);

    let store = match store {
        Ok(store) => store,
        Err(ex) => {
            println!("FAILED store: {}", ex);
            return "Hello, World!";
        }
    };

    let ollama = OllamaEmbedder::new(OllamaConfig {
        url: std::env::var("OLLAMA_URL").unwrap(),
        model: std::env::var("EMBEDDING_MODEL").unwrap(),
        dim: std::env::var("EMBEDDING_DIM").unwrap().parse().unwrap(),
    });

    // 2) Ingest only `rag_records.jsonl` from the latest timestamp directory
    //    under: code_data/project_x/graphs_data/<YYYYMMDD_HHMMSS>/rag_records.jsonl
    let count = store.ingest_latest_all_embedded("code_data", &ollama).await;

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
