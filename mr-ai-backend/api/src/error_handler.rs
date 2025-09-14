use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use project_code_store::errors::GitCloneError;
use serde::Serialize;
use thiserror::Error;

use crate::core::app_state::ConfigError;

/// Public application error type.
#[derive(Debug, Error)]
pub enum AppError {
    // --- Boot / config ---
    #[error("missing required environment variable: {0}")]
    MissingEnv(&'static str),

    #[error(transparent)]
    Config(#[from] ConfigError),

    // --- IO / network / server ---
    #[error("failed to bind listener")]
    Bind(#[source] std::io::Error),

    #[error("server error")]
    Server(#[source] std::io::Error),

    // --- Request / routing ---
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("not found")]
    NotFound,
}

impl AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            // 4xx
            AppError::MissingEnv(_) => StatusCode::INTERNAL_SERVER_ERROR, // startup-only
            AppError::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,     // startup-only
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::NotFound => StatusCode::NOT_FOUND,

            // 5xx
            AppError::Bind(_) | AppError::Server(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_code(&self) -> &'static str {
        match self {
            AppError::MissingEnv(_) => "MISSING_ENV",
            AppError::Config(_) => "CONFIG_ERROR",
            AppError::Bind(_) => "BIND_ERROR",
            AppError::Server(_) => "SERVER_ERROR",
            AppError::BadRequest(_) => "BAD_REQUEST",
            AppError::NotFound => "NOT_FOUND",
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorBody {
            error: self.error_code(),
            message: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

/// Handy result alias used across handlers.
pub type AppResult<T> = Result<T, AppError>;

/// Optional: convert common Axum rejections to `AppError`.
impl From<axum::extract::rejection::JsonRejection> for AppError {
    fn from(err: axum::extract::rejection::JsonRejection) -> Self {
        AppError::BadRequest(err.to_string())
    }
}

impl From<axum::extract::rejection::QueryRejection> for AppError {
    fn from(err: axum::extract::rejection::QueryRejection) -> Self {
        AppError::BadRequest(err.to_string())
    }
}

impl From<GitCloneError> for AppError {
    fn from(err: GitCloneError) -> Self {
        use std::io;
        match err {
            GitCloneError::Io(e) => AppError::Server(e),
            GitCloneError::Join(e) => AppError::Server(io::Error::new(
                io::ErrorKind::Other,
                format!("join error: {e}"),
            )),
            GitCloneError::Git(msg) => AppError::Server(io::Error::new(
                io::ErrorKind::Other,
                format!("git error: {msg}"),
            )),
        }
    }
}
