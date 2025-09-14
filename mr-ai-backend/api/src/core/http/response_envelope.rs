use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Universal response envelope for both success and error (simplified).
#[derive(Serialize)]
pub struct ApiResponse<T>
where
    T: Serialize,
{
    pub success: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

#[derive(Serialize)]
pub struct ApiError {
    /// Stable, machine-readable error code (e.g. "BAD_REQUEST").
    pub code: &'static str,
    /// Human-friendly error message.
    pub message: String,
    /// Optional fine-grained error details (per-field, hints, etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<ApiErrorDetail>,
}

#[derive(Serialize)]
pub struct ApiErrorDetail {
    /// Field path like `urls` or `items[2].name`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Optional hint to help the client fix the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl<T> ApiResponse<T>
where
    T: Serialize,
{
    /// Build a success envelope.
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    /// Build an error envelope.
    pub fn error(
        code: &'static str,
        message: impl Into<String>,
        details: Vec<ApiErrorDetail>,
    ) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(ApiError {
                code,
                message: message.into(),
                details,
            }),
        }
    }

    /// Convert to axum Response.
    pub fn into_response_with_status(self, status: StatusCode) -> Response {
        (status, Json(self)).into_response()
    }
}
