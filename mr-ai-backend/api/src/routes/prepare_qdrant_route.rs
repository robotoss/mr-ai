use std::sync::Arc;

use axum::extract::State;
use rag_store::{OllamaConfig, OllamaEmbedder, RagConfig, RagStore};

use crate::core::app_state::AppState;

pub async fn prepare_qdrant(State(state): State<Arc<AppState>>) -> &'static str {
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
        svc: state.llm_profiles.clone(),
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
