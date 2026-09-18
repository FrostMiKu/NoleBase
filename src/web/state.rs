//! Shared handler state for the web layer: only the handles handlers need,
//! never the whole [`crate::core::nole::Nole`] bootstrap. The watcher-driven
//! event hub and the web agent session live here as presentation-layer state.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::core::agent_service::AgentService;
use crate::core::workspace::Workspace;
use crate::web::agent::WebAgentSession;
use crate::web::dto::WebWorkspaceEvent;

/// Broadcast capacity for workspace events. Workspace bursts are small (a
/// save touches a handful of paths); slow consumers see
/// [`WebWorkspaceEvent::ResyncRequired`] instead of blocking the hub.
const WORKSPACE_EVENT_BUFFER: usize = 256;

/// Handler-facing application state. Cloned into every request; the watcher
/// background task holds only a clone of `workspace_events`.
#[derive(Clone)]
pub struct WebState {
    pub workspace: Arc<Workspace>,
    pub agent: Arc<AgentService>,
    pub session: WebAgentSession,
    pub workspace_events: broadcast::Sender<WebWorkspaceEvent>,
}

impl WebState {
    pub fn new(workspace: Arc<Workspace>, agent: Arc<AgentService>) -> Self {
        let (workspace_events, _) = broadcast::channel(WORKSPACE_EVENT_BUFFER);
        Self {
            workspace,
            agent,
            session: WebAgentSession::new(),
            workspace_events,
        }
    }
}
