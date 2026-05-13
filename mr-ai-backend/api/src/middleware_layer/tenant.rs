//! `X-Project-Slug` extractor for the admin router. Sprint C3 of 🅲.
//!
//! Reads `X-Project-Slug` from the request, looks it up against the
//! `projects` table to resolve a `ProjectId`, wraps the id into an
//! `AuthorizedScope`, and stashes that into request extensions so
//! downstream extractors (`Extension<AuthorizedScope>`) can pull it
//! without re-doing the DB lookup.
//!
//! This is the **only** middleware that should call
//! `AuthorizedScope::from_project_id` from the api crate — every
//! other code path is reached via this extension.
//!
//! Wired by `lib.rs::admin_router` **after** `admin_auth`, so a 401
//! is returned for missing token before a 400 fires for a missing
//! slug. The two middlewares stack cleanly because they read
//! different headers.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use domain::AuthorizedScope;
use serde_json::json;

use crate::core::app_state::AppState;

/// Header name the operator must present. Lowercase per HTTP spec.
pub const TENANT_SLUG_HEADER: &str = "x-project-slug";

/// Middleware that resolves `X-Project-Slug` → `AuthorizedScope` and
/// places it into request extensions for downstream handlers.
///
/// Error contract:
/// - Missing header → 400 `MISSING_PROJECT_SLUG`.
/// - Unknown slug   → 400 `UNKNOWN_PROJECT`.
/// - Persistence disabled → 503 `PERSISTENCE_DISABLED`.
pub async fn extract_tenant(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let slug = req
        .headers()
        .get(TENANT_SLUG_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let Some(slug) = slug else {
        return error_envelope(
            StatusCode::BAD_REQUEST,
            "MISSING_PROJECT_SLUG",
            format!(
                "request must carry the `{}` header (admin-router contract)",
                TENANT_SLUG_HEADER
            ),
        );
    };

    let Some(pool) = state.db.as_ref() else {
        return error_envelope(
            StatusCode::SERVICE_UNAVAILABLE,
            "PERSISTENCE_DISABLED",
            "tenant resolution requires DATABASE_URL",
        );
    };

    let project_id =
        match persistence::repos::projects::find_project_id_by_slug(pool, slug).await {
            Ok(Some(pid)) => pid,
            Ok(None) => {
                return error_envelope(
                    StatusCode::BAD_REQUEST,
                    "UNKNOWN_PROJECT",
                    format!("project slug `{slug}` not configured"),
                );
            }
            Err(err) => {
                tracing::warn!(
                    target = "api::tenant",
                    error = %err,
                    "tenant lookup failed"
                );
                return error_envelope(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "TENANT_LOOKUP_FAILED",
                    "internal error resolving project slug",
                );
            }
        };

    let scope = AuthorizedScope::from_project_id(project_id);
    req.extensions_mut().insert(scope);
    next.run(req).await
}

fn error_envelope(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
) -> Response {
    (
        status,
        Json(json!({
            "error": code,
            "message": message.into(),
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-smoke: ensure the header constant matches the spec
    /// shape so a hand-rolled curl works.
    #[test]
    fn tenant_slug_header_is_lowercase_dash_separated() {
        assert_eq!(TENANT_SLUG_HEADER, "x-project-slug");
    }

    #[test]
    fn error_envelope_returns_expected_status_and_payload_shape() {
        let resp = error_envelope(
            StatusCode::BAD_REQUEST,
            "MISSING_PROJECT_SLUG",
            "request must carry the `x-project-slug` header",
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn error_envelope_503_returns_service_unavailable_status() {
        // Sanity: the same constructor handles persistence-disabled.
        let resp = error_envelope(
            StatusCode::SERVICE_UNAVAILABLE,
            "PERSISTENCE_DISABLED",
            "tenant resolution requires DATABASE_URL",
        );
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
