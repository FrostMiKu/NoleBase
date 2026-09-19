//! Shared handler state for the web layer: only the handles handlers need,
//! never the whole [`crate::core::nole::Nole`] bootstrap. The watcher-driven
//! event hub, the core agent session, and the web backend's copy of the
//! session transcript live here as presentation-layer state.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::broadcast;

use crate::core::agent::AgentEvent;
use crate::core::agent_panel::AgentPanel;
use crate::core::agent_service::AgentService;
use crate::core::agent_session::{AgentPanelEntry, AgentSession, AgentSessionScope, TokenUsage};
use crate::core::storage::Storage;
use crate::core::workspace::Workspace;
use crate::web::dto::WebWorkspaceEvent;

/// Broadcast capacity for workspace events. Workspace bursts are small (a
/// save touches a handful of paths); slow consumers see
/// [`WebWorkspaceEvent::ResyncRequired`] instead of blocking the hub.
const WORKSPACE_EVENT_BUFFER: usize = 256;

/// The web backend's own copy of the agent session transcript, restored from
/// the persisted session at startup and kept current by the session
/// recorder. This is what the TUI keeps as `agent_panel` plus the usage
/// counters its persistence writes — the display half of
/// [`AgentSessionScope`], so a web run and a TUI run leave the same
/// `session.json` behind.
pub struct SessionTranscript {
    panel: AgentPanel,
    usage: TokenUsage,
    timed_output_tokens: u64,
    response_duration: Duration,
}

impl SessionTranscript {
    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Usage(usage) => self.usage.add(*usage),
            AgentEvent::ResponseTiming {
                output_tokens,
                elapsed,
            } => {
                self.timed_output_tokens = self.timed_output_tokens.saturating_add(*output_tokens);
                self.response_duration = self.response_duration.saturating_add(*elapsed);
            }
            _ => {}
        }
        self.panel.observe(event);
    }
}

/// Handler-facing application state. Cloned into every request; the watcher
/// background task holds only a clone of `workspace_events`.
#[derive(Clone)]
pub struct WebState {
    pub workspace: Arc<Workspace>,
    pub agent: Arc<AgentService>,
    pub session: Arc<AgentSession>,
    transcript: Arc<Mutex<SessionTranscript>>,
    pub workspace_events: broadcast::Sender<WebWorkspaceEvent>,
}

impl WebState {
    /// Build the state over a restored session scope (an empty scope means a
    /// fresh session). The persisted conversation continues the agent's
    /// context; the persisted panel seeds the visible transcript.
    pub fn new(
        workspace: Arc<Workspace>,
        agent: Arc<AgentService>,
        restored: AgentSessionScope,
    ) -> Self {
        let (workspace_events, _) = broadcast::channel(WORKSPACE_EVENT_BUFFER);
        let (conversation, panel, usage, timed_output_tokens, response_duration) =
            restored.into_parts();
        let session = AgentSession::new(agent.clone(), conversation, true);
        Self {
            workspace,
            agent,
            session,
            transcript: Arc::new(Mutex::new(SessionTranscript {
                panel: AgentPanel::from_entries(panel),
                usage,
                timed_output_tokens,
                response_duration,
            })),
            workspace_events,
        }
    }

    fn lock_transcript(&self) -> std::sync::MutexGuard<'_, SessionTranscript> {
        self.transcript
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Echo a user prompt into the transcript. Pushed before the run starts
    /// so it precedes the run's events in the panel.
    pub fn push_agent_prompt(&self, text: String) {
        self.lock_transcript().panel.push_prompt(text, false);
    }

    /// Roll back a prompt echo whose run start was rejected.
    pub fn pop_agent_prompt(&self) -> bool {
        self.lock_transcript().panel.pop_prompt()
    }

    /// Push a terminal error row (cancellation notice).
    pub fn push_agent_error(&self, text: &str) {
        self.lock_transcript().panel.push_error(text);
    }

    /// The settled transcript entries, for `GET /api/agent/history`.
    pub fn history_entries(&self) -> Vec<AgentPanelEntry> {
        self.lock_transcript().panel.snapshot()
    }

    /// Apply one agent event to the transcript. Called by the session
    /// recorder for every event on the shared stream.
    ///
    /// Conversation updates are applied to the session before persisting —
    /// the supervisor thread syncs the session's copy asynchronously, and
    /// the recorder's stream order must be what the persisted scope
    /// reflects (the same ordering rule the TUI event loop encodes).
    pub fn record_agent_event(&self, event: &AgentEvent, storage: &Storage) {
        if let AgentEvent::ConversationUpdated(conversation) = event {
            self.session.replace_conversation(conversation.clone());
        }
        self.lock_transcript().observe(event);
        if matches!(
            event,
            AgentEvent::ConversationUpdated(_) | AgentEvent::Finished(_) | AgentEvent::Stopped(_)
        ) {
            if let Err(error) = self.persist_agent_session(storage) {
                eprintln!("[nole web] agent session save error: {error:#}");
            }
        }
    }

    /// Persist the session scope from the current transcript state.
    pub fn persist_agent_session(&self, storage: &Storage) -> anyhow::Result<()> {
        let transcript = self.lock_transcript();
        let session = AgentSessionScope::from_parts(
            &self.session.conversation(),
            &transcript.panel.snapshot(),
            transcript.usage,
            transcript.timed_output_tokens,
            transcript.response_duration,
        );
        storage.write_agent_session(&session)
    }
}
