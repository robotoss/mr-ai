//! Shared helpers for webhook handlers (payload extraction, event_id
//! derivation, persistence + enqueue glue).

use axum::http::StatusCode;
use domain::{ProjectId, ProviderKind, RepoId, WebhookEventId};
use persistence::repos::jobs;
use persistence::repos::projects;
use persistence::repos::webhook_events::{self, RecordOutcome, WebhookRecord};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{debug, info, warn};

use crate::error_handler::AppError;

/// Outcome reported back to the caller. Wrapped in JSON.
pub struct WebhookOutcome {
    pub status: StatusCode,
    pub event_id: WebhookEventId,
    pub deduplicated: bool,
    pub job_kind: Option<&'static str>,
}

impl WebhookOutcome {
    pub fn into_json(self) -> (StatusCode, axum::Json<Value>) {
        let body = json!({
            "event_id": self.event_id.to_string(),
            "duplicate": self.deduplicated,
            "enqueued_kind": self.job_kind,
        });
        (self.status, axum::Json(body))
    }
}

/// Compute the body hash used for both `payload_hash` storage and the
/// fallback event id when the provider does not supply one.
pub fn body_hash_hex(body: &[u8]) -> ([u8; 32], String) {
    let digest = Sha256::digest(body);
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    (bytes, hex)
}

/// Pick the first non-empty repo URL from a list of candidate JSON paths.
/// Each path is a slice of keys to descend into.
pub fn pick_remote_url(payload: &Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let mut cur = payload;
        let mut ok = true;
        for key in *path {
            match cur.get(*key) {
                Some(next) => cur = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let Some(s) = cur.as_str() {
                if !s.is_empty() {
                    return Some(s.to_owned());
                }
            }
        }
    }
    None
}

pub struct ResolvedRepo {
    pub project_id: ProjectId,
    /// Read by the multi-repo fan-out worker that lands in S2-D / S3 — kept
    /// here so the resolver contract is stable from S2-B onwards.
    #[allow(dead_code)]
    pub repo_id: RepoId,
    /// Surfaced in `UnknownRepo` rejection messages and consumed by the
    /// multi-repo fan-out worker.
    #[allow(dead_code)]
    pub remote_url: String,
}

/// Look up the project + repo for a given remote URL, with lenient matching.
pub async fn resolve_repo(pool: &PgPool, remote_url: &str) -> Result<Option<ResolvedRepo>, AppError> {
    let resolved = projects::find_repo_by_remote_url_lenient(pool, remote_url).await?;
    Ok(resolved.map(|(project_id, repo_id)| ResolvedRepo {
        project_id,
        repo_id,
        remote_url: remote_url.to_owned(),
    }))
}

/// Decision returned by per-provider parsers.
pub enum Decision {
    /// Verified, recognised and ready to enqueue.
    Enqueue {
        provider: ProviderKind,
        event_id: String,
        event_kind: String,
        job_kind: &'static str,
        repo: ResolvedRepo,
        job_payload: Value,
    },
    /// Verified but ignored (ping, irrelevant kind, ...). Still recorded for
    /// audit, no job created.
    Ack {
        provider: ProviderKind,
        event_id: String,
        event_kind: String,
    },
    /// Verified but the repo is unknown — caller returns 422 with diagnostics.
    UnknownRepo {
        provider: ProviderKind,
        event_id: String,
        event_kind: String,
        remote_url: String,
    },
}

/// Common end-of-pipeline: record + (optionally) enqueue. Idempotent across
/// retries.
pub async fn record_and_enqueue(
    pool: &PgPool,
    payload: &Value,
    payload_hash: &[u8],
    decision: Decision,
) -> Result<WebhookOutcome, AppError> {
    match decision {
        Decision::Enqueue {
            provider,
            event_id,
            event_kind,
            job_kind,
            repo,
            mut job_payload,
        } => {
            // Inject the current span's W3C traceparent so the worker
            // can attach the resulting job span to the same trace as
            // this webhook handler. Noop when OTLP is not configured.
            observability::inject_into_payload(&mut job_payload);
            let rec = WebhookRecord {
                provider,
                event_id: event_id.clone(),
                event_kind,
                payload_hash: payload_hash.to_vec(),
                payload: payload.clone(),
            };
            // Atomic record + enqueue + status-update so a crash mid-
            // pipeline cannot leave a "received" event without a matching
            // queued job (which would never re-enqueue thanks to the
            // ON CONFLICT DO NOTHING dedup on retry).
            let mut tx = pool
                .begin()
                .await
                .map_err(persistence::PersistenceError::from)?;
            let recorded = webhook_events::record_in_tx(&mut tx, &rec).await?;
            if recorded.outcome == RecordOutcome::Duplicate {
                tx.commit()
                    .await
                    .map_err(persistence::PersistenceError::from)?;
                info!(target = "webhook", provider = %provider, event_id = %event_id, "duplicate event ignored");
                return Ok(WebhookOutcome {
                    status: StatusCode::OK,
                    event_id: recorded.id,
                    deduplicated: true,
                    job_kind: Some(job_kind),
                });
            }

            let opts = jobs::EnqueueOptions {
                project_id: Some(repo.project_id),
                ..Default::default()
            };
            let job_id = jobs::enqueue_in_tx(&mut tx, job_kind, &job_payload, opts).await?;
            webhook_events::mark_enqueued_in_tx(&mut tx, recorded.id).await?;
            tx.commit()
                .await
                .map_err(persistence::PersistenceError::from)?;
            info!(
                target = "webhook",
                provider = %provider,
                event_id = %event_id,
                job_id = %job_id,
                kind = job_kind,
                project_id = %repo.project_id,
                "enqueued job"
            );
            Ok(WebhookOutcome {
                status: StatusCode::ACCEPTED,
                event_id: recorded.id,
                deduplicated: false,
                job_kind: Some(job_kind),
            })
        }
        Decision::Ack {
            provider,
            event_id,
            event_kind,
        } => {
            let rec = WebhookRecord {
                provider,
                event_id,
                event_kind,
                payload_hash: payload_hash.to_vec(),
                payload: payload.clone(),
            };
            let recorded = webhook_events::record(pool, &rec).await?;
            debug!(target = "webhook", provider = %provider, event_id = %recorded.id, dup = ?recorded.outcome, "ack-only event recorded");
            Ok(WebhookOutcome {
                status: StatusCode::OK,
                event_id: recorded.id,
                deduplicated: recorded.outcome == RecordOutcome::Duplicate,
                job_kind: None,
            })
        }
        Decision::UnknownRepo {
            provider,
            event_id,
            event_kind,
            remote_url,
        } => {
            let rec = WebhookRecord {
                provider,
                event_id: event_id.clone(),
                event_kind,
                payload_hash: payload_hash.to_vec(),
                payload: payload.clone(),
            };
            let recorded = webhook_events::record(pool, &rec).await?;
            webhook_events::mark_rejected(pool, recorded.id).await?;
            warn!(
                target = "webhook",
                provider = %provider,
                event_id = %event_id,
                remote_url = %remote_url,
                "rejected: repo not registered in any project group"
            );
            Err(AppError::Http {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "UNKNOWN_REPO",
                message: format!(
                    "remote_url '{remote_url}' is not registered in any project group"
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pick_remote_url_descends_paths() {
        let payload = json!({
            "project": {
                "git_ssh_url": "git@gitlab.com:org/app.git",
                "git_http_url": "https://gitlab.com/org/app.git"
            }
        });
        let url = pick_remote_url(
            &payload,
            &[&["project", "git_ssh_url"], &["project", "git_http_url"]],
        );
        assert_eq!(url.as_deref(), Some("git@gitlab.com:org/app.git"));
    }

    #[test]
    fn pick_remote_url_falls_through_missing() {
        let payload = json!({
            "project": { "git_http_url": "https://example/x.git" }
        });
        let url = pick_remote_url(
            &payload,
            &[&["project", "git_ssh_url"], &["project", "git_http_url"]],
        );
        assert_eq!(url.as_deref(), Some("https://example/x.git"));
    }

    #[test]
    fn body_hash_is_stable_and_hex() {
        let (_, hex) = body_hash_hex(b"hello");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
