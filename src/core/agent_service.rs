//! The agent application service: owns the Agent worker and the runtime
//! plumbing (event stream, approval / ask-user / private-input channels,
//! permission mode, buffered prompts, terminal and job handles) so clients
//! never wire those channels themselves.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::core::agent::{
    AgentEvent, AgentJobsHandle, AgentRuntime, AgentTerminalHandle, AgentWorker, ApprovalDecision,
    AskUserResponse, PermissionMode, PrivateTerminalInputDecision, AGENT_STREAM_BUFFER,
};
use crate::core::agent_session::AgentConversation;
use crate::core::observable::Observable;
use crate::core::workspace::Workspace;

pub struct AgentService {
    worker: AgentWorker,
    events: crate::core::agent::AgentEventSender,
    // The decision senders sit behind a mutex so tests can tap them; sends
    // are uncontended in production.
    approval_tx: Mutex<tokio::sync::mpsc::UnboundedSender<ApprovalDecision>>,
    user_tx: Mutex<tokio::sync::mpsc::UnboundedSender<AskUserResponse>>,
    private_input_tx: Mutex<tokio::sync::mpsc::UnboundedSender<PrivateTerminalInputDecision>>,
    input_buffer: Arc<Mutex<Vec<String>>>,
    permission_mode_atomic: Arc<AtomicU8>,
    cancelled: Arc<AtomicBool>,
    terminal: AgentTerminalHandle,
    jobs: AgentJobsHandle,
}

impl AgentService {
    /// Build the service on top of a workspace: spawn the worker thread and
    /// share the workspace's index and attachment-usage handles with the
    /// agent runtime and its tools.
    pub fn new(workspace: &Workspace) -> Self {
        let storage = workspace.storage();
        let terminal = AgentTerminalHandle::default();
        let input_buffer = Arc::new(Mutex::new(Vec::new()));
        let permission_mode_atomic = Arc::new(AtomicU8::new(PermissionMode::Approve.code()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (event_sender, _initial) = tokio::sync::broadcast::channel(AGENT_STREAM_BUFFER);
        let jobs = AgentJobsHandle::new(event_sender.clone());
        let (approval_tx, approval_rx) = tokio::sync::mpsc::unbounded_channel();
        let (user_tx, user_rx) = tokio::sync::mpsc::unbounded_channel();
        let (private_input_tx, private_input_rx) = tokio::sync::mpsc::unbounded_channel();
        let worker = AgentWorker::spawn(
            storage.ai_config_path.clone(),
            storage.root.clone(),
            AgentRuntime::new(
                event_sender.clone(),
                approval_rx,
                user_rx,
                private_input_rx,
                input_buffer.clone(),
                permission_mode_atomic.clone(),
                cancelled.clone(),
            )
            .with_terminal(terminal.clone())
            .with_jobs(jobs.clone())
            .with_workspace_index(workspace.workspace_index().clone())
            .with_wiki_link_index(workspace.wiki_links().clone()),
            workspace.attachment_usage().clone(),
        );
        Self {
            worker,
            events: event_sender,
            approval_tx: Mutex::new(approval_tx),
            user_tx: Mutex::new(user_tx),
            private_input_tx: Mutex::new(private_input_tx),
            input_buffer,
            permission_mode_atomic,
            cancelled,
            terminal,
            jobs,
        }
    }

    /// Start a run; the returned [`Observable`] carries the run's own event
    /// stream, output future, and cancellation token.
    pub fn run(
        &self,
        prompt: String,
        conversation: AgentConversation,
    ) -> Result<Observable<crate::core::agent::AgentRunOutput, AgentEvent>> {
        self.worker.run(prompt, conversation)
    }

    /// Subscribe a new receiver to the shared agent event stream. Every
    /// client (TUI today, other clients later) can observe the same events.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    /// Request cancellation of the current run. The worker observes the
    /// shared flag; the active run's observable cancellation token must be
    /// cancelled separately by its owner.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether a cancellation has been requested and not yet consumed by a
    /// new run. Test-only.
    #[cfg(test)]
    pub(crate) fn is_cancel_requested(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Swap the approval decision channel for tests so callers can observe
    /// exactly what the UI sends.
    #[cfg(test)]
    pub(crate) fn set_approval_sender_for_test(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<ApprovalDecision>,
    ) {
        if let Ok(mut current) = self.approval_tx.lock() {
            *current = sender;
        }
    }

    /// Swap the ask-user response channel for tests.
    #[cfg(test)]
    pub(crate) fn set_user_sender_for_test(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<AskUserResponse>,
    ) {
        if let Ok(mut current) = self.user_tx.lock() {
            *current = sender;
        }
    }

    /// Swap the private terminal input channel for tests.
    #[cfg(test)]
    pub(crate) fn set_private_input_sender_for_test(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<PrivateTerminalInputDecision>,
    ) {
        if let Ok(mut current) = self.private_input_tx.lock() {
            *current = sender;
        }
    }

    /// Answer a pending approval request.
    pub fn respond_approval(&self, decision: ApprovalDecision) -> Result<()> {
        let sender = self
            .approval_tx
            .lock()
            .map_err(|_| anyhow::anyhow!("Agent approval channel is unavailable"))?;
        sender
            .send(decision)
            .context("sending Agent approval decision")
    }

    /// Answer a pending ask-user request.
    pub fn respond_user(&self, response: AskUserResponse) -> Result<()> {
        let sender = self
            .user_tx
            .lock()
            .map_err(|_| anyhow::anyhow!("Agent user-response channel is unavailable"))?;
        sender.send(response).context("sending response to Agent")
    }

    /// Submit or cancel private terminal input. The decision value is moved
    /// through a private channel and never enters the cloneable event stream.
    pub fn submit_private_terminal_input(
        &self,
        decision: PrivateTerminalInputDecision,
    ) -> Result<()> {
        let sender = self
            .private_input_tx
            .lock()
            .map_err(|_| anyhow::anyhow!("Agent private-input channel is unavailable"))?;
        sender
            .send(decision)
            .context("sending private input to Agent terminal")
    }

    pub fn permission_mode(&self) -> PermissionMode {
        PermissionMode::from_code(self.permission_mode_atomic.load(Ordering::Relaxed))
    }

    pub fn set_permission_mode(&self, mode: PermissionMode) {
        self.permission_mode_atomic
            .store(mode.code(), Ordering::Relaxed);
    }

    /// Queue a prompt to run after the current run finishes.
    pub fn buffer_prompt(&self, prompt: String) -> bool {
        match self.input_buffer.lock() {
            Ok(mut buffer) => {
                buffer.push(prompt);
                true
            }
            Err(_) => false,
        }
    }

    /// Take every buffered follow-up prompt, in order.
    pub fn take_buffered_prompts(&self) -> Vec<String> {
        self.input_buffer
            .lock()
            .map(|mut buffer| std::mem::take(&mut *buffer))
            .unwrap_or_default()
    }

    pub fn clear_buffered_prompts(&self) {
        if let Ok(mut buffer) = self.input_buffer.lock() {
            buffer.clear();
        }
    }

    /// Invalidate cached agent reads for the given paths (for example after
    /// external filesystem changes).
    pub fn mark_reads_dirty(&self, paths: &[PathBuf]) -> Result<()> {
        self.worker.mark_reads_dirty(paths)
    }

    /// Drop cached agent reads and registers (after clearing a session).
    pub fn clear_read_state(&self) -> Result<()> {
        self.worker.clear_read_state()
    }

    /// Terminate the agent's embedded terminal sessions and background jobs.
    pub fn terminate_session(&self) {
        self.terminal.terminate();
        self.jobs.terminate();
    }

    pub fn terminal(&self) -> &AgentTerminalHandle {
        &self.terminal
    }

    pub fn jobs(&self) -> &AgentJobsHandle {
        &self.jobs
    }

    /// The attachment-usage state this service's worker observes.
    /// Test-only: verifies the service shares the workspace's handles.
    #[cfg(test)]
    pub(crate) fn attachment_usage_handle(
        &self,
    ) -> &crate::core::attachment_usage::AttachmentUsageHandle {
        self.worker.attachment_usage()
    }

    /// The wiki-link index this service's worker observes.
    /// Test-only: verifies the service shares the workspace's handles.
    #[cfg(test)]
    pub(crate) fn wiki_links_handle(&self) -> &crate::core::wiki_link_index::WikiLinkIndexHandle {
        self.worker.wiki_links()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::attachment_index::AttachmentReferenceIndex;
    use crate::core::storage::Storage;
    use crate::core::wiki_link_index::WikiLinkIndex;

    fn service() -> (Workspace, AgentService, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let workspace = Workspace::open(storage.clone()).unwrap();
        let service = AgentService::new(&workspace);
        (workspace, service, directory)
    }

    #[test]
    fn builds_on_a_workspace_and_shares_its_handles() {
        let (workspace, service, directory) = service();
        let storage = Storage::new(directory.path()).unwrap();

        // Attachment usage: publishing through the workspace reaches the
        // worker's delete_attachment tool state.
        assert!(!service.attachment_usage_handle().is_ready());
        workspace
            .attachment_usage()
            .publish_snapshot(0, AttachmentReferenceIndex::default());
        assert!(service.attachment_usage_handle().is_ready());

        // Wiki links: publishing through the workspace reaches the worker's
        // wiki-link tools.
        std::fs::write(storage.data_dir.join("A.md"), "[[B]]\n").unwrap();
        std::fs::write(storage.data_dir.join("B.md"), "body\n").unwrap();
        let target = storage.data_dir.join("B.md");
        assert!(service
            .wiki_links_handle()
            .with_index(|index| index.backlinks(&target))
            .is_none());
        workspace
            .wiki_links()
            .replace(WikiLinkIndex::build(&storage));
        assert_eq!(
            service
                .wiki_links_handle()
                .with_index(|index| index.backlinks(&target))
                .unwrap(),
            vec![storage.data_dir.join("A.md")]
        );
    }

    #[test]
    fn subscriptions_are_independent_receivers() {
        let (_workspace, service, _directory) = service();
        let mut first = service.subscribe();
        let mut second = service.subscribe();
        // Both receivers observe a broadcast sent after subscription.
        service
            .events
            .send(AgentEvent::Notification("hello".to_string()))
            .unwrap();
        assert!(matches!(
            first.try_recv(),
            Ok(AgentEvent::Notification(message)) if message == "hello"
        ));
        assert!(matches!(
            second.try_recv(),
            Ok(AgentEvent::Notification(message)) if message == "hello"
        ));
    }

    #[test]
    fn permission_mode_round_trips() {
        let (_workspace, service, _directory) = service();
        assert_eq!(service.permission_mode(), PermissionMode::Approve);
        service.set_permission_mode(PermissionMode::Yolo);
        assert_eq!(service.permission_mode(), PermissionMode::Yolo);
        service.set_permission_mode(PermissionMode::Auto);
        assert_eq!(service.permission_mode(), PermissionMode::Auto);
    }

    #[test]
    fn buffered_prompts_queue_take_and_clear() {
        let (_workspace, service, _directory) = service();
        assert!(service.buffer_prompt("first".to_string()));
        assert!(service.buffer_prompt("second".to_string()));
        service.clear_buffered_prompts();
        assert!(service.take_buffered_prompts().is_empty());

        assert!(service.buffer_prompt("third".to_string()));
        assert_eq!(service.take_buffered_prompts(), vec!["third".to_string()]);
        assert!(service.take_buffered_prompts().is_empty());
    }
}
