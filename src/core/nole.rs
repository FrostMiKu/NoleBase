//! The application bootstrap: owns the workspace, Session notes, and watcher
//! services as one lifecycle object so a client (the TUI today, other
//! clients later) starts Nole with a single call instead of assembling
//! storage, indexes, agent channels, and the filesystem watcher itself.

use std::sync::Arc;

use anyhow::Result;

use crate::core::agent_sessions::AgentSessions;
use crate::core::storage::Storage;
use crate::core::workspace::Workspace;
use crate::core::workspace_watcher::{WorkspaceEvent, WorkspaceWatcher};

pub struct Nole {
    workspace: Arc<Workspace>,
    sessions: Arc<AgentSessions>,
    watcher: WorkspaceWatcher,
}

impl Nole {
    /// Initialize workspace files, register the filesystem watcher, and open
    /// the workspace and Session catalog. Session workers start as notes are opened.
    pub fn open(storage: Storage) -> Result<Self> {
        storage.ensure_files()?;
        let watcher = WorkspaceWatcher::new(&storage)?;
        let workspace = Arc::new(Workspace::open(storage)?);
        let sessions = AgentSessions::open(workspace.clone())?;
        Ok(Self {
            workspace,
            sessions,
            watcher,
        })
    }

    pub fn workspace(&self) -> &Arc<Workspace> {
        &self.workspace
    }

    pub fn sessions(&self) -> &Arc<AgentSessions> {
        &self.sessions
    }

    /// Drain filesystem changes: refresh the workspace indexes and return
    /// the domain events clients should respond to. This is the only
    /// orchestration the bootstrap performs.
    pub fn poll_workspace(&mut self) -> Vec<WorkspaceEvent> {
        self.watcher.poll(&self.workspace)
    }
}
