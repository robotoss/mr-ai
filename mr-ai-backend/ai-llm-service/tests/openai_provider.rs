//! Wiremock-backed unit tests for [`OpenAiProvider`].

use ai_llm_service::providers::OpenAiProvider;
use ai_llm_service::{
    EmbeddingProvider, EmbeddingRequest, GatewayError, LlmProvider, ProviderConfig, ProviderKind,
    UnifiedRequest,
};
use serde_json::json;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(endpoint: &str, model: &str) -> ProviderConfig {
    ProviderConfig {
        provider: ProviderKind::OpenAI,
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        api_key: Some("sk-test".to_string()),
        max_tokens: Some(64),
        temperature: Some(0.2),
        top_p: None,
        timeout_secs: Some(5),
        ..Default::default()
    }
}

#[tokio::test]
async fn complete_parses_choices_and_usage_and_sends_bearer() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(header_exists("content-type"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "x",
            "choices": [{"message": {"role": "assistant", "content": "Hi from OpenAI"}}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 4, "total_tokens": 13}
        })))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(&server.uri(), "gpt-4o-mini")).unwrap();
    let resp = provider.complete(UnifiedRequest::user_only("hi")).await.unwrap();

    assert_eq!(resp.content, "Hi from OpenAI");
    assert_eq!(resp.usage.prompt, 9);
    assert_eq!(resp.usage.completion, 4);
    assert_eq!(resp.usage.total, 13);
    assert_eq!(resp.provider, ProviderKind::OpenAI);
    assert_eq!(resp.model, "gpt-4o-mini");
}

#[tokio::test]
async fn missing_api_key_rejected() {
    let cfg = ProviderConfig {
        provider: ProviderKind::OpenAI,
        model: "gpt-4o-mini".into(),
        endpoint: "https://api.openai.com".into(),
        api_key: None,
        ..Default::default()
    };
    let err = OpenAiProvider::new(cfg).unwrap_err();
    matches!(err, GatewayError::Provider(_));
}

#[tokio::test]
async fn embed_batch_native_array_and_dimension_alignment() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"index": 1, "embedding": [0.4, 0.5]},
                {"index": 0, "embedding": [0.1, 0.2]}
            ],
            "usage": {"prompt_tokens": 6, "total_tokens": 6}
        })))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(&server.uri(), "text-embedding-3-small")).unwrap();
    let resp = provider
        .embed_batch(EmbeddingRequest::new(vec!["a".into(), "b".into()]))
        .await
        .unwrap();

    // Vectors must be re-ordered by `index` ascending.
    assert_eq!(resp.vectors.len(), 2);
    assert_eq!(resp.vectors[0], vec![0.1, 0.2]);
    assert_eq!(resp.vectors[1], vec![0.4, 0.5]);
    assert_eq!(resp.usage.prompt, 6);
}

#[tokio::test]
async fn complete_propagates_429_with_snippet() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(&server.uri(), "gpt-4o-mini")).unwrap();
    let err = provider.complete(UnifiedRequest::user_only("hi")).await.unwrap_err();
    let s = err.to_string();
    assert!(s.contains("429"), "{s}");
    assert!(s.contains("rate limited"), "{s}");
}

#[tokio::test]
async fn empty_choices_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [],
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
        })))
        .mount(&server)
        .await;

    let provider = OpenAiProvider::new(cfg(&server.uri(), "gpt-4o-mini")).unwrap();
    let err = provider.complete(UnifiedRequest::user_only("hi")).await.unwrap_err();
    assert!(err.to_string().contains("empty"), "{err}");
}
