use super::*;

#[derive(Clone)]
pub struct AgentRuntime {
    pub(super) events: AgentEventSender,
    pub(super) decisions:
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<ApprovalDecision>>>,
    pub(super) user_responses:
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<AskUserResponse>>>,
    pub(super) private_terminal_input:
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<PrivateTerminalInputDecision>>>,
    pub(super) input_buffer: Arc<Mutex<Vec<String>>>,
    pub(super) permission_mode: Arc<AtomicU8>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) terminal: AgentTerminalHandle,
    pub(super) jobs: AgentJobsHandle,
    pub(super) workspace_index: WorkspaceIndexHandle,
    pub(super) wiki_links: crate::core::wiki_link_index::WikiLinkIndexHandle,
    pub(super) session_directory: Option<PathBuf>,
    pub(super) mutation_lock: Arc<tokio::sync::Mutex<()>>,
    pub(super) collaborators: Option<collaboration::CollaboratorsHandle>,
}

impl AgentRuntime {
    pub fn new(
        events: AgentEventSender,
        decisions: tokio::sync::mpsc::UnboundedReceiver<ApprovalDecision>,
        user_responses: tokio::sync::mpsc::UnboundedReceiver<AskUserResponse>,
        private_terminal_input: tokio::sync::mpsc::UnboundedReceiver<PrivateTerminalInputDecision>,
        input_buffer: Arc<Mutex<Vec<String>>>,
        permission_mode: Arc<AtomicU8>,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            session_directory: None,
            collaborators: None,
            mutation_lock: Arc::default(),
            jobs: AgentJobsHandle::new(events.clone()),
            events,
            decisions: Arc::new(tokio::sync::Mutex::new(decisions)),
            user_responses: Arc::new(tokio::sync::Mutex::new(user_responses)),
            private_terminal_input: Arc::new(tokio::sync::Mutex::new(private_terminal_input)),
            input_buffer,
            cancelled,
            permission_mode,
            terminal: AgentTerminalHandle::default(),
            workspace_index: WorkspaceIndexHandle::default(),
            wiki_links: crate::core::wiki_link_index::WikiLinkIndexHandle::default(),
        }
    }

    pub(crate) fn with_collaborators(
        mut self,
        collaborators: collaboration::CollaboratorsHandle,
    ) -> Self {
        self.collaborators = Some(collaborators);
        self
    }

    pub fn with_session_directory(mut self, directory: PathBuf) -> Self {
        self.session_directory = Some(directory);
        self
    }

    pub fn with_terminal(mut self, terminal: AgentTerminalHandle) -> Self {
        self.terminal = terminal;
        self
    }

    pub fn with_jobs(mut self, jobs: AgentJobsHandle) -> Self {
        self.jobs = jobs;
        self
    }

    pub fn with_workspace_index(mut self, workspace_index: WorkspaceIndexHandle) -> Self {
        self.workspace_index = workspace_index;
        self
    }

    pub fn with_wiki_link_index(
        mut self,
        wiki_links: crate::core::wiki_link_index::WikiLinkIndexHandle,
    ) -> Self {
        self.wiki_links = wiki_links;
        self
    }
}
