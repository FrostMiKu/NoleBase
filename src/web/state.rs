//! Shared handler state for the web layer: only the handles handlers need,
//! never the whole [`crate::core::nole::Nole`] bootstrap. The watcher-driven
//! event hub and the core agent session live here as presentation-layer state.

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::core::agent_service::AgentService;
use crate::core::agent_session::{AgentConversation, AgentSession};
use crate::core::workspace::Workspace;
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
    pub session: Arc<AgentSession>,
    pub workspace_events: broadcast::Sender<WebWorkspaceEvent>,
}

impl WebState {
    pub fn new(workspace: Arc<Workspace>, agent: Arc<AgentService>) -> Self {
        let (workspace_events, _) = broadcast::channel(WORKSPACE_EVENT_BUFFER);
        let session = AgentSession::new(agent.clone(), AgentConversation::default(), true);
        Self {
            workspace,
            agent,
            session,
            workspace_events,
        }
    }
}
