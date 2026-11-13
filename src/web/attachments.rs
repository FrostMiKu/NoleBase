//! Browser access to managed attachments, addressed only by validated UUIDs.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, Request};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tower_http::services::ServeFile;

use crate::core::attachment::AttachmentId;

use super::error::{ApiError, ApiResult};
use super::state::WebState;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentQuery {
    #[serde(default)]
    download: bool,
}

pub async fn content(
    State(state): State<WebState>,
    Path(id): Path<String>,
    Query(query): Query<AttachmentQuery>,
    request: Request<Body>,
) -> ApiResult<Response> {
    let id = AttachmentId::parse(&id)
        .map_err(|_| ApiError::BadRequest("Invalid attachment ID".into()))?;
    // Store validation rejects symlinks, inconsistent metadata, and files over
    // the attachment limit. Never accept a filesystem path from the browser.
    let (path, metadata) = tokio::task::spawn_blocking(move || {
        let store = state.workspace.attachments();
        let metadata = store
            .lookup(id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::NotFound("Attachment not found".into()))?;
        let path = store.open(id).map_err(ApiError::internal)?;
        Ok::<_, ApiError>((path, metadata))
    })
    .await
    .map_err(ApiError::internal)??;

    // ServeFile streams the body and supports HEAD and byte ranges without
    // loading a potentially 256 MiB attachment into the web executor's memory.
    let mut response = ServeFile::new(path)
        .try_call(request)
        .await
        .map_err(ApiError::internal)?
        .into_response();
    let mime = metadata
        .mime_type
        .as_deref()
        .unwrap_or("application/octet-stream");
    let inline_image = matches!(
        mime,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/x-icon"
            | "image/svg+xml"
    );
    let disposition = if inline_image && !query.download {
        "inline"
    } else {
        "attachment"
    };
    let filename: String = metadata
        .display_name
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).map_err(ApiError::internal)?,
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "{disposition}; filename=\"attachment\"; filename*=UTF-8''{filename}"
        ))
        .map_err(ApiError::internal)?,
    );
    // Attachments are mutable and potentially active content. Even when
    // opened directly (e.g. SVG), they must never execute with the app origin.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'"),
    );
    Ok(response)
}
