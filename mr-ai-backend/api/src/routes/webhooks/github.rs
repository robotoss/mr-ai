//! GitHub webhook handler.
//!
//! Authentication: `X-Hub-Signature-256: sha256=<hex>` (HMAC-SHA256 over the
//! raw body, secret = `webhook_hmac` / `WEBHOOK_HMAC_SECRET`).
//! Event id source: `X-GitHub-Delivery` header (UUID), with body-hash
//! fallback for resilience against malformed deliveries.
//! Recognised events: `push` → `IngestPush`, `pull_request` → `IngestMr`.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use domain::ProviderKind;
use secrets::{SecretKey, webhook};
use serde_json::{Value, json};
use tracing::warn;

use crate::core::app_state::AppState;
use crate::error_handler::{AppError, AppResult};

use super::common::{
    Decision, body_hash_hex, pick_remote_url, record_and_enqueue, resolve_repo,
};

pub async fn github_webhook_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<impl IntoResponse> {
    let pool = state
        .db
        .as_ref()
        .ok_or(AppError::Http {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "PERSISTENCE_DISABLED",
            message: "webhooks require DATABASE_URL to be configured".into(),
        })?;

    let signature = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let expected = state
        .secrets
        .get_optional(None, &SecretKey::WebhookHmac)
        .await
        .map_err(|e| AppError::Http {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "SECRET_LOOKUP_ERROR",
            message: e.to_string(),
        })?;
    let Some(expected_secret) = expected else {
        return Err(AppError::Http {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "WEBHOOK_SECRET_UNSET",
            message: "WEBHOOK_HMAC_SECRET is not configured".into(),
        });
    };
    if let Err(err) = webhook::verify_github_sha256(signature, &body, expected_secret.as_bytes()) {
        warn!(target = "webhook.github", error = ?err, "signature rejected");
        return Err(AppError::Http {
            status: StatusCode::UNAUTHORIZED,
            code: "WEBHOOK_SIGNATURE_INVALID",
            message: "X-Hub-Signature-256 mismatch".into(),
        });
    }

    let (hash_bytes, hash_hex) = body_hash_hex(&body);
    let event_id = headers
        .get("X-GitHub-Delivery")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| format!("sha256:{hash_hex}"));
    let event_kind = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();

    let payload: Value = serde_json::from_slice(&body).map_err(|e| AppError::Http {
        status: StatusCode::BAD_REQUEST,
        code: "WEBHOOK_BAD_BODY",
        message: format!("invalid JSON: {e}"),
    })?;

    let decision = build_decision(&payload, &event_kind, event_id.clone());

    let decision = match decision {
        Some(DecisionDraft::Enqueue {
            job_kind,
            remote_url,
            job_payload,
        }) => match resolve_repo(pool, &remote_url).await? {
            Some(repo) => Decision::Enqueue {
                provider: ProviderKind::Github,
                event_id,
                event_kind,
                job_kind,
                repo,
                job_payload,
            },
            None => Decision::UnknownRepo {
                provider: ProviderKind::Github,
                event_id,
                event_kind,
                remote_url,
            },
        },
        Some(DecisionDraft::Ack) | None => Decision::Ack {
            provider: ProviderKind::Github,
            event_id,
            event_kind,
        },
    };

    let outcome = record_and_enqueue(pool, &payload, &hash_bytes, decision).await?;
    Ok(outcome.into_json())
}

enum DecisionDraft {
    Enqueue {
        job_kind: &'static str,
        remote_url: String,
        job_payload: Value,
    },
    Ack,
}

fn build_decision(payload: &Value, event_kind: &str, event_id: String) -> Option<DecisionDraft> {
    match event_kind {
        "ping" => Some(DecisionDraft::Ack),
        "push" => {
            let remote_url = pick_remote_url(
                payload,
                &[
                    &["repository", "ssh_url"],
                    &["repository", "clone_url"],
                    &["repository", "git_url"],
                ],
            )?;
            let branch = payload
                .get("ref")
                .and_then(Value::as_str)
                .map(|r| r.trim_start_matches("refs/heads/").to_owned())
                .unwrap_or_default();
            let head_sha = payload
                .get("after")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();

            Some(DecisionDraft::Enqueue {
                job_kind: worker::handlers::KIND_INGEST_PUSH,
                remote_url: remote_url.clone(),
                job_payload: json!({
                    "provider": "github",
                    "event_id": event_id,
                    "remote_url": remote_url,
                    "branch": branch,
                    "head_sha": head_sha,
                }),
            })
        }
        "pull_request" => {
            let remote_url = pick_remote_url(
                payload,
                &[
                    &["repository", "ssh_url"],
                    &["repository", "clone_url"],
                ],
            )?;
            let pr = payload.get("pull_request")?;
            let mr_iid = pr.get("number")?.to_string();
            let source_branch = pr
                .get("head")
                .and_then(|h| h.get("ref"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let target_branch = pr
                .get("base")
                .and_then(|b| b.get("ref"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let head_sha = pr
                .get("head")
                .and_then(|h| h.get("sha"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();

            Some(DecisionDraft::Enqueue {
                job_kind: worker::handlers::KIND_INGEST_MR,
                remote_url: remote_url.clone(),
                job_payload: json!({
                    "provider": "github",
                    "event_id": event_id,
                    "remote_url": remote_url,
                    "mr_iid": mr_iid,
                    "source_branch": source_branch,
                    "target_branch": target_branch,
                    "head_sha": head_sha,
                }),
            })
        }
        _ => Some(DecisionDraft::Ack),
    }
}
