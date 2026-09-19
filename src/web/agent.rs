//! Agent HTTP command handlers over the shared core [`AgentSession`].
//!
//! HTTP carries commands; events flow only through the WebSocket stream.
//! Session semantics — conversation ownership, run lifecycle, and idle
//! background-job delivery — are a core runtime capability shared with the
//! TUI; this layer only forwards commands. Web and TUI sessions are
//! separate processes and stay fully independent; there is no web-side
//! persistence.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::core::agent::{
    ApprovalDecision, AskUserResponse, PermissionMode, PrivateTerminalInputDecision,
};
use crate::core::agent_session::AgentSessionStart;
use crate::web::error::{ApiError, ApiResult};
use crate::web::state::WebState;

/// `GET /api/agent` — session bootstrap state.
pub async fn status(State(state): State<WebState>) -> Json<serde_json::Value> {
    Json(json!({
        "running": state.session.is_running(),
        "permission_mode": permission_mode_name(state.agent.permission_mode()),
    }))
}

#[derive(Deserialize)]
pub struct RunRequest {
    prompt: Option<String>,
}

/// `POST /api/agent/run` — accept a prompt; 202 means the agent took the
/// task, not that it finished. Output arrives over `/api/ws/agent`.
pub async fn run(
    State(state): State<WebState>,
    Json(request): Json<RunRequest>,
) -> ApiResult<StatusCode> {
    let prompt = request
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| ApiError::BadRequest("prompt must not be empty".to_string()))?
        .to_string();
    match state.session.start(prompt).map_err(ApiError::Internal)? {
        AgentSessionStart::Started(_) => Ok(StatusCode::ACCEPTED),
        AgentSessionStart::AlreadyRunning => Err(ApiError::Conflict(
            "an agent run is already active".to_string(),
        )),
    }
}

/// `POST /api/agent/cancel` — idempotent.
pub async fn cancel(State(state): State<WebState>) -> StatusCode {
    state.session.cancel();
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
pub struct ApprovalRequestDto {
    decision: Option<String>,
}

/// `POST /api/agent/approval` — answer the pending approval request. The
/// core keeps at most one pending request (same as the TUI overlay), so
/// there is no request id on the wire.
pub async fn approval(
    State(state): State<WebState>,
    Json(request): Json<ApprovalRequestDto>,
) -> ApiResult<StatusCode> {
    let decision = match request.decision.as_deref() {
        Some("approve") => ApprovalDecision::Approve,
        Some("deny") => ApprovalDecision::Deny,
        _ => {
            return Err(ApiError::BadRequest(
                "decision must be \"approve\" or \"deny\"".to_string(),
            ))
        }
    };
    state
        .agent
        .respond_approval(decision)
        .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AskUserRequestDto {
    answer: Option<String>,
    #[serde(default)]
    cancel: bool,
}

/// `POST /api/agent/answer` — answer or cancel the pending ask-user request.
pub async fn answer(
    State(state): State<WebState>,
    Json(request): Json<AskUserRequestDto>,
) -> ApiResult<StatusCode> {
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
    state
        .agent
        .respond_user(response)
        .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct TerminalInputRequestDto {
    text: Option<String>,
    #[serde(default)]
    cancel: bool,
}

/// `POST /api/agent/terminal-input` — submit or cancel private terminal
/// input. The submitted text is moved through the core's private channel and
/// never enters the cloneable event stream.
pub async fn terminal_input(
    State(state): State<WebState>,
    Json(request): Json<TerminalInputRequestDto>,
) -> ApiResult<StatusCode> {
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
    state
        .agent
        .submit_private_terminal_input(decision)
        .map_err(ApiError::internal)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/agent/permission`
pub async fn get_permission(State(state): State<WebState>) -> Json<serde_json::Value> {
    Json(json!({ "mode": permission_mode_name(state.agent.permission_mode()) }))
}

#[derive(Deserialize)]
pub struct PermissionRequestDto {
    mode: Option<String>,
}

/// `PUT /api/agent/permission`
pub async fn put_permission(
    State(state): State<WebState>,
    Json(request): Json<PermissionRequestDto>,
) -> ApiResult<Json<serde_json::Value>> {
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
    state.agent.set_permission_mode(mode);
    Ok(Json(json!({ "mode": permission_mode_name(mode) })))
}

fn permission_mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Approve => "approve",
        PermissionMode::Auto => "auto",
        PermissionMode::Yolo => "yolo",
    }
}
