//! The application bootstrap: owns the workspace, agent, and watcher
//! services as one lifecycle object so a client (the TUI today, other
//! clients later) starts Nole with a single call instead of assembling
//! storage, indexes, agent channels, and the filesystem watcher itself.

use std::sync::Arc;

use anyhow::Result;

use crate::core::agent_service::AgentService;
use crate::core::storage::Storage;
use crate::core::workspace::Workspace;
use crate::core::workspace_watcher::{WorkspaceEvent, WorkspaceWatcher};

pub struct Nole {
    workspace: Arc<Workspace>,
    agent: Arc<AgentService>,
    watcher: WorkspaceWatcher,
}

impl Nole {
    /// Open the application: register the filesystem watcher, open the
    /// workspace (spawning the document indexer), and start the agent
    /// worker on top of the workspace's shared handles.
    pub fn open(storage: Storage) -> Result<Self> {
        let watcher = WorkspaceWatcher::new(&storage)?;
        let workspace = Arc::new(Workspace::open(storage)?);
        let agent = Arc::new(AgentService::new(&workspace));
        Ok(Self {
            workspace,
            agent,
            watcher,
        })
    }

    pub fn workspace(&self) -> &Arc<Workspace> {
        &self.workspace
    }

    pub fn agent(&self) -> &Arc<AgentService> {
        &self.agent
    }

    /// Drain filesystem changes: refresh the workspace indexes and return
    /// the domain events clients should respond to. This is the only
    /// orchestration the bootstrap performs.
    pub fn poll_workspace(&mut self) -> Vec<WorkspaceEvent> {
        self.watcher.poll(&self.workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client that imports no TUI modules can boot the application,
    /// query the workspace, subscribe to the agent event stream, and poll
    /// the watcher. Compile-time and runtime proof of the service boundary.
    #[test]
    fn opens_and_serves_clients_without_terminal_dependencies() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();

        let mut nole = Nole::open(storage).unwrap();

        // Workspace queries work standalone.
        let workspace = nole.workspace();
        assert!(workspace.daily_notes().unwrap().is_empty());
        assert!(workspace.todos().unwrap().is_empty());

        // Agent events subscribe standalone.
        let mut events = nole.agent().subscribe();
        assert!(events.try_recv().is_err());

        // The watcher polls without a terminal event loop.
        assert!(nole.poll_workspace().is_empty());
    }
}
