//! HTTP-level smoke test for the inbound webhook surface.
//!
//! Boots a real Postgres via testcontainers, builds a minimal `AppState`
//! using the dummy LLM gateway from `ai_llm_service::test_support`, and
//! drives the GitLab webhook handler through `axum::Router::oneshot`.
//! The test asserts that a verified payload lands in `webhook_events`,
//! enqueues a job in `jobs`, and that a redelivery short-circuits as a
//! duplicate.
//!
//! Marked `#[ignore]` so the default Docker-free `cargo test` path stays
//! green. Run with `cargo test --workspace --tests -- --ignored`.

#![allow(clippy::needless_return)]

use std::sync::Arc;

use ai_llm_service::test_support::dummy_gateway;
use api::routes::webhooks::gitlab::gitlab_webhook_route;
use axum::{routing::post, Router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{ProjectId, ProviderKind, RepoId};
use persistence::repos::{jobs, projects, webhook_events};
use secrets::FileSecretProvider;
use services::llm_health::LlmHealthMonitor;
use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres as PgImage;
use tower::ServiceExt; // for `oneshot`

async fn boot_pool() -> (PgPool, testcontainers::ContainerAsync<PgImage>) {
    let container = PgImage::default()
        .start()
        .await
        .expect("postgres container");
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let pool = persistence::init_pool(&persistence::PoolConfig {
        url,
        max_connections: 4,
        acquire_timeout: std::time::Duration::from_secs(10),
    })
    .await
    .expect("connect");
    persistence::run_migrations(&pool).await.unwrap();
    (pool, container)
}

async fn register_repo(
    pool: &PgPool,
    remote_url: &str,
) -> (ProjectId, RepoId) {
    let project_id = ProjectId::new();
    let repo = domain::ProjectRepo {
        id: RepoId::new(),
        project_id,
        provider: ProviderKind::Gitlab,
        remote_url: remote_url.into(),
        default_branch: "main".into(),
        is_primary: true,
    };
    let group = domain::ProjectGroup {
        id: project_id,
        slug: "smoke".into(),
        name: "Smoke".into(),
        repos: vec![repo.clone()],
        dependencies: vec![],
    };
    projects::upsert_group(pool, &group).await.unwrap();
    (project_id, repo.id)
}

/// Build state backed by a `FileSecretProvider` rooted in a temp dir so
/// the test can plant `webhook_hmac` without mutating process-global env
/// (race-free with any other `#[ignore]` test that needs the same key).
fn build_state(
    pool: PgPool,
    secrets_root: &std::path::Path,
) -> Arc<api::core::app_state::AppState> {
    let config = Arc::new(api::core::app_state::AppConfig {
        project_slug: "smoke".into(),
        default_project_id: domain::ProjectId::new(),
        git_api_base: "https://gitlab.example/api/v4".into(),
        git_token: "stub-token".into(),
        trigger_secret: "stub".into(),
    });
    Arc::new(api::core::app_state::AppState::new(
        config,
        dummy_gateway(),
        Arc::new(FileSecretProvider::new(secrets_root.to_path_buf())),
        Some(pool),
        LlmHealthMonitor::empty(),
    ))
}

#[tokio::test]
#[ignore = "requires Docker; run with --ignored"]
async fn webhook_gitlab_full_round_trip() {
    let (pool, _container) = boot_pool().await;

    // Provider URL the webhook will reference. Register it so the route
    // resolves to a known repo.
    let remote = "git@gitlab.com:org/smoke.git";
    let (_project_id, _repo_id) = register_repo(&pool, remote).await;

    // Webhook secret comes from SecretProvider — plant it on disk via
    // FileSecretProvider so we don't race other tests on env state.
    let secret = "smoke-secret";
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_dir = tmp.path().join("_global");
    std::fs::create_dir_all(&global_dir).expect("create _global dir");
    std::fs::write(global_dir.join("webhook_hmac"), secret).expect("write secret");

    let state = build_state(pool.clone(), tmp.path());
    let app = Router::new()
        .route("/webhooks/gitlab", post(gitlab_webhook_route))
        .with_state(state);

    let body = serde_json::json!({
        "object_kind": "merge_request",
        "project": { "git_ssh_url": remote },
        "object_attributes": {
            "iid": 17,
            "source_branch": "feat/smoke",
            "target_branch": "main",
            "last_commit": { "id": "deadbeef" }
        }
    });
    let body_bytes = serde_json::to_vec(&body).unwrap();

    let request = Request::builder()
        .method("POST")
        .uri("/webhooks/gitlab")
        .header("content-type", "application/json")
        .header("X-Gitlab-Token", secret)
        .header("X-Gitlab-Event", "Merge Request Hook")
        .header("X-Gitlab-Event-UUID", "smoke-event-1")
        .body(Body::from(body_bytes.clone()))
        .unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body_resp = to_bytes(response.into_body(), 4096).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body_resp).unwrap();
    assert_eq!(parsed["duplicate"], serde_json::Value::Bool(false));
    assert_eq!(
        parsed["enqueued_kind"].as_str(),
        Some(worker::handlers::KIND_INGEST_MR)
    );

    // Persistence side-effects.
    let event_count: (i64,) = sqlx::query_as(
        "SELECT count(*)::bigint FROM webhook_events WHERE provider='gitlab' AND event_id='smoke-event-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event_count.0, 1);

    let job_count: (i64,) = sqlx::query_as(
        "SELECT count(*)::bigint FROM jobs WHERE kind='IngestMr' AND status='queued'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(job_count.0, 1);

    // Replay → 200 + duplicate=true; no extra rows.
    let replay = Request::builder()
        .method("POST")
        .uri("/webhooks/gitlab")
        .header("content-type", "application/json")
        .header("X-Gitlab-Token", secret)
        .header("X-Gitlab-Event", "Merge Request Hook")
        .header("X-Gitlab-Event-UUID", "smoke-event-1")
        .body(Body::from(body_bytes))
        .unwrap();
    let replay_response = app.oneshot(replay).await.unwrap();
    assert_eq!(replay_response.status(), StatusCode::OK);
    let replay_body = to_bytes(replay_response.into_body(), 4096).await.unwrap();
    let replay_parsed: serde_json::Value = serde_json::from_slice(&replay_body).unwrap();
    assert_eq!(replay_parsed["duplicate"], serde_json::Value::Bool(true));

    let job_count_after: (i64,) = sqlx::query_as(
        "SELECT count(*)::bigint FROM jobs WHERE kind='IngestMr'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(job_count_after.0, 1);

    // Worker can claim the job (verifies queue table is healthy).
    let claimed = jobs::claim_next(&pool, "smoke-worker")
        .await
        .unwrap()
        .expect("queued job should be claimable");
    assert_eq!(claimed.kind, worker::handlers::KIND_INGEST_MR);

    // Webhook_events module isn't strictly required at this point but the
    // import keeps the assertion neighbourhood honest; force the import
    // to register so editors do not strip it.
    let _ = webhook_events::RecordOutcome::Inserted;
}
