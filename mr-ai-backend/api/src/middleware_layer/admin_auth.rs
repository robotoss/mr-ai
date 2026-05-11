//! Shared-secret guard for operator endpoints (`/admin/*`, `/retrieve`).
//!
//! Until a proper identity layer lands, anything that reads the
//! indexed codebase or triggers heavy worker jobs is gated by the
//! same `TRIGGER_SECRET` already used to protect `/trigger_git_mr`.
//! Comparison is constant-time via [`secrets::webhook::verify_gitlab_token`]
//! so a timing-side channel can't leak the secret.
//!
//! The middleware is opt-in per route group — `lib.rs` wires it onto
//! the operator router; unauthenticated routes (`/webhooks/*`,
//! `/health/*`, `/usage`) stay untouched.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::core::app_state::AppState;

/// Header name the operator must present. Lowercase per HTTP spec —
/// callers using `axum::http::HeaderMap` lookup are case-insensitive.
pub const ADMIN_TOKEN_HEADER: &str = "x-admin-token";

pub async fn admin_auth(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let presented = req
        .headers()
        .get(ADMIN_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = state.config.trigger_secret.as_bytes();
    if presented.is_empty()
        || secrets::webhook::verify_gitlab_token(presented.as_bytes(), expected).is_err()
    {
        tracing::warn!(
            target = "api::admin_auth",
            path = %req.uri().path(),
            "rejecting request: missing or invalid X-Admin-Token"
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "UNAUTHORIZED",
                "message": "X-Admin-Token header required",
            })),
        )
            .into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use http_body_util::BodyExt as _;
    use std::path::PathBuf;
    use tower::ServiceExt;

    fn state(secret: &str) -> Arc<AppState> {
        let config = Arc::new(crate::core::app_state::AppConfig {
            project_slug: "test".into(),
            default_project_id: domain::ProjectId::new(),
            git_api_base: "https://gitlab.example/api/v4".into(),
            git_token: "x".into(),
            trigger_secret: secret.into(),
        });
        let rag_cfg = Arc::new(rag_base::structs::rag_base_config::RagConfig {
            project_name: "test".into(),
            code_jsonl: PathBuf::from("/tmp/unused"),
            qdrant: rag_base::structs::rag_base_config::QdrantConfig {
                url: "http://localhost:6334".into(),
                collection: "test".into(),
                distance: rag_base::structs::rag_base_config::DistanceMetric::Cosine,
                batch_size: 8,
            },
            embedding: rag_base::structs::rag_base_config::EmbeddingConfig { dim: 8 },
            search: rag_base::structs::rag_base_config::SearchConfig::default(),
            clamp: rag_base::structs::rag_base_config::ChunkClampConfig::default(),
        });
        Arc::new(AppState::new(
            config,
            ai_llm_service::test_support::dummy_gateway(),
            Arc::new(secrets::EnvSecretProvider::new()),
            None,
            services::llm_health::LlmHealthMonitor::empty(),
            rag_cfg,
        ))
    }

    async fn router(secret: &str) -> Router {
        Router::new()
            .route("/protected", get(|| async { "ok" }))
            .route_layer(axum::middleware::from_fn_with_state(
                state(secret),
                admin_auth,
            ))
            .with_state(state(secret))
    }

    #[tokio::test]
    async fn rejects_request_without_token() {
        let app = router("super-secret").await;
        let req = axum::http::Request::builder()
            .uri("/protected")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"], "UNAUTHORIZED");
    }

    #[tokio::test]
    async fn rejects_request_with_wrong_token() {
        let app = router("super-secret").await;
        let req = axum::http::Request::builder()
            .uri("/protected")
            .header(ADMIN_TOKEN_HEADER, "guess")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_request_with_correct_token() {
        let app = router("super-secret").await;
        let req = axum::http::Request::builder()
            .uri("/protected")
            .header(ADMIN_TOKEN_HEADER, "super-secret")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
