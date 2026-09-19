//! Workspace and document HTTP handlers. Each handler validates its input,
//! calls one [`Workspace`] method, and maps the result into a DTO.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::web::dto::{
    DailyNoteDto, DocumentDto, DocumentWriteRequest, NoteSummaryDto, SearchHitDto, TagDocumentDto,
    TagSummaryDto, TodoDto, WorkspaceOverviewDto,
};
use crate::web::error::{ApiError, ApiResult};
use crate::web::state::WebState;

pub async fn overview(State(state): State<WebState>) -> ApiResult<Json<WorkspaceOverviewDto>> {
    let dto = WorkspaceOverviewDto::from_workspace(&state.workspace).map_err(ApiError::Internal)?;
    Ok(Json(dto))
}

pub async fn notes(State(state): State<WebState>) -> ApiResult<Json<Vec<NoteSummaryDto>>> {
    let files = state.workspace.note_files().map_err(ApiError::Internal)?;
    Ok(Json(
        files
            .iter()
            .map(|file| NoteSummaryDto::from_note_file(&state.workspace, file))
            .collect(),
    ))
}

pub async fn daily(State(state): State<WebState>) -> ApiResult<Json<Vec<DailyNoteDto>>> {
    let notes = state.workspace.daily_notes().map_err(ApiError::Internal)?;
    Ok(Json(
        notes
            .iter()
            .map(|note| DailyNoteDto::from_daily_note(&state.workspace, note))
            .collect(),
    ))
}

pub async fn todos(State(state): State<WebState>) -> ApiResult<Json<Vec<TodoDto>>> {
    let items = state.workspace.todos().map_err(ApiError::Internal)?;
    Ok(Json(items.iter().map(TodoDto::from_todo_item).collect()))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
}

pub async fn search(
    State(state): State<WebState>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let term = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .ok_or_else(|| ApiError::BadRequest("missing search query".to_string()))?;
    let hits = state
        .workspace
        .search(term)
        .ok_or(ApiError::IndexNotReady)?;
    let hits: Vec<SearchHitDto> = hits
        .iter()
        .map(|hit| SearchHitDto::from_search_hit(&state.workspace, hit))
        .collect();
    Ok(Json(json!({ "query": term, "hits": hits })))
}

pub async fn tags(State(state): State<WebState>) -> ApiResult<Json<Vec<TagSummaryDto>>> {
    let tags = state.workspace.tags().ok_or(ApiError::IndexNotReady)?;
    Ok(Json(
        tags.iter().map(TagSummaryDto::from_tag_summary).collect(),
    ))
}

pub async fn tag_documents(
    State(state): State<WebState>,
    axum::extract::Path(tag): axum::extract::Path<String>,
) -> ApiResult<Json<Vec<TagDocumentDto>>> {
    let documents = state
        .workspace
        .tag_documents(&tag)
        .ok_or(ApiError::IndexNotReady)?;
    Ok(Json(
        documents
            .iter()
            .map(|document| TagDocumentDto::from_tag_document(&state.workspace, document))
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct DocumentQuery {
    path: Option<String>,
}

/// Read a managed document. The address is a workspace-relative path; the
/// core resolver owns traversal rejection, and io-level failures classify
/// into stable 404/500 responses.
pub async fn read_document(
    State(state): State<WebState>,
    Query(query): Query<DocumentQuery>,
) -> ApiResult<Json<DocumentDto>> {
    let path = document_path(&query)?;
    let source = state
        .workspace
        .read_document(&path)
        .map_err(ApiError::from_document_error)?;
    Ok(Json(DocumentDto { path, source }))
}

/// Write a managed document. Same addressing rules as the read side; the
/// write goes through the core atomic publication path and the watcher
/// pipeline refreshes indexes naturally.
pub async fn write_document(
    State(state): State<WebState>,
    Json(request): Json<DocumentWriteRequest>,
) -> ApiResult<StatusCode> {
    state
        .workspace
        .write_document(&request.path, &request.source)
        .map_err(ApiError::from_document_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Reject clearly-malformed document addresses before touching the core:
/// empty, absolute, or parent-directory addresses are client errors. The
/// core resolver remains the security authority for everything else.
fn document_path(query: &DocumentQuery) -> Result<String, ApiError> {
    let path = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| ApiError::BadRequest("missing document path".to_string()))?;
    if path.contains('\\') || std::path::Path::new(path).is_absolute() {
        return Err(ApiError::BadRequest(
            "document path must be workspace-relative".to_string(),
        ));
    }
    if std::path::Path::new(path)
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ApiError::BadRequest(
            "document path must not contain `..`".to_string(),
        ));
    }
    Ok(path.to_string())
}

/// Backlink sources for a document addressed by workspace-relative path.
/// Reuses the core resolver so unmanaged addresses never reach the index.
pub async fn backlinks(
    State(state): State<WebState>,
    Query(query): Query<DocumentQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let path = document_path(&query)?;
    let target = state
        .workspace
        .document_path(&path)
        .map_err(ApiError::from_document_error)?;
    let links = state
        .workspace
        .backlinks(&target)
        .ok_or(ApiError::IndexNotReady)?;
    let sources: Vec<String> = links
        .iter()
        .map(|source| {
            state
                .workspace
                .relative_path(source)
                .unwrap_or_else(|| relative_file_name(source))
        })
        .collect();
    Ok(Json(json!({ "path": path, "backlinks": sources })))
}

fn relative_file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}
