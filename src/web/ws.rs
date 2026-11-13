//! WebSocket endpoints: the event half of the web API (`HTTP = command,
//! WebSocket = event`). Every connection owns its broadcast receiver; a slow
//! consumer receives a `resync_required` marker and keeps consuming the
//! latest events instead of dragging the producer down.

use crate::web::error::{ApiError, ApiResult};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::core::agent::AgentEvent;
use crate::web::dto::{AgentHistoryEntryDto, WebAgentEvent, WebWorkspaceEvent};
use crate::web::state::WebState;

/// `GET /api/ws/workspace` — workspace change events.
pub async fn workspace_events(
    State(state): State<WebState>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| {
        forward_broadcast(socket, state.workspace_events.subscribe(), |event| {
            serde_json::to_string(&event).unwrap_or_else(|_| {
                serde_json::json!({
                    "type": "watcher_failed",
                    "message": "workspace event serialization failed"
                })
                .to_string()
            })
        })
    })
}

/// `GET /api/agents/{id}/events` — the shared agent event stream. The workspace handle
/// is only used to relativize paths in event DTOs.
pub async fn agent_events(
    State(state): State<WebState>,
    Path(id): Path<Uuid>,
    upgrade: WebSocketUpgrade,
) -> ApiResult<Response> {
    let session = state
        .sessions
        .get(id)
        .map_err(|_| ApiError::NotFound("Session not found".into()))?;
    let (live_panel, scope, receiver) = session.attach();
    let (conversation, panel, _, _, _) = scope.into_parts();
    let (approval, question, private_input) = session.pending_interactions();
    let interactions = [
        approval.map(AgentEvent::Approval),
        question.map(AgentEvent::AskUser),
        private_input.map(AgentEvent::PrivateTerminalInput),
    ]
    .into_iter()
    .flatten()
    .map(|event| WebAgentEvent::from_agent_event(&state.workspace, &event))
    .collect::<Vec<_>>();
    let snapshot = serde_json::json!({
        "type": "session_snapshot", "entries": panel.iter().map(AgentHistoryEntryDto::from).collect::<Vec<_>>(),
        "message_count": conversation.messages.len(), "live_tools": live_panel.live_tools(), "running": session.session.is_running(),
        "permission": session.service.permission_mode(), "interactions": interactions,
        "save_error": session.save_error(),
    }).to_string();
    Ok(upgrade.on_upgrade(move |mut socket| async move {
        if socket.send(Message::Text(snapshot.into())).await.is_err() {
            return;
        }
        let workspace = state.workspace.clone();
        forward_broadcast(socket, receiver, move |event: AgentEvent| {
            let web_event = WebAgentEvent::from_agent_event(&workspace, &event);
            serde_json::to_string(&web_event).unwrap_or_else(|_| {
                serde_json::json!({
                    "type": "notification",
                    "message": "agent event serialization failed"
                })
                .to_string()
            })
        })
        .await
    }))
}

/// Pump a broadcast receiver into a WebSocket as JSON text frames.
///
/// Lag policy: a `Lagged` receiver has missed `skipped` events; it receives
/// a `resync_required` marker and continues from the newest retained event.
/// The connection ends when the sender disappears, the socket errors, or the
/// client disconnects. Incoming frames are drained and ignored (the event
/// streams are one-way; commands stay on HTTP).
async fn forward_broadcast<T, F>(
    socket: WebSocket,
    mut receiver: broadcast::Receiver<T>,
    serialize: F,
) where
    T: Clone,
    F: Fn(T) -> String,
{
    let (mut sender, mut incoming) = socket.split();
    let mut drain = tokio::spawn(async move { while incoming.next().await.is_some() {} });
    loop {
        tokio::select! {
            result = receiver.recv() => match result {
                Ok(event) => {
                    if sender.send(Message::Text(serialize(event).into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    let marker = serde_json::json!({
                        "type": "resync_required",
                        "skipped": skipped,
                    })
                    .to_string();
                    if sender.send(Message::Text(marker.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = &mut drain => break,
        }
    }
    drain.abort();
}

/// Compile-time assertion that both event DTO families share the
/// `resync_required` marker shape used by the lag path above.
const _: fn(WebWorkspaceEvent) = |event| {
    matches!(event, WebWorkspaceEvent::ResyncRequired { .. });
};
const _: fn(WebAgentEvent) = |event| {
    matches!(event, WebAgentEvent::ResyncRequired { .. });
};
