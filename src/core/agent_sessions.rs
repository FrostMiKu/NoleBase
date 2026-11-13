//! Session notes and their independent runtimes, shared by every frontend.
mod store;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::core::agent::{AgentEvent, PermissionMode};
use crate::core::agent_panel::AgentPanel;
use crate::core::agent_service::AgentService;
use crate::core::agent_session::{
    AgentPanelEntry, AgentSession, AgentSessionScope, AgentSessionStart, TokenUsage,
};
use crate::core::workspace::Workspace;
use store::SessionNote;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: Uuid,
    pub title: String,
    pub updated_at: i64,
    pub running: bool,
    pub unread: bool,
}

struct SessionState {
    note: SessionNote,
    panel: AgentPanel,
    usage: TokenUsage,
    output_tokens: u64,
    duration: Duration,
    deleted: bool,
    retained: bool,
    approval: Option<crate::core::agent::ApprovalRequest>,
    question: Option<crate::core::agent::AskUserRequest>,
    private_input: Option<crate::core::agent::PrivateTerminalInputRequest>,
    save_error: Option<String>,
}

pub struct SessionNoteRuntime {
    root: PathBuf,
    pub session: Arc<AgentSession>,
    pub service: Arc<AgentService>,
    state: Mutex<SessionState>,
    events: broadcast::Sender<AgentEvent>,
}

impl SessionNoteRuntime {
    fn open(workspace: &Workspace, note: SessionNote, retained: bool) -> Arc<Self> {
        let root = workspace.storage().root.clone();
        let service = Arc::new(AgentService::in_directory(
            workspace,
            store::directory(&root, note.id),
        ));
        service.set_permission_mode(note.permission);
        let (conversation, panel, usage, output_tokens, duration) = note.scope.clone().into_parts();
        let session = AgentSession::recorded(service.clone(), conversation, true);
        let (events, _) = broadcast::channel(crate::core::agent::AGENT_STREAM_BUFFER);
        let mut receiver = service.subscribe();
        let runtime = Arc::new(Self {
            root,
            service,
            session,
            events,
            state: Mutex::new(SessionState {
                note,
                panel: AgentPanel::from_entries(panel),
                usage,
                output_tokens,
                duration,
                deleted: false,
                retained,
                approval: None,
                question: None,
                private_input: None,
                save_error: None,
            }),
        });
        let weak = Arc::downgrade(&runtime);
        std::thread::Builder::new()
            .name("session-note".into())
            .spawn(move || loop {
                let event = match receiver.blocking_recv() {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let Some(runtime) = weak.upgrade() else {
                    break;
                };
                runtime.observe(&event);
            })
            .expect("starting Session note recorder");
        runtime
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn save(&self, state: &mut SessionState) -> Result<()> {
        if state.deleted || !state.retained {
            return Ok(());
        }
        state.note.scope = AgentSessionScope::from_parts(
            &self.session.conversation(),
            &state.panel.snapshot(),
            state.usage,
            state.output_tokens,
            state.duration,
        );
        state.note.permission = self.service.permission_mode();
        state.note.run_active = self.session.is_running();
        let result = store::write(&self.root, &state.note);
        state.save_error = result.as_ref().err().map(|error| format!("{error:#}"));
        result
    }

    pub(crate) fn observe(&self, event: &AgentEvent) {
        let mut state = self.lock();
        if state.deleted {
            return;
        }
        match event {
            AgentEvent::Usage(usage) => state.usage.add(*usage),
            AgentEvent::ResponseTiming {
                output_tokens,
                elapsed,
            } => {
                state.output_tokens = state.output_tokens.saturating_add(*output_tokens);
                state.duration = state.duration.saturating_add(*elapsed);
            }
            AgentEvent::Approval(request) => state.approval = Some(request.clone()),
            AgentEvent::ApprovalResolved => state.approval = None,
            AgentEvent::AskUser(request) => state.question = Some(request.clone()),
            AgentEvent::PrivateTerminalInput(request) => {
                state.private_input = Some(request.clone())
            }
            AgentEvent::Finished(_) | AgentEvent::Stopped(_) => {
                state.note.completed = state.note.completed.saturating_add(1);
                state.question = None;
                state.private_input = None;
            }
            _ => {}
        }
        self.session.observe(event);
        state.panel.observe(event);
        let _ = self.events.send(event.clone());
        if matches!(
            event,
            AgentEvent::ConversationUpdated(_) | AgentEvent::Finished(_) | AgentEvent::Stopped(_)
        ) {
            state.note.updated_at = chrono::Utc::now().timestamp_millis();
            if let Err(error) = self.save(&mut state) {
                eprintln!("[nole] Session save failed: {error:#}");
            }
        }
    }

    pub fn summary(&self) -> SessionSummary {
        let state = self.lock();
        SessionSummary {
            id: state.note.id,
            title: state.note.title.clone(),
            updated_at: state.note.updated_at,
            running: self.session.is_running()
                || self.service.collaborators().is_running()
                || self.service.jobs().has_running(),
            unread: state.note.completed > state.note.read,
        }
    }

    pub fn scope(&self) -> AgentSessionScope {
        let state = self.lock();
        AgentSessionScope::from_parts(
            &self.session.conversation(),
            &state.panel.snapshot(),
            state.usage,
            state.output_tokens,
            state.duration,
        )
    }

    pub fn attach(
        &self,
    ) -> (
        AgentPanel,
        AgentSessionScope,
        broadcast::Receiver<AgentEvent>,
    ) {
        let state = self.lock();
        let receiver = self.events.subscribe();
        let scope = AgentSessionScope::from_parts(
            &self.session.conversation(),
            &state.panel.snapshot(),
            state.usage,
            state.output_tokens,
            state.duration,
        );
        (state.panel.clone(), scope, receiver)
    }

    pub fn history(&self) -> Vec<AgentPanelEntry> {
        self.lock().panel.snapshot()
    }
    pub fn save_error(&self) -> Option<String> {
        self.lock().save_error.clone()
    }
    pub fn pending_interactions(
        &self,
    ) -> (
        Option<crate::core::agent::ApprovalRequest>,
        Option<crate::core::agent::AskUserRequest>,
        Option<crate::core::agent::PrivateTerminalInputRequest>,
    ) {
        let state = self.lock();
        (
            state.approval.clone(),
            state.question.clone(),
            state.private_input.clone(),
        )
    }
    pub fn clear_approval(&self) {
        self.lock().approval = None;
    }
    pub fn clear_question(&self) {
        self.lock().question = None;
    }
    pub fn clear_private_input(&self) {
        self.lock().private_input = None;
    }
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    pub fn start(&self, prompt: String) -> Result<AgentSessionStart> {
        let mut state = self.lock();
        anyhow::ensure!(!state.deleted, "Session has been deleted");
        if self.session.is_running() {
            return Ok(AgentSessionStart::AlreadyRunning);
        }
        state.panel.push_prompt(prompt.clone(), false);
        match self.session.start(prompt) {
            Ok(AgentSessionStart::Started(_)) => {
                self.save(&mut state)?;
                Ok(AgentSessionStart::Started(self.events.subscribe()))
            }
            result => {
                state.panel.pop_prompt();
                result
            }
        }
    }

    pub fn buffer_prompt(&self, prompt: String, echo: String) -> bool {
        let mut state = self.lock();
        if state.deleted || !self.service.buffer_prompt(prompt) {
            return false;
        }
        state.panel.push_prompt(echo, true);
        true
    }

    pub fn mark_read(&self) -> Result<()> {
        let mut state = self.lock();
        if state.note.read == state.note.completed {
            return Ok(());
        }
        let previous = state.note.read;
        state.note.read = state.note.completed;
        if let Err(error) = self.save(&mut state) {
            state.note.read = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn persist(&self) -> Result<()> {
        self.save(&mut self.lock())
    }

    pub fn rename(&self, title: &str) -> Result<()> {
        let title = valid_title(title)?;
        let mut state = self.lock();
        let previous = std::mem::replace(&mut state.note.title, title);
        if let Err(error) = self.save(&mut state) {
            state.note.title = previous;
            return Err(error);
        }
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        let mut state = self.lock();
        let directory = store::directory(&self.root, state.note.id);
        self.session.cancel();
        self.service.terminate_session();
        std::fs::remove_dir_all(directory)?;
        state.deleted = true;
        Ok(())
    }
}

fn valid_title(title: &str) -> Result<String> {
    let title = title.trim();
    anyhow::ensure!(
        !title.is_empty() && title.chars().count() <= 200 && !title.chars().any(char::is_control),
        "Use a Session name between 1 and 200 characters"
    );
    Ok(title.to_string())
}

enum SessionEntry {
    Saved(Box<SessionNote>),
    Open(Arc<SessionNoteRuntime>),
}

pub struct AgentSessions {
    workspace: Arc<Workspace>,
    sessions: Mutex<HashMap<Uuid, SessionEntry>>,
}

impl AgentSessions {
    pub fn open(workspace: Arc<Workspace>) -> Result<Arc<Self>> {
        let mut notes = store::load(&workspace.storage().root)?;
        for note in &mut notes {
            if note.run_active {
                let (conversation, mut panel, usage, tokens, duration) =
                    note.scope.clone().into_parts();
                panel.push(AgentPanelEntry::Error(
                    "Session interrupted. Send a message to continue.".into(),
                ));
                note.scope =
                    AgentSessionScope::from_parts(&conversation, &panel, usage, tokens, duration);
                note.run_active = false;
                note.completed = note.completed.saturating_add(1);
                store::write(&workspace.storage().root, note)?;
            }
        }
        let sessions = notes
            .into_iter()
            .map(|note| (note.id, SessionEntry::Saved(Box::new(note))))
            .collect();
        Ok(Arc::new(Self {
            workspace,
            sessions: Mutex::new(sessions),
        }))
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        let mut summaries = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|entry| match entry {
                SessionEntry::Open(session) => session.summary(),
                SessionEntry::Saved(note) => SessionSummary {
                    id: note.id,
                    title: note.title.clone(),
                    updated_at: note.updated_at,
                    running: false,
                    unread: note.completed > note.read,
                },
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        summaries
    }

    pub fn get(&self, id: Uuid) -> Result<Arc<SessionNoteRuntime>> {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions.get_mut(&id).context("Session not found")?;
        if let SessionEntry::Saved(note) = entry {
            *entry = SessionEntry::Open(SessionNoteRuntime::open(
                &self.workspace,
                (**note).clone(),
                true,
            ));
        }
        match entry {
            SessionEntry::Open(session) => Ok(session.clone()),
            SessionEntry::Saved(_) => unreachable!(),
        }
    }

    pub fn draft(&self, title: &str) -> Result<Arc<SessionNoteRuntime>> {
        let title = valid_title(title)?;
        let now = chrono::Utc::now().timestamp_millis();
        let note = SessionNote {
            id: Uuid::new_v4(),
            title,
            created_at: now,
            updated_at: now,
            completed: 0,
            read: 0,
            permission: PermissionMode::Approve,
            run_active: false,
            scope: AgentSessionScope::default(),
        };
        Ok(SessionNoteRuntime::open(&self.workspace, note, false))
    }

    pub fn retain(&self, session: Arc<SessionNoteRuntime>) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let mut state = session.lock();
        if sessions.contains_key(&state.note.id) {
            return Ok(());
        }
        anyhow::ensure!(!state.deleted, "Session has been deleted");
        state.retained = true;
        if let Err(error) = session.save(&mut state) {
            state.retained = false;
            return Err(error);
        }
        let id = state.note.id;
        drop(state);
        sessions.insert(id, SessionEntry::Open(session));
        Ok(())
    }

    pub fn create(&self, title: &str) -> Result<Arc<SessionNoteRuntime>> {
        let session = self.draft(title)?;
        self.retain(session.clone())?;
        Ok(session)
    }

    pub fn delete(&self, id: Uuid) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(&id).context("Session not found")? {
            SessionEntry::Open(session) => session.delete()?,
            SessionEntry::Saved(_) => {
                std::fs::remove_dir_all(store::directory(&self.workspace.storage().root, id))?
            }
        }
        sessions.remove(&id);
        Ok(())
    }

    pub fn mark_reads_dirty(&self, paths: &[PathBuf]) {
        for entry in self.sessions.lock().unwrap().values() {
            if let SessionEntry::Open(session) = entry {
                let _ = session.service.mark_reads_dirty(paths);
            }
        }
    }
}
