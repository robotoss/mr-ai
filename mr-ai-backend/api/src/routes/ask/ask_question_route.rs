//! POST /ask_question — asks the LLM with RAG context.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};

use contextor::{AskOptions, QaAnswer, ask_with_opts};

use crate::{
    core::app_state::AppState,
    routes::ask::ask_request::{AskRequest, AskResponse, CtxItem},
};

/// Handler: POST /ask_question
///
/// # Example
/// ```bash
/// curl -X POST http://127.0.0.1:8080/ask_question \
///   -H 'content-type: application/json' \
///   -d '{"question":"Where is gamesIcon defined?","top_k":8,"context_k":5}'
/// ```
pub async fn ask_question(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AskRequest>,
) -> Result<Json<AskResponse>, (StatusCode, String)> {
    // Build AskOptions (fallback to env if client omits values)
    let mut opts = AskOptions::default();
    if let Some(k) = body.top_k {
        opts.top_k = k;
    }
    if let Some(k) = body.context_k {
        opts.context_k = k;
    }

    // Delegate to contextor (RAG + LLM)
    let QaAnswer { answer, context } =
        ask_with_opts(state.llm_profiles.clone(), &body.question, opts)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;

    // Map to API response DTOs
    let items = context
        .into_iter()
        .map(|u| CtxItem {
            score: u.score,
            source: u.source,
            fqn: u.fqn,
            kind: u.kind,
            preview: u.text,
        })
        .collect();

    Ok(Json(AskResponse {
        answer,
        context: items,
    }))
}
