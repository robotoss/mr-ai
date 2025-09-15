use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Universal response envelope for both success and error (compact variant).
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
    /// Optional field-level details or hints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<ApiErrorDetail>,
}

#[derive(Serialize)]
pub struct ApiErrorDetail {
    /// JSON path or field name (e.g., "urls", "items[2].name").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Hint to help the client fix the request.
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

    /// Convert to an Axum Response and mark it as already formatted,
    /// so error middleware does not wrap it again.
    pub fn into_response_with_status(self, status: StatusCode) -> Response {
        let mut res = (status, Json(&self)).into_response();
        res.headers_mut()
            .insert("X-Api-Envelope", HeaderValue::from_static("1"));
        if !self.success {
            res.headers_mut()
                .insert("X-Api-Error-Formatted", HeaderValue::from_static("1"));
        }
        res
    }
}
