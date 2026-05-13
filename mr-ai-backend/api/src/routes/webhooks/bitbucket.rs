//! Bitbucket webhook handler.
//!
//! Authentication: `X-Hub-Signature: sha256=<hex>` (HMAC-SHA256 over the raw
//! body, secret = `webhook_hmac_bitbucket` / `BITBUCKET_WEBHOOK_SECRET`). This is the
//! Bitbucket Server scheme; Bitbucket Cloud has no built-in HMAC and must
//! be fronted by a reverse proxy that adds the same header.
//! Event id source: `X-Request-UUID` header, with body-hash fallback.
//! Recognised events: `repo:push` → `IngestPush`, `pullrequest:created` /
//! `pullrequest:updated` → `IngestMr`.

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
    name = "webhook.bitbucket",
    skip_all,
    fields(body_size = body.len()),
)]
pub async fn bitbucket_webhook_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<impl IntoResponse> {
    observability::counter!(
        observability::metrics::WEBHOOK_RECEIVED_TOTAL,
        "provider" => "bitbucket",
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

    let signature = headers
        .get("X-Hub-Signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let expected = state
        .secrets
        .get_optional(None, &SecretKey::WebhookHmacBitbucket)
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
            message: "BITBUCKET_WEBHOOK_SECRET is not configured".into(),
        });
    };
    if let Err(err) =
        webhook::verify_bitbucket_signature(signature, &body, expected_secret.as_bytes())
    {
        warn!(target = "webhook.bitbucket", error = ?err, "signature rejected");
        return Err(AppError::Http {
            status: StatusCode::UNAUTHORIZED,
            code: "WEBHOOK_SIGNATURE_INVALID",
            message: "X-Hub-Signature mismatch".into(),
        });
    }

    let (hash_bytes, hash_hex) = body_hash_hex(&body);
    let event_id = headers
        .get("X-Request-UUID")
        .or_else(|| headers.get("X-Hook-UUID"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| format!("sha256:{hash_hex}"));
    let event_kind = headers
        .get("X-Event-Key")
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
                provider: ProviderKind::Bitbucket,
                event_id,
                event_kind,
                job_kind,
                repo,
                job_payload,
            },
            None => Decision::UnknownRepo {
                provider: ProviderKind::Bitbucket,
                event_id,
                event_kind,
                remote_url,
            },
        },
        Some(DecisionDraft::Ack) | None => Decision::Ack {
            provider: ProviderKind::Bitbucket,
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

/// Bitbucket exposes clone URLs in `repository.links.clone[*]` keyed by `name`
/// (`ssh` / `https`). Walk the array preferring SSH first.
fn extract_bitbucket_clone_url(payload: &Value) -> Option<String> {
    let arr = payload
        .pointer("/repository/links/clone")?
        .as_array()?;
    let pick = |target: &str| -> Option<String> {
        arr.iter().find_map(|entry| {
            let name = entry.get("name")?.as_str()?;
            if name == target {
                entry.get("href")?.as_str().map(str::to_owned)
            } else {
                None
            }
        })
    };
    pick("ssh").or_else(|| pick("https"))
}

fn build_decision(payload: &Value, event_kind: &str, event_id: String) -> Option<DecisionDraft> {
    match event_kind {
        "repo:push" => {
            let remote_url = extract_bitbucket_clone_url(payload).or_else(|| {
                pick_remote_url(
                    payload,
                    &[&["repository", "full_name"]],
                )
            })?;
            // Bitbucket push events list multiple changes. Use the first one
            // for branch/sha; the worker can re-derive details if needed.
            let first = payload
                .pointer("/push/changes/0")
                .cloned()
                .unwrap_or(Value::Null);
            let branch = first
                .pointer("/new/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let head_sha = first
                .pointer("/new/target/hash")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();

            Some(DecisionDraft::Enqueue {
                job_kind: worker::handlers::KIND_INGEST_PUSH,
                remote_url: remote_url.clone(),
                job_payload: json!({
                    "provider": "bitbucket",
                    "event_id": event_id,
                    "remote_url": remote_url,
                    "branch": branch,
                    "head_sha": head_sha,
                }),
            })
        }
        "pullrequest:created" | "pullrequest:updated" => {
            let remote_url = extract_bitbucket_clone_url(payload)?;
            let pr = payload.get("pullrequest")?;
            let mr_iid = pr.get("id")?.to_string();
            let source_branch = pr
                .pointer("/source/branch/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let target_branch = pr
                .pointer("/destination/branch/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let head_sha = pr
                .pointer("/source/commit/hash")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();

            Some(DecisionDraft::Enqueue {
                job_kind: worker::handlers::KIND_INGEST_MR,
                remote_url: remote_url.clone(),
                job_payload: json!({
                    "provider": "bitbucket",
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
