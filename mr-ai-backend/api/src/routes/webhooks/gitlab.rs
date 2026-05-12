//! GitLab webhook handler.
//!
//! Authentication: `X-Gitlab-Token` shared secret (constant-time match against
//! the configured `webhook_hmac` / `WEBHOOK_HMAC_SECRET`).
//! Event id source: `X-Gitlab-Event-UUID` header, falling back to a SHA-256
//! body hash so retries still deduplicate.
//! Recognised events: `push` → `IngestPush`, `merge_request` → `IngestMr`.
//! Anything else is recorded for audit and acknowledged.

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

#[tracing::instrument(
    name = "webhook.gitlab",
    skip_all,
    fields(body_size = body.len()),
)]
pub async fn gitlab_webhook_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<impl IntoResponse> {
    observability::counter!(
        observability::metrics::WEBHOOK_RECEIVED_TOTAL,
        "provider" => "gitlab",
    )
    .increment(1);

    let pool = state
        .db
        .as_ref()
        .ok_or(AppError::Http {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "PERSISTENCE_DISABLED",
            message: "webhooks require DATABASE_URL to be configured".into(),
        })?;

    // 1. Verify shared-token header against the configured secret.
    let presented = headers
        .get("X-Gitlab-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .as_bytes()
        .to_vec();
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
    if let Err(err) = webhook::verify_gitlab_token(&presented, expected_secret.as_bytes()) {
        warn!(target = "webhook.gitlab", error = ?err, "signature rejected");
        return Err(AppError::Http {
            status: StatusCode::UNAUTHORIZED,
            code: "WEBHOOK_SIGNATURE_INVALID",
            message: "X-Gitlab-Token mismatch".into(),
        });
    }

    // 2. Compute hash + event id.
    let (hash_bytes, hash_hex) = body_hash_hex(&body);
    let event_id = headers
        .get("X-Gitlab-Event-UUID")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| format!("sha256:{hash_hex}"));
    let event_kind = headers
        .get("X-Gitlab-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();

    // 3. Parse JSON body.
    let payload: Value = serde_json::from_slice(&body).map_err(|e| AppError::Http {
        status: StatusCode::BAD_REQUEST,
        code: "WEBHOOK_BAD_BODY",
        message: format!("invalid JSON: {e}"),
    })?;

    // 4. Build per-event decision.
    let decision = build_decision(&payload, &event_kind, event_id.clone());
    let decision = match decision {
        Some(decision_with_url) => match decision_with_url {
            DecisionDraft::Enqueue {
                job_kind,
                remote_url,
                job_payload,
                ..
            } => match resolve_repo(pool, &remote_url).await? {
                Some(repo) => Decision::Enqueue {
                    provider: ProviderKind::Gitlab,
                    event_id,
                    event_kind,
                    job_kind,
                    repo,
                    job_payload,
                },
                None => Decision::UnknownRepo {
                    provider: ProviderKind::Gitlab,
                    event_id,
                    event_kind,
                    remote_url,
                },
            },
            DecisionDraft::Ack => Decision::Ack {
                provider: ProviderKind::Gitlab,
                event_id,
                event_kind,
            },
        },
        None => Decision::Ack {
            provider: ProviderKind::Gitlab,
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

fn build_decision(
    payload: &Value,
    event_kind: &str,
    event_id: String,
) -> Option<DecisionDraft> {
    // Gitlab uses `object_kind` in the body too; prefer it when present.
    let object_kind = payload
        .get("object_kind")
        .and_then(Value::as_str)
        .unwrap_or(event_kind)
        .to_ascii_lowercase();

    match object_kind.as_str() {
        "push" | "tag_push" => {
            let remote_url = pick_remote_url(
                payload,
                &[
                    &["project", "git_ssh_url"],
                    &["project", "git_http_url"],
                    &["repository", "git_ssh_url"],
                    &["repository", "git_http_url"],
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
                    "provider": "gitlab",
                    "event_id": event_id,
                    "remote_url": remote_url,
                    "branch": branch,
                    "head_sha": head_sha,
                }),
            })
        }
        "merge_request" => {
            let remote_url = pick_remote_url(
                payload,
                &[
                    &["project", "git_ssh_url"],
                    &["project", "git_http_url"],
                ],
            )?;
            let attrs = payload.get("object_attributes")?;
            let mr_iid = attrs.get("iid")?.to_string();
            let source_branch = attrs
                .get("source_branch")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let target_branch = attrs
                .get("target_branch")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let head_sha = attrs
                .get("last_commit")
                .and_then(|c| c.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();

            Some(DecisionDraft::Enqueue {
                job_kind: worker::handlers::KIND_INGEST_MR,
                remote_url: remote_url.clone(),
                job_payload: json!({
                    "provider": "gitlab",
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
