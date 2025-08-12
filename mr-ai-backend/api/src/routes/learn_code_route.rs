use std::env;

use graph_prepare::parce;
use vector_lib::{ask::ask_question, ollama::OllamaEmb, qdrant::QdrantStore};

pub async fn learn_code() -> &'static str {
    let project_name = env::var("PROJECT_NAME").expect("PROJECT_NAME must be set in environment");

    let result = parce::parse_and_save_language_aware(&format!("code_data/{}", project_name));

    match result {
        Ok(_) => {}
        Err(ex) => println!("Failed learn {}", ex),
    }

    // Read config from env
    let qdrant_url = env::var("QDRANT_URL").unwrap();
    let collection = env::var("QDRANT_COLLECTION").unwrap();
    let ollama_url = env::var("OLLAMA_URL").unwrap();
    let emb_model = env::var("EMBEDDING_MODEL").unwrap();
    let conc: usize = env::var("EMBEDDING_CONCURRENCY")
        .unwrap_or_else(|_| "4".into())
        .parse()
        .unwrap_or(4);

    // Build clients
    let q = QdrantStore::new(&qdrant_url).unwrap();
    let emb = OllamaEmb::new(ollama_url, emb_model, conc);

    // Make sure collection exists (dim must match your model, e.g., 1536 for Qwen3-Embedding-0.6B)
    let dim: u64 = env::var("EMBEDDING_DIM")
        .unwrap_or_else(|_| "1536".into())
        .parse()
        .unwrap_or(1536);
    match q.ensure_collection(&collection, dim, "Cosine", false).await {
        Ok(_) => {}
        Err(err) => println!("Ensure Collection Error: {}", err),
    }

    // Natural language question
    let question = "Where is the RAWG API key configured in the project.unwrap()";
    let hits = match ask_question(&q, &collection, &emb, question, 5).await {
        Ok(hits) => hits,
        Err(err) => {
            println!("Ensure Collection Error: {}", err);
            return "Hello, World!";
        }
    };

    if hits.is_empty() {
        println!("No matches.");
        return "Hello, World!";
    }

    let best = &hits[0];
    println!("Top match (score={}):", best.score);
    if let Some(file) = &best.file {
        println!("File: {}", file);
    }
    if let (Some(s), Some(e)) = (best.start_line, best.end_line) {
        println!("Lines: {}-{}", s, e);
    }
    println!("Snippet:\n{}\n", best.text);

    "Hello, World!"
}
