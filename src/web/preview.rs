//! Read-only rendering of editor snapshots through the shared export engine.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::dto::scrub_workspace_paths;
use super::error::{ApiError, ApiResult};
use super::state::WebState;

const MAX_PREVIEW_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_PREVIEW_HTML_BYTES: usize = 32 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    path: String,
    source: String,
}

#[derive(Serialize)]
pub struct PreviewResponse {
    html: String,
    warnings: Vec<String>,
}

pub async fn render(
    State(state): State<WebState>,
    Json(request): Json<PreviewRequest>,
) -> ApiResult<Json<PreviewResponse>> {
    if request.source.len() > MAX_PREVIEW_SOURCE_BYTES {
        return Err(ApiError::BadRequest("Preview source exceeds 1 MiB".into()));
    }
    // Parsing, local image decoding, and embedded runtimes never block the
    // HTTP/WebSocket executor. This operation does not write the document.
    tokio::task::spawn_blocking(move || {
        let rendered = state
            .workspace
            .render_document_preview(&request.path, &request.source)
            .map_err(|error| ApiError::from_workspace_error(&state.workspace, error))?;
        if rendered.bytes.len() > MAX_PREVIEW_HTML_BYTES {
            return Err(ApiError::BadRequest(
                "Rendered preview exceeds 32 MiB".into(),
            ));
        }
        let html =
            String::from_utf8(rendered.bytes).map_err(|error| ApiError::Internal(error.into()))?;
        // Diagnostics also appear in visible HTML image placeholders.
        let html = scrub_workspace_paths(&state.workspace, &html);
        let warnings = rendered
            .diagnostics
            .iter()
            .map(|warning| scrub_workspace_paths(&state.workspace, &warning.message))
            .collect();
        Ok(Json(PreviewResponse { html, warnings }))
    })
    .await
    .map_err(|error| ApiError::Internal(error.into()))?
}
