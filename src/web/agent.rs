//! Agent HTTP command handlers and the web-local agent session.
//!
//! HTTP carries commands; events flow only through the WebSocket stream.
//! The session holds the caller-owned [`AgentConversation`] that
//! [`AgentService::run`] requires — the same role the TUI app plays on its
//! side. Web and TUI sessions are separate processes and stay fully
//! independent; there is no web-side persistence.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::core::agent::{
    ApprovalDecision, AskUserResponse, PermissionMode, PrivateTerminalInputDecision,
};
use crate::core::agent_service::AgentService;
use crate::core::agent_session::AgentConversation;
use crate::web::error::{ApiError, ApiResult};
use crate::web::state::WebState;

/// The outcome of a run request: a run started, or one was already active.
pub enum RunOutcome {
    Started,
    AlreadyRunning,
}

/// Web-local agent session state: the conversation handle for continuation,
/// plus the active run's cancellation token. Cloned handlers share one
/// session.
#[derive(Clone, Default)]
pub struct WebAgentSession {
    inner: Arc<SessionInner>,
}

#[derive(Default)]
struct SessionInner {
    conversation: Mutex<AgentConversation>,
    run: Mutex<Option<CancellationToken>>,
}

impl WebAgentSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_running(&self) -> bool {
        self.inner.run.lock().is_ok_and(|run| run.is_some())
    }

    /// Accept a run on the shared agent service, remembering the active
    /// run's cancellation token. A detached task drives the run's output
    /// future so completion updates the session even with no WebSocket
    /// attached.
    pub fn start(&self, agent: &AgentService, prompt: String) -> anyhow::Result<RunOutcome> {
        let mut run = self
            .inner
            .run
            .lock()
            .map_err(|_| anyhow::anyhow!("agent session lock poisoned"))?;
        if run.is_some() {
            return Ok(RunOutcome::AlreadyRunning);
        }
        let conversation = self
            .inner
            .conversation
            .lock()
            .map_err(|_| anyhow::anyhow!("agent session lock poisoned"))?
            .clone();
        let observable = agent.run(prompt, conversation)?;
        *run = Some(observable.cancel.clone());
        drop(run);

        let inner = self.inner.clone();
        tokio::spawn(async move {
            let output = observable.output.await;
            if let Ok(output) = output {
                if let Ok(mut conversation) = inner.conversation.lock() {
                    *conversation = output.conversation;
                }
            }
            if let Ok(mut run) = inner.run.lock() {
                *run = None;
            }
        });
        Ok(RunOutcome::Started)
    }

    /// Cancel the active run, mirroring the TUI: signal the service-level
    /// flag and the run's own token. Idempotent.
    pub fn cancel(&self, agent: &AgentService) {
        agent.cancel();
        if let Ok(run) = self.inner.run.lock() {
            if let Some(cancel) = run.as_ref() {
                cancel.cancel();
            }
        }
    }
}

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
    match state
        .session
        .start(&state.agent, prompt)
        .map_err(ApiError::Internal)?
    {
        RunOutcome::Started => Ok(StatusCode::ACCEPTED),
        RunOutcome::AlreadyRunning => Err(ApiError::Conflict(
            "an agent run is already active".to_string(),
        )),
    }
}

/// `POST /api/agent/cancel` — idempotent.
pub async fn cancel(State(state): State<WebState>) -> StatusCode {
    state.session.cancel(&state.agent);
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
