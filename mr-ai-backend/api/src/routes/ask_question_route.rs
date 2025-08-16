use rag_store::{EmbeddingPolicy, OllamaConfig, OllamaEmbedder, RagConfig, RagQuery, RagStore};

pub async fn ask_question() -> &'static str {
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

    let count = store
        .ingest_latest_from("code_data", EmbeddingPolicy::PrecomputedOr(&ollama))
        .await;

    let count = match count {
        Ok(count) => count,
        Err(ex) => {
            println!("FAILED count: {}", ex);
            return "Hello, World!";
        }
    };

    println!("Ingested points: {count}");

    let query = RagQuery {
        text: "Where change gamesIcon",
        top_k: 5,
        filter: None,
    };

    let hits = match store.rag_context(query, &ollama).await {
        Ok(hits) => hits,
        Err(err) => {
            println!("Ensure Collection Error: {}", err);
            return "Hello, World!";
        }
    };

    for hit in hits {
        println!(
            "- score={:.3}, source={:?}, text={}",
            hit.score, hit.source, hit.text
        );
    }

    "Hello world"
}
