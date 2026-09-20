//! Workspace and document HTTP handlers. Each handler validates its input,
//! calls one [`Workspace`] method, and maps the result into a DTO.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::web::dto::relative_path;
use crate::web::dto::{
    CreateNoteRequest, DailyNoteDto, DocumentDto, DocumentMutationDto, DocumentPathRequest,
    DocumentWriteRequest, MoveDailyRequest, NoteSummaryDto, RenameDocumentRequest, SearchHitDto,
    TagDocumentDto, TagSummaryDto, TodoDto, WorkspaceOverviewDto,
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

/// `POST /api/todos/{index}/toggle` — flip the completion state of the
/// indexed daily task and answer with the refreshed list so the client
/// needs no second round trip.
pub async fn toggle_todo(
    State(state): State<WebState>,
    axum::extract::Path(index): axum::extract::Path<usize>,
) -> ApiResult<Json<Vec<TodoDto>>> {
    let toggled = state
        .workspace
        .toggle_todo_task(index)
        .map_err(ApiError::Internal)?;
    if !toggled {
        return Err(ApiError::NotFound(format!("no task at index {index}")));
    }
    let items = state.workspace.todos().map_err(ApiError::Internal)?;
    Ok(Json(items.iter().map(TodoDto::from_todo_item).collect()))
}

#[derive(Deserialize)]
pub struct DailyAppendRequest {
    body: Option<String>,
}

/// `POST /api/daily/today` — append content to today's daily note,
/// creating it on first send. Returns the updated note.
pub async fn append_to_today(
    State(state): State<WebState>,
    Json(request): Json<DailyAppendRequest>,
) -> ApiResult<Json<DailyNoteDto>> {
    let body = request
        .body
        .as_deref()
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .ok_or_else(|| ApiError::BadRequest("daily content must not be empty".to_string()))?;
    let note = state
        .workspace
        .append_to_today(body)
        .map_err(ApiError::Internal)?;
    Ok(Json(DailyNoteDto::from_daily_note(&state.workspace, &note)))
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

// -----------------------------------------------------------------------
// Document lifecycle mutations. Each handler does the thinnest dispatch on
// the managed directory and delegates every rule — naming, transitions,
// collisions, wikilink rewriting, index sync — to one Workspace method; no
// handler ever touches the filesystem itself.
// -----------------------------------------------------------------------

/// Map a mutation failure through the workspace-aware classifier so domain
/// conflicts surface as 409 and validation text never carries absolute
/// local paths.
fn mutation_error(state: &WebState, error: anyhow::Error) -> ApiError {
    ApiError::from_workspace_error(&state.workspace, error)
}

/// Resolve a workspace-relative document address, classifying lexical
/// rejection (traversal, unknown directory, wrong shape) as client errors.
fn resolve_document(state: &WebState, path: &str) -> Result<std::path::PathBuf, ApiError> {
    state
        .workspace
        .document_path(path)
        .map_err(|error| mutation_error(state, error))
}

/// `POST /api/notes` — create a note in `data/`, seeded with the default
/// heading or the workspace template. Answers 201 with the canonical path
/// the workspace produced; clients never guess extensions or directories.
pub async fn create_note(
    State(state): State<WebState>,
    Json(request): Json<CreateNoteRequest>,
) -> ApiResult<(StatusCode, Json<DocumentMutationDto>)> {
    let path = if request.from_template {
        state.workspace.create_note_from_template(&request.name)
    } else {
        state.workspace.create_note(&request.name)
    }
    .map_err(|error| mutation_error(&state, error))?;
    Ok((
        StatusCode::CREATED,
        Json(DocumentMutationDto {
            path: relative_path(&state.workspace, &path),
        }),
    ))
}

/// `POST /api/document/rename` — rename a note or archived note in place.
/// The thinnest dispatch on the managed directory picks the flavor; daily
/// addresses reach `rename_note`, whose group check refuses them.
pub async fn rename_document(
    State(state): State<WebState>,
    Json(request): Json<RenameDocumentRequest>,
) -> ApiResult<Json<DocumentMutationDto>> {
    let from = resolve_document(&state, &request.path)?;
    let to = if request.path.starts_with("archives/") {
        state
            .workspace
            .rename_archived_note(&from, &request.new_name)
    } else {
        state.workspace.rename_note(&from, &request.new_name)
    }
    .map_err(|error| mutation_error(&state, error))?;
    Ok(Json(DocumentMutationDto {
        path: relative_path(&state.workspace, &to),
    }))
}

/// `POST /api/document/archive` — move a `data/` note into `archives/`.
/// Non-note inputs (archives, daily) are refused by the workspace as client
/// errors; wikilinks are untouched because the resolver matches by name.
pub async fn archive_document(
    State(state): State<WebState>,
    Json(request): Json<DocumentPathRequest>,
) -> ApiResult<Json<DocumentMutationDto>> {
    let from = resolve_document(&state, &request.path)?;
    let to = state
        .workspace
        .archive_note(&from)
        .map_err(|error| mutation_error(&state, error))?;
    Ok(Json(DocumentMutationDto {
        path: relative_path(&state.workspace, &to),
    }))
}

/// `POST /api/document/restore` — move an `archives/` note back into
/// `data/`. Only archive inputs succeed; the workspace refuses the rest.
pub async fn restore_document(
    State(state): State<WebState>,
    Json(request): Json<DocumentPathRequest>,
) -> ApiResult<Json<DocumentMutationDto>> {
    let from = resolve_document(&state, &request.path)?;
    let to = state
        .workspace
        .restore_note(&from)
        .map_err(|error| mutation_error(&state, error))?;
    Ok(Json(DocumentMutationDto {
        path: relative_path(&state.workspace, &to),
    }))
}

/// `DELETE /api/document?path=...` — delete a note or archived note. Daily
/// documents are deleted by date through `DELETE /api/daily/{date}` and are
/// refused here by the workspace.
pub async fn delete_document(
    State(state): State<WebState>,
    Query(query): Query<DocumentQuery>,
) -> ApiResult<StatusCode> {
    let path = document_path(&query)?;
    let target = resolve_document(&state, &path)?;
    state
        .workspace
        .delete_note(&target)
        .map_err(|error| mutation_error(&state, error))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/daily/{date}` — delete one dated daily note. Dates use the
/// workspace's own `YYYY-MM-DD` parsing; an unknown date is a stable 404.
pub async fn delete_daily(
    State(state): State<WebState>,
    Path(date): Path<String>,
) -> ApiResult<StatusCode> {
    let removed = state
        .workspace
        .delete_daily_note(&date)
        .map_err(|error| mutation_error(&state, error))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound("Daily note not found".to_string()))
    }
}

/// `POST /api/daily/{date}/move` — file one dated daily note into a managed
/// note: its body is appended as a dated section and the daily file removed,
/// exactly the TUI's move flow. Only `data/` targets are legal; the
/// workspace's own validation refuses everything else.
pub async fn move_daily(
    State(state): State<WebState>,
    Path(date): Path<String>,
    Json(request): Json<MoveDailyRequest>,
) -> ApiResult<Json<DocumentMutationDto>> {
    let note = state
        .workspace
        .daily_note(&date)
        .map_err(|error| mutation_error(&state, error))?;
    let target = resolve_document(&state, &request.target_path)?;
    state
        .workspace
        .move_daily_to_note(&target, &note)
        .map_err(|error| mutation_error(&state, error))?;
    Ok(Json(DocumentMutationDto {
        path: relative_path(&state.workspace, &target),
    }))
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
