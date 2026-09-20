//! The web layer's stable JSON error model. Every failed handler responds
//! with `{"error":{"code","message"}}`; internal failures are logged with
//! their full chain server-side and answer with a generic message so
//! backtraces and absolute local paths never reach the browser.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::core::model::MutationConflict;
use crate::core::workspace::Workspace;
use crate::web::dto::{scrub_workspace_paths, HealthDto};

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    /// The workspace index has not published its first snapshot yet.
    IndexNotReady,
    Internal(anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, "bad_request", message),
            ApiError::NotFound(message) => (StatusCode::NOT_FOUND, "not_found", message),
            ApiError::Conflict(message) => (StatusCode::CONFLICT, "conflict", message),
            ApiError::IndexNotReady => (
                StatusCode::SERVICE_UNAVAILABLE,
                "index_not_ready",
                "The workspace index is still building; retry shortly".to_string(),
            ),
            ApiError::Internal(error) => {
                // Full context stays in the server log; the response is a
                // stable generic message.
                eprintln!("[nole web] internal error: {error:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Internal server error".to_string(),
                )
            }
        };
        let body = json!({"error": {"code": code, "message": message}});
        (status, Json(body)).into_response()
    }
}

impl ApiError {
    /// Classify a core document error for the HTTP boundary: a missing file
    /// (io NotFound anywhere in the chain) is a stable 404, a domain-raised
    /// conflict marker or a create collision (io AlreadyExists) is 409, pure
    /// validation failures are 400, and everything else is internal.
    pub fn from_document_error(error: anyhow::Error) -> Self {
        if let Some(conflict) = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<MutationConflict>())
        {
            return Self::Conflict(conflict.0.clone());
        }
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
        }) {
            return Self::NotFound("Document not found".to_string());
        }
        if error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::AlreadyExists)
        }) {
            return Self::Conflict("A document with that name already exists".to_string());
        }
        if error
            .chain()
            .any(|cause| cause.downcast_ref::<std::io::Error>().is_some())
        {
            return Self::Internal(error);
        }
        Self::BadRequest(error.to_string())
    }

    /// Classify like [`ApiError::from_document_error`], then scrub
    /// workspace-root prefixes from client-facing messages: core validation
    /// text names absolute paths, and the wire must stay workspace-relative.
    pub fn from_workspace_error(workspace: &Workspace, error: anyhow::Error) -> Self {
        match Self::from_document_error(error) {
            Self::BadRequest(message) => {
                Self::BadRequest(scrub_workspace_paths(workspace, &message))
            }
            Self::Conflict(message) => Self::Conflict(scrub_workspace_paths(workspace, &message)),
            other => other,
        }
    }

    pub fn internal(error: impl Into<anyhow::Error>) -> Self {
        Self::Internal(error.into())
    }
}

/// The `GET /` greeting: plain text, no HTML UI in this phase.
pub fn index_response() -> &'static str {
    "Nole Web API"
}

/// The `GET /api/health` payload; the version comes from the package
/// manifest, not a copied string.
pub fn health_response() -> Json<HealthDto> {
    Json(HealthDto {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
    })
}
