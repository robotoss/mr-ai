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

    /// Rich HTTP error mapped from lower layers with specific status & code.
    #[error("{message}")]
    Http {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
}

impl AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            // 4xx
            AppError::MissingEnv(_) => StatusCode::INTERNAL_SERVER_ERROR, // startup-only
            AppError::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,     // startup-only
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::NotFound => StatusCode::NOT_FOUND,

            // custom mapped
            AppError::Http { status, .. } => *status,

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
            AppError::Http { code, .. } => code,
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

/// Convert `GitCloneError` to `AppError::Http` with precise HTTP status & code.
/// Uses text heuristics to avoid importing `git2` types here.
impl From<GitCloneError> for AppError {
    fn from(err: GitCloneError) -> Self {
        match err {
            GitCloneError::Io(e) => AppError::Http {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "IO_ERROR",
                message: format!("Filesystem error occurred during cloning: {e}"),
            },
            GitCloneError::Join(e) => AppError::Http {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "JOIN_ERROR",
                message: format!("Background task failed to complete: {e}"),
            },
            GitCloneError::Git(e) => {
                let msg = e.to_string();
                let lower = msg.to_lowercase();

                // Heuristics for common remote/auth failures.
                if lower.contains("auth")
                    || lower.contains("unauthorized")
                    || lower.contains("permission")
                    || lower.contains("denied")
                {
                    AppError::Http {
                        status: StatusCode::UNAUTHORIZED,
                        code: "UNAUTHORIZED",
                        message: "The project you were looking for could not be found or you don't have permission to view it.".into(),
                    }
                } else if lower.contains("not found")
                    || lower.contains("could not be found")
                    || lower.contains("repository not found")
                {
                    AppError::Http {
                        status: StatusCode::NOT_FOUND,
                        code: "REPO_NOT_FOUND",
                        message: "Repository not found or not accessible.".into(),
                    }
                } else if lower.contains("ssl") || lower.contains("tls") {
                    AppError::Http {
                        status: StatusCode::BAD_GATEWAY,
                        code: "TLS_ERROR",
                        message: "TLS/SSL error while communicating with the remote.".into(),
                    }
                } else if lower.contains("remote")
                    || lower.contains("network")
                    || lower.contains("eof")
                    || lower.contains("early eof")
                {
                    AppError::Http {
                        status: StatusCode::BAD_GATEWAY,
                        code: "GIT_REMOTE_ERROR",
                        message: "Remote error or network issue occurred during cloning.".into(),
                    }
                } else {
                    AppError::Http {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        code: "GIT_ERROR",
                        message: format!("Git operation failed: {msg}"),
                    }
                }
            }
        }
    }
}
