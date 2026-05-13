//! Wiremock-backed unit tests for [`OllamaProvider`].

use ai_llm_service::providers::OllamaProvider;
use ai_llm_service::{
    EmbeddingProvider, EmbeddingRequest, GatewayError, LlmProvider, ProviderConfig, ProviderKind,
    UnifiedRequest,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(endpoint: &str, model: &str) -> ProviderConfig {
    ProviderConfig {
        provider: ProviderKind::Ollama,
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        api_key: None,
        max_tokens: Some(64),
        temperature: Some(0.2),
        top_p: None,
        timeout_secs: Some(5),
        ..Default::default()
    }
}

#[tokio::test]
async fn complete_parses_message_and_token_usage() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "llama3",
            "message": {"role": "assistant", "content": "Hello!"},
            "done": true,
            "prompt_eval_count": 12,
            "eval_count": 7
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(cfg(&server.uri(), "llama3")).unwrap();
    let req = UnifiedRequest::user_only("hi");
    let resp = provider.complete(req).await.unwrap();

    assert_eq!(resp.content, "Hello!");
    assert_eq!(resp.usage.prompt, 12);
    assert_eq!(resp.usage.completion, 7);
    assert_eq!(resp.usage.total, 19);
    assert_eq!(resp.provider, ProviderKind::Ollama);
    assert_eq!(resp.model, "llama3");
}

#[tokio::test]
async fn complete_propagates_4xx_with_snippet() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad request: missing model"))
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(cfg(&server.uri(), "llama3")).unwrap();
    let err = provider.complete(UnifiedRequest::user_only("hi")).await.unwrap_err();
    let s = err.to_string();
    assert!(s.contains("400"), "{s}");
    assert!(s.contains("bad request"), "{s}");
}

#[tokio::test]
async fn complete_propagates_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream busy"))
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(cfg(&server.uri(), "llama3")).unwrap();
    let err = provider.complete(UnifiedRequest::user_only("hi")).await.unwrap_err();
    let s = err.to_string();
    assert!(s.contains("503"), "{s}");
}

#[tokio::test]
async fn embed_batch_loops_per_input_and_aggregates_dim() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embedding": [0.1, 0.2, 0.3],
            "prompt_eval_count": 5
        })))
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(cfg(&server.uri(), "bge-m3")).unwrap();
    let resp = provider
        .embed_batch(EmbeddingRequest::new(vec!["a".into(), "b".into(), "c".into()]))
        .await
        .unwrap();

    assert_eq!(resp.vectors.len(), 3);
    assert_eq!(resp.vectors[0], vec![0.1, 0.2, 0.3]);
    assert_eq!(resp.vectors[1], vec![0.1, 0.2, 0.3]);
    assert_eq!(resp.vectors[2], vec![0.1, 0.2, 0.3]);
    assert_eq!(resp.usage.prompt, 15); // 3 × 5 aggregated
    assert_eq!(resp.usage.completion, 0);
}

#[tokio::test]
async fn invalid_endpoint_rejected_at_construction() {
    let cfg = ProviderConfig {
        provider: ProviderKind::Ollama,
        model: "llama3".into(),
        endpoint: "ftp://nope".into(),
        ..Default::default()
    };
    let err = OllamaProvider::new(cfg).unwrap_err();
    matches!(err, GatewayError::Provider(_));
}

#[tokio::test]
async fn empty_response_message_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"done": true})))
        .mount(&server)
        .await;

    let provider = OllamaProvider::new(cfg(&server.uri(), "llama3")).unwrap();
    let err = provider.complete(UnifiedRequest::user_only("hi")).await.unwrap_err();
    let s = err.to_string();
    assert!(s.contains("empty"), "{s}");
}
