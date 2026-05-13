//! Wiremock-backed unit tests for [`BedrockProvider`].
//!
//! Note: wiremock does not validate AWS SigV4 signatures. We verify the
//! provider produces correct URLs, JSON bodies, and the expected
//! `Authorization: AWS4-HMAC-SHA256 …` / `x-amz-date` / `x-amz-content-sha256`
//! headers. End-to-end signature correctness is covered by `tests/sigv4_*`
//! unit tests in the SigV4 module itself.

use std::collections::HashMap;

use ai_llm_service::providers::BedrockProvider;
use ai_llm_service::{
    EmbeddingProvider, EmbeddingRequest, GatewayError, LlmProvider, ProviderConfig, ProviderKind,
    UnifiedMessage, UnifiedRequest,
};
use serde_json::{json, Value};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn cfg(endpoint: &str, model: &str) -> ProviderConfig {
    let mut extras = HashMap::new();
    extras.insert("region".into(), "us-east-1".into());
    extras.insert(
        "secret_key".into(),
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
    );
    ProviderConfig {
        provider: ProviderKind::Bedrock,
        model: model.to_string(),
        endpoint: endpoint.to_string(),
        api_key: Some("AKIAIOSFODNN7EXAMPLE".into()),
        max_tokens: Some(64),
        temperature: Some(0.2),
        top_p: None,
        timeout_secs: Some(5),
        extras,
    }
}

#[tokio::test]
async fn converse_signs_request_and_parses_output() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path(
            "/model/anthropic.claude-3-5-sonnet-20241022-v2:0/converse",
        ))
        .and(header_exists("authorization"))
        .and(header_exists("x-amz-date"))
        .and(header_exists("x-amz-content-sha256"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": {
                "message": {
                    "role": "assistant",
                    "content": [{"text": "hello from claude"}]
                }
            },
            "stopReason": "end_turn",
            "usage": {"inputTokens": 10, "outputTokens": 6, "totalTokens": 16}
        })))
        .mount(&server)
        .await;

    let provider = BedrockProvider::new(cfg(
        &server.uri(),
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
    ))
    .unwrap();

    let req = UnifiedRequest::with_system(Some("be brief"), "what is rust?");
    let resp = provider.complete(req).await.unwrap();

    assert_eq!(resp.content, "hello from claude");
    assert_eq!(resp.usage.prompt, 10);
    assert_eq!(resp.usage.completion, 6);
    assert_eq!(resp.usage.total, 16);
    assert_eq!(resp.provider, ProviderKind::Bedrock);
}

#[tokio::test]
async fn converse_separates_system_messages_from_conversation() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/model/anthropic.claude-3-5-haiku-20241022-v1:0/converse"))
        .respond_with(|req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            // System messages must live in the top-level `system` array,
            // not inside `messages`.
            let system = body.get("system").and_then(|s| s.as_array()).unwrap();
            assert_eq!(system.len(), 1);
            assert_eq!(system[0]["text"].as_str().unwrap(), "be brief");

            let msgs = body["messages"].as_array().unwrap();
            assert_eq!(msgs.len(), 2);
            assert_eq!(msgs[0]["role"], "user");
            assert_eq!(msgs[1]["role"], "assistant");
            // inferenceConfig wired from cfg + req.
            assert_eq!(body["inferenceConfig"]["maxTokens"], json!(64));
            assert_eq!(body["inferenceConfig"]["temperature"], json!(0.2));

            ResponseTemplate::new(200).set_body_json(json!({
                "output": {"message": {"role": "assistant", "content": [{"text": "ok"}]}},
                "usage": {"inputTokens": 1, "outputTokens": 1, "totalTokens": 2}
            }))
        })
        .mount(&server)
        .await;

    let provider = BedrockProvider::new(cfg(
        &server.uri(),
        "anthropic.claude-3-5-haiku-20241022-v1:0",
    ))
    .unwrap();

    let mut req = UnifiedRequest::user_only("hi");
    req.messages.insert(0, UnifiedMessage::system("be brief"));
    req.messages.push(UnifiedMessage::assistant("partial reply"));

    let _ = provider.complete(req).await.unwrap();
}

#[tokio::test]
async fn converse_propagates_403_with_snippet() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/model/anthropic.claude-3-5-haiku-20241022-v1:0/converse"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Signature mismatch"))
        .mount(&server)
        .await;

    let provider = BedrockProvider::new(cfg(
        &server.uri(),
        "anthropic.claude-3-5-haiku-20241022-v1:0",
    ))
    .unwrap();

    let err = provider
        .complete(UnifiedRequest::user_only("hi"))
        .await
        .unwrap_err();
    let s = err.to_string();
    assert!(s.contains("403"), "{s}");
    assert!(s.contains("Signature mismatch"), "{s}");
}

#[tokio::test]
async fn titan_embeddings_loops_and_aggregates_tokens() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/model/amazon.titan-embed-text-v2:0/invoke"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "embedding": [0.1, 0.2, 0.3, 0.4],
            "inputTextTokenCount": 4
        })))
        .mount(&server)
        .await;

    let provider =
        BedrockProvider::new(cfg(&server.uri(), "amazon.titan-embed-text-v2:0")).unwrap();

    let resp = provider
        .embed_batch(EmbeddingRequest::new(vec![
            "alpha".into(),
            "beta".into(),
            "gamma".into(),
        ]))
        .await
        .unwrap();

    assert_eq!(resp.vectors.len(), 3);
    assert_eq!(resp.vectors[0], vec![0.1, 0.2, 0.3, 0.4]);
    assert_eq!(resp.usage.prompt, 12); // 3 × 4
    assert_eq!(resp.usage.completion, 0);
    assert_eq!(resp.provider, ProviderKind::Bedrock);
}

#[tokio::test]
async fn missing_region_in_extras_rejected() {
    let mut extras = HashMap::new();
    extras.insert(
        "secret_key".into(),
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
    );
    let cfg = ProviderConfig {
        provider: ProviderKind::Bedrock,
        model: "anthropic.claude-3-5-haiku-20241022-v1:0".into(),
        endpoint: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        api_key: Some("AKIA".into()),
        max_tokens: None,
        temperature: None,
        top_p: None,
        timeout_secs: None,
        extras,
    };

    let err = BedrockProvider::new(cfg).unwrap_err();
    matches!(err, GatewayError::Provider(_));
    assert!(err.to_string().contains("region"));
}

#[tokio::test]
async fn missing_secret_key_in_extras_rejected() {
    let mut extras = HashMap::new();
    extras.insert("region".into(), "us-east-1".into());
    let cfg = ProviderConfig {
        provider: ProviderKind::Bedrock,
        model: "anthropic.claude-3-5-haiku-20241022-v1:0".into(),
        endpoint: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        api_key: Some("AKIA".into()),
        max_tokens: None,
        temperature: None,
        top_p: None,
        timeout_secs: None,
        extras,
    };

    let err = BedrockProvider::new(cfg).unwrap_err();
    assert!(err.to_string().contains("secret_key"));
}

#[tokio::test]
async fn missing_access_key_id_rejected() {
    let mut extras = HashMap::new();
    extras.insert("region".into(), "us-east-1".into());
    extras.insert("secret_key".into(), "abc".into());
    let cfg = ProviderConfig {
        provider: ProviderKind::Bedrock,
        model: "anthropic.claude-3-5-haiku-20241022-v1:0".into(),
        endpoint: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        api_key: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
        timeout_secs: None,
        extras,
    };

    let err = BedrockProvider::new(cfg).unwrap_err();
    assert!(err.to_string().contains("API key"));
}

#[tokio::test]
async fn empty_converse_output_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/model/anthropic.claude-3-5-haiku-20241022-v1:0/converse"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "stopReason": "end_turn",
            "usage": {"inputTokens": 0, "outputTokens": 0, "totalTokens": 0}
        })))
        .mount(&server)
        .await;

    let provider = BedrockProvider::new(cfg(
        &server.uri(),
        "anthropic.claude-3-5-haiku-20241022-v1:0",
    ))
    .unwrap();

    let err = provider
        .complete(UnifiedRequest::user_only("hi"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("empty"));
}
