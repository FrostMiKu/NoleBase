use crate::core::agent_sessions::SessionNoteRuntime;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use crate::core::agent::{
    ApprovalDecision, AskUserResponse, PermissionMode, PrivateTerminalInputDecision,
};
use crate::core::agent_session::AgentSessionStart;
use crate::web::dto::{AgentHistoryDto, AgentHistoryEntryDto};
use crate::web::error::{ApiError, ApiResult};
use crate::web::state::WebState;

/// `GET /api/agents/{id}` — session bootstrap state.
pub async fn status(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let runtime = session(&state, id)?;
    Ok(Json(json!({
        "running": runtime.session.is_running(),
        "session": runtime.summary(),
        "save_error": runtime.save_error(),
        "permission_mode": permission_mode_name(runtime.service.permission_mode()),
    })))
}

/// `GET /api/agents/{id}/history` — the restored session transcript so a
/// (re)connecting client can rebuild the turns that predate its stream.
pub async fn history(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AgentHistoryDto>> {
    let runtime = session(&state, id)?;
    let entries = runtime
        .history()
        .iter()
        .map(AgentHistoryEntryDto::from)
        .collect();
    let message_count = runtime.session.with_conversation(|c| c.messages.len());
    Ok(Json(AgentHistoryDto {
        entries,
        message_count,
    }))
}

/// `GET /api/agents/{id}/jobs` — every background job row, so a (re)connecting
/// client can rebuild the session-level job registry it missed events for.
pub async fn jobs(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let runtime = session(&state, id)?;
    let rows: Vec<serde_json::Value> = runtime
        .service
        .jobs()
        .rows()
        .iter()
        .map(|row| {
            json!({
                "id": row.id,
                "label": row.label,
                "status": row.status.as_str(),
            })
        })
        .collect();
    Ok(Json(json!({ "jobs": rows })))
}

#[derive(Deserialize)]
pub struct RunRequest {
    prompt: Option<String>,
}

/// `POST /api/agents/{id}/run` — accept a prompt; 202 means the agent took the
/// task, not that it finished. Output arrives over `/api/agents/{id}/events`.
pub async fn run(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
    Json(request): Json<RunRequest>,
) -> ApiResult<StatusCode> {
    let runtime = session(&state, id)?;
    let prompt = request
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| ApiError::BadRequest("prompt must not be empty".to_string()))?
        .to_string();
    // Echo the prompt before starting so it precedes the run's events in
    // the transcript; roll the echo back when the start is rejected.
    match runtime.start(prompt) {
        Ok(AgentSessionStart::Started(_)) => Ok(StatusCode::ACCEPTED),
        Ok(AgentSessionStart::AlreadyRunning) => Err(ApiError::Conflict(
            "an agent run is already active".to_string(),
        )),
        Err(error) => Err(ApiError::Internal(error)),
    }
}

/// `POST /api/agents/{id}/cancel` — idempotent.
pub async fn cancel(State(state): State<WebState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let runtime = session(&state, id)?;
    runtime.session.cancel();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ApprovalRequestDto {
    decision: Option<String>,
}

/// `POST /api/agents/{id}/approval` — answer the pending approval request. The
/// core keeps at most one pending request (same as the TUI overlay), so
/// there is no request id on the wire.
pub async fn approval(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
    Json(request): Json<ApprovalRequestDto>,
) -> ApiResult<StatusCode> {
    let runtime = session(&state, id)?;
    let decision = match request.decision.as_deref() {
        Some("approve") => ApprovalDecision::Approve,
        Some("deny") => ApprovalDecision::Deny,
        _ => {
            return Err(ApiError::BadRequest(
                "decision must be \"approve\" or \"deny\"".to_string(),
            ))
        }
    };
    runtime
        .service
        .respond_approval(decision)
        .map_err(ApiError::internal)?;
    runtime.clear_approval();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AskUserRequestDto {
    answer: Option<String>,
    #[serde(default)]
    cancel: bool,
}

/// `POST /api/agents/{id}/answer` — answer or cancel the pending ask-user request.
pub async fn answer(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
    Json(request): Json<AskUserRequestDto>,
) -> ApiResult<StatusCode> {
    let runtime = session(&state, id)?;
    let response = if request.cancel {
        AskUserResponse::Cancelled
    } else {
        match request.answer.as_deref() {
            Some(answer) if !answer.trim().is_empty() => {
                AskUserResponse::Answer(answer.to_string())
            }
            _ => {
                return Err(ApiError::BadRequest(
                    "answer must be a non-empty string, or cancel must be true".to_string(),
                ))
            }
        }
    };
    runtime
        .service
        .respond_user(response)
        .map_err(ApiError::internal)?;
    runtime.clear_question();
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct TerminalInputRequestDto {
    text: Option<String>,
    #[serde(default)]
    cancel: bool,
}

/// `POST /api/agents/{id}/terminal-input` — submit or cancel private terminal
/// input. The submitted text is moved through the core's private channel and
/// never enters the cloneable event stream.
pub async fn terminal_input(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
    Json(request): Json<TerminalInputRequestDto>,
) -> ApiResult<StatusCode> {
    let runtime = session(&state, id)?;
    let decision = if request.cancel {
        PrivateTerminalInputDecision::Cancelled
    } else {
        match request.text.as_deref() {
            Some(text) => {
                PrivateTerminalInputDecision::Submit(zeroize::Zeroizing::new(text.to_string()))
            }
            None => {
                return Err(ApiError::BadRequest(
                    "text must be a string, or cancel must be true".to_string(),
                ))
            }
        }
    };
    runtime
        .service
        .submit_private_terminal_input(decision)
        .map_err(ApiError::internal)?;
    runtime.clear_private_input();
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/agents/{id}/permission`
pub async fn get_permission(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let runtime = session(&state, id)?;
    Ok(Json(
        json!({ "mode": permission_mode_name(runtime.service.permission_mode()) }),
    ))
}

#[derive(Deserialize)]
pub struct PermissionRequestDto {
    mode: Option<String>,
}

/// `PUT /api/agents/{id}/permission`
pub async fn put_permission(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
    Json(request): Json<PermissionRequestDto>,
) -> ApiResult<Json<serde_json::Value>> {
    let runtime = session(&state, id)?;
    let mode = match request.mode.as_deref() {
        Some("approve") => PermissionMode::Approve,
        Some("auto") => PermissionMode::Auto,
        Some("yolo") => PermissionMode::Yolo,
        _ => {
            return Err(ApiError::BadRequest(
                "mode must be \"approve\", \"auto\", or \"yolo\"".to_string(),
            ))
        }
    };
    runtime.service.set_permission_mode(mode);
    runtime.persist().map_err(ApiError::internal)?;
    Ok(Json(json!({ "mode": permission_mode_name(mode) })))
}

fn permission_mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Approve => "approve",
        PermissionMode::Auto => "auto",
        PermissionMode::Yolo => "yolo",
    }
}

fn session(state: &WebState, id: Uuid) -> ApiResult<Arc<SessionNoteRuntime>> {
    state
        .sessions
        .get(id)
        .map_err(|_| ApiError::NotFound("Session not found".into()))
}

pub async fn list(State(state): State<WebState>) -> Json<serde_json::Value> {
    Json(json!({ "sessions": state.sessions.list() }))
}

#[derive(Deserialize)]
pub struct NameRequest {
    title: String,
}

pub async fn create(
    State(state): State<WebState>,
    Json(request): Json<NameRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let runtime = state
        .sessions
        .create(&request.title)
        .map_err(ApiError::from_document_error)?;
    Ok(Json(json!(runtime.summary())))
}

pub async fn rename(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
    Json(request): Json<NameRequest>,
) -> ApiResult<StatusCode> {
    session(&state, id)?
        .rename(&request.title)
        .map_err(ApiError::from_document_error)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(State(state): State<WebState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    session(&state, id)?;
    state.sessions.delete(id).map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn mark_read(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    session(&state, id)?
        .mark_read()
        .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}
