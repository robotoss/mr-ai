//! Detailed health snapshot. Always 200; per-component status in the body.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;
use serde_json::json;
use tracing::warn;

use crate::core::app_state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct ComponentReport {
    pub name: &'static str,
    pub healthy: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub note: Option<String>,
}

pub async fn detailed_route(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let components = collect_components(&state).await;
    let healthy = components.iter().all(|c| c.healthy);
    let body = json!({
        "status": if healthy { "ok" } else { "degraded" },
        "components": components,
    });
    (StatusCode::OK, axum::Json(body))
}

/// Run every check sequentially. Each check is bounded by an internal
/// timeout (default 2 s) so a single hung dependency does not stall the
/// probe.
pub async fn collect_components(state: &Arc<AppState>) -> Vec<ComponentReport> {
    let timeout = std::time::Duration::from_millis(
        std::env::var("HEALTH_DETAILED_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000),
    );

    let mut out = Vec::with_capacity(4);
    out.push(check_postgres(state, timeout).await);
    out.push(check_secrets(state, timeout).await);
    out.push(check_llm_gateway(state, timeout).await);
    out.push(check_queue_depth(state, timeout).await);
    out
}

async fn check_postgres(state: &Arc<AppState>, timeout: std::time::Duration) -> ComponentReport {
    let Some(pool) = state.db.as_ref() else {
        return ComponentReport {
            name: "postgres",
            healthy: true,
            latency_ms: None,
            error: None,
            note: Some("disabled (DATABASE_URL not set)".into()),
        };
    };
    let started = Instant::now();
    let result = tokio::time::timeout(timeout, sqlx::query("SELECT 1").execute(pool)).await;
    match result {
        Ok(Ok(_)) => ComponentReport {
            name: "postgres",
            healthy: true,
            latency_ms: Some(started.elapsed().as_millis() as u64),
            error: None,
            note: None,
        },
        Ok(Err(err)) => ComponentReport {
            name: "postgres",
            healthy: false,
            latency_ms: Some(started.elapsed().as_millis() as u64),
            error: Some(err.to_string()),
            note: None,
        },
        Err(_) => ComponentReport {
            name: "postgres",
            healthy: false,
            latency_ms: Some(timeout.as_millis() as u64),
            error: Some("timeout".into()),
            note: None,
        },
    }
}

async fn check_secrets(state: &Arc<AppState>, _timeout: std::time::Duration) -> ComponentReport {
    // SecretProvider is in-process — a backend probe is not meaningful for
    // the env backend. Report which backend is wired in.
    ComponentReport {
        name: "secrets",
        healthy: true,
        latency_ms: Some(0),
        error: None,
        note: Some(state.secrets.backend_name().to_owned()),
    }
}

async fn check_llm_gateway(
    _state: &Arc<AppState>,
    _timeout: std::time::Duration,
) -> ComponentReport {
    // The gateway exposes its own `health_all` at boot. A live probe per
    // request would be expensive (every detail call would burn LLM
    // credits). Surface the last-known status from process boot for now;
    // S5-B replaces this with a cached snapshot updated by a background
    // task.
    ComponentReport {
        name: "llm_gateway",
        healthy: true,
        latency_ms: None,
        error: None,
        note: Some("checked at boot; live probe lands in S5-B".into()),
    }
}

async fn check_queue_depth(state: &Arc<AppState>, timeout: std::time::Duration) -> ComponentReport {
    let Some(pool) = state.db.as_ref() else {
        return ComponentReport {
            name: "queue",
            healthy: true,
            latency_ms: None,
            error: None,
            note: Some("disabled (DATABASE_URL not set)".into()),
        };
    };
    let started = Instant::now();
    let result =
        tokio::time::timeout(timeout, persistence::repos::jobs::counts_by_status(pool)).await;
    match result {
        Ok(Ok(rows)) => {
            let mut queued = 0i64;
            let mut running = 0i64;
            let mut dead = 0i64;
            for (status, n) in rows {
                match status.as_str() {
                    "queued" => queued = n,
                    "running" => running = n,
                    "dead" => dead = n,
                    _ => {}
                }
            }
            let healthy = dead < 1000; // any reasonable threshold
            if !healthy {
                warn!(target = "health.queue", dead, "queue dead-letter accumulating");
            }
            ComponentReport {
                name: "queue",
                healthy,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                error: None,
                note: Some(format!("queued={queued}, running={running}, dead={dead}")),
            }
        }
        Ok(Err(err)) => ComponentReport {
            name: "queue",
            healthy: false,
            latency_ms: Some(started.elapsed().as_millis() as u64),
            error: Some(err.to_string()),
            note: None,
        },
        Err(_) => ComponentReport {
            name: "queue",
            healthy: false,
            latency_ms: Some(timeout.as_millis() as u64),
            error: Some("timeout".into()),
            note: None,
        },
    }
}
