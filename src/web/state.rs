//! Shared workspace and Session note services for HTTP and WebSocket handlers.
use crate::core::agent_sessions::AgentSessions;
use crate::core::workspace::Workspace;
use crate::web::dto::WebWorkspaceEvent;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct WebState {
    pub workspace: Arc<Workspace>,
    pub sessions: Arc<AgentSessions>,
    pub workspace_events: broadcast::Sender<WebWorkspaceEvent>,
}

impl WebState {
    pub fn new(workspace: Arc<Workspace>, sessions: Arc<AgentSessions>) -> Self {
        let (workspace_events, _) = broadcast::channel(256);
        Self {
            workspace,
            sessions,
            workspace_events,
        }
    }
}
