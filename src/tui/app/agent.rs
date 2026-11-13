//! Agent coordination: the event loop, prompt dispatch, session lifecycle,
//! and approval / ask-user / permission handling.

use super::*;

impl App {
    pub(in crate::tui::app) fn last_agent_output(&self) -> Option<&str> {
        self.agent_panel.last_assistant_output()
    }

    pub(in crate::tui::app) fn copy_last_agent_output(&mut self) -> Option<Command> {
        let Some(text) = self.last_agent_output().map(str::to_string) else {
            self.set_status("No Agent output to copy");
            return None;
        };
        Some(Command::CopyText(text))
    }

    pub(in crate::tui::app) fn scroll_agent_by(&mut self, delta: i32) {
        self.agent_follow_tail = false;
        self.agent_scroll = if delta > 0 {
            self.agent_scroll.saturating_add(delta as u16)
        } else {
            self.agent_scroll
                .saturating_sub(delta.unsigned_abs() as u16)
        };
    }

    pub(in crate::tui::app) fn scroll_agent_stats_by(&mut self, delta: i32) {
        self.agent_stats_scroll = if delta > 0 {
            self.agent_stats_scroll.saturating_add(delta as usize)
        } else {
            self.agent_stats_scroll
                .saturating_sub(delta.unsigned_abs() as usize)
        };
    }

    /// Scroll whichever agent view is on screen: the statistics panel while
    /// the center shows chat, the output panel otherwise.
    pub(in crate::tui::app) fn scroll_agent_view_by(&mut self, delta: i32) {
        if self.center_view == CenterView::Chat {
            self.scroll_agent_stats_by(delta);
        } else {
            self.scroll_agent_by(delta);
        }
    }

    pub(in crate::tui::app) fn agent_view_at_top(&self) -> bool {
        if self.center_view == CenterView::Chat {
            self.agent_stats_scroll == 0
        } else {
            self.agent_scroll == 0
        }
    }

    pub fn poll_agent(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        let selected_session = self.selected_agent_session();
        self.agent_notes = self.sessions.list();
        if let Some(id) = selected_session {
            if let Some(row) = self
                .visible_file_rows()
                .iter()
                .position(|row| *row == FileListRow::Agent(id))
            {
                self.file_row = row;
            }
        }
        if self.terminal_focused
            && (self.center_view == CenterView::Chat || self.focus == Focus::Agent)
        {
            if let Err(error) = self.session_note.mark_read() {
                self.set_error(format!("Session save failed: {error}"));
            }
        }
        loop {
            match self.agent_events_rx.try_recv() {
                Ok(event) => events.push(event),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    disconnected = true;
                    break;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    let (_, panel, usage, tokens, duration) =
                        self.session_note.scope().into_parts();
                    self.agent_panel = AgentPanel::from_entries(panel);
                    self.agent_usage = usage;
                    self.agent_timed_output_tokens = tokens;
                    self.agent_response_duration = duration;
                    events.clear();
                }
            }
        }
        for event in events {
            if self.ai_cancelling
                && !matches!(
                    &event,
                    AgentEvent::Stopped(_) | AgentEvent::Finished(_) | AgentEvent::ApprovalResolved
                )
            {
                continue;
            }
            match event {
                AgentEvent::AssistantDelta(delta) => {
                    self.agent_panel.apply_assistant_delta(&delta);
                }
                AgentEvent::AssistantMessageFinished { text, final_output } => {
                    self.agent_panel
                        .apply_assistant_message_finished(&text, final_output);
                }
                AgentEvent::ThinkingDelta(delta) => {
                    self.agent_panel.apply_thinking_delta(&delta);
                }
                AgentEvent::ThinkingFinished => {
                    self.agent_panel.apply_thinking_finished();
                }
                AgentEvent::BufferedInputConsumed(count) => {
                    self.agent_panel.apply_buffered_input_consumed(count);
                }
                AgentEvent::ToolPreparing { index, message } => {
                    self.agent_panel.apply_tool_preparing(index, &message);
                    self.set_status(message.lines().next().unwrap_or("Preparing tool..."));
                }
                AgentEvent::ToolPreparationFinished { index, id } => {
                    self.agent_panel
                        .apply_tool_preparation_finished(index, id.as_deref());
                }
                AgentEvent::ToolStarted { id, message } => {
                    self.agent_panel.apply_tool_started(&id, &message);
                    self.set_status(message);
                }
                AgentEvent::ToolFinished {
                    id,
                    message,
                    preview,
                } => {
                    self.agent_panel
                        .apply_tool_finished(&id, &message, preview.as_deref());
                    self.set_status(message);
                }
                AgentEvent::ToolUpdated { id, message } => {
                    if self.agent_panel.apply_tool_updated(&id, &message) {
                        self.set_status(message);
                    }
                }
                AgentEvent::JobStarted { id, label, .. } => {
                    self.set_status(format!("Background job {id} started: {label}"));
                }
                AgentEvent::JobSettled { id, failed } => {
                    // While a run is active, the Agent loop drains deliveries
                    // at the next round boundary and injects them as a framed
                    // user message. Idle: start a delivery run now.
                    let label = self
                        .agent
                        .jobs()
                        .rows()
                        .into_iter()
                        .find(|row| row.id == id)
                        .map(|row| row.label)
                        .unwrap_or_default();
                    self.notifications.notify(if failed {
                        format!("Background job {id} failed: {label}")
                    } else {
                        format!("Background job {id} finished: {label}")
                    });
                    // Idle settlement: no run will drain the delivery, so
                    // start the wake-up run through the session capability.
                }
                AgentEvent::JobDelivered(ids) => {
                    self.set_status(format!(
                        "Background job result delivered: {}",
                        ids.join(", ")
                    ));
                }
                AgentEvent::Usage(usage) => self.agent_usage.add(usage),
                AgentEvent::ContextWindow { tokens, capacity } => {
                    self.agent_context_window = tokens;
                    self.agent_context_capacity = capacity;
                }
                AgentEvent::ResponseTiming {
                    output_tokens,
                    elapsed,
                } => {
                    self.agent_timed_output_tokens =
                        self.agent_timed_output_tokens.saturating_add(output_tokens);
                    self.agent_response_duration =
                        self.agent_response_duration.saturating_add(elapsed);
                }
                AgentEvent::Retry => {
                    self.agent_retry_count = self.agent_retry_count.saturating_add(1);
                    self.agent_panel.apply_retry();
                }
                AgentEvent::Round { current } => {
                    self.ai_running = true;
                    self.agent_round = current;
                    if !self.agent_panel.has_active_tools() {
                        self.set_status("AI is working...");
                    }
                }
                AgentEvent::ConversationUpdated(conversation) => {
                    // Apply before persisting: the supervisor syncs the
                    // session's copy asynchronously, but the TUI's own
                    // stream order is deterministic for persistence.
                    let _ = conversation;
                    if let Err(error) = self.persist_agent_session() {
                        self.set_error(format!("Agent session save error: {error}"));
                    }
                }
                AgentEvent::Notification(message) => {
                    self.notifications.notify(message);
                    self.set_status("Agent sent a notification");
                }
                AgentEvent::FileMoved { from, to } => {
                    self.handle_agent_file_moved(&from, &to);
                }
                AgentEvent::OpenFile(path) => {
                    self.open_file_document(&path, DocumentReturn::Daily);
                    if self
                        .document
                        .as_ref()
                        .is_some_and(|document| document.kind == DocumentKind::File(path.clone()))
                    {
                        self.set_status(format!("Agent opened {}", path.display()));
                    }
                }
                AgentEvent::Approval(request) => {
                    // The gate is the single decision source: whatever lands
                    // here already needs an explicit user decision. The UI
                    // presents that decision consistently across permission modes.
                    self.set_status(format!("Approval required: {}", request.title));
                    self.approval_request = Some(request);
                    self.approval_scroll = 0;
                    self.set_overlay(Overlay::Approval);
                }
                AgentEvent::ApprovalResolved => {
                    self.approval_request = None;
                    if self.overlay == Some(Overlay::Approval) {
                        self.overlay = None;
                    }
                    if self.dialog.as_ref().is_some_and(|dialog| {
                        matches!(
                            dialog.purpose,
                            DialogPurpose::AgentApproval | DialogPurpose::AgentDestructiveApproval
                        )
                    }) {
                        self.dialog = None;
                    }
                }
                AgentEvent::AskUser(request) => {
                    self.set_status("Agent is waiting for your answer");
                    self.ask_user_option = 0;
                    self.ask_user_input.clear();
                    self.ask_user_cursor = 0;
                    self.ask_user_request = Some(request);
                    self.set_overlay(Overlay::AskUser);
                }
                AgentEvent::PrivateTerminalInput(request) => {
                    self.private_terminal_input.zeroize();
                    self.private_terminal_input_cursor = 0;
                    self.private_terminal_input_request = Some(request);
                    self.set_status("Agent terminal is waiting for private input");
                    self.set_overlay(Overlay::PrivateTerminalInput);
                }
                AgentEvent::Stopped(reason) => {
                    self.agent_panel.deactivate_tools();
                    if self.ai_cancelling {
                        self.ai_cancelling = false;
                        continue;
                    }
                    self.ai_running = false;
                    let (notification, status) = match reason {
                        AgentStopReason::ToolApprovalDenied => (
                            "Agent stopped after tool approval was denied",
                            "Agent stopped after tool approval was denied",
                        ),
                    };
                    self.notifications.notify(notification);
                    self.set_status(status);
                    self.clear_ask_user();
                    self.clear_private_terminal_input();
                    self.reload_workspace();
                    if let Err(error) = self.persist_agent_session() {
                        self.set_error(format!("Agent session save error: {error}"));
                    }
                }
                AgentEvent::Finished(result) => {
                    self.agent_panel.deactivate_tools();
                    self.agent_panel.drop_unclaimed_preparations();
                    if self.ai_cancelling {
                        self.ai_cancelling = false;
                        continue;
                    }
                    self.ai_running = false;
                    match result {
                        Ok(_) => {
                            self.notifications.notify("Agent finished");
                            self.set_status("Agent finished");
                        }
                        Err(error) => {
                            self.agent_panel.settle_streaming();
                            self.agent_panel
                                .push_error(format!("Agent failed: {error}"));
                            self.set_error(format!("AI error: {error}"));
                        }
                    }
                    self.clear_ask_user();
                    self.clear_private_terminal_input();
                    self.reload_workspace();
                    if let Err(error) = self.persist_agent_session() {
                        self.set_error(format!("Agent session save error: {error}"));
                    }
                }
            }
        }
        if disconnected && self.ai_running {
            self.ai_running = false;
            self.agent_panel
                .push_error("Agent worker stopped unexpectedly");
            self.clear_ask_user();
            self.clear_private_terminal_input();
            self.set_error("AI error: worker stopped unexpectedly");
        }
    }

    /// Respond to a freshly published document-index snapshot (applied to
    /// the shared handles by the workspace service): refresh the views that
    /// read those indexes.
    pub fn on_index_update(&mut self) {
        if self.status == "Workspace index is still building" {
            self.status.clear();
        }
        if self.center_view == CenterView::Search && !self.search_query.trim().is_empty() {
            self.recompute_search();
        } else if self.center_view == CenterView::Tags {
            self.recompute_tags();
        }
        if self.center_view == CenterView::Attachments {
            self.recompute_attachments();
        }
        self.recompute_document_backlinks();
    }

    /// Recompute [`App::document_backlinks`] from the published wiki-link index
    /// for the currently open managed note (daily, data, or archives). Skills
    /// and previews use local content, leaving the managed backlink list empty.
    pub(crate) fn recompute_document_backlinks(&mut self) {
        self.document_backlinks.clear();
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let path = match &document.kind {
            DocumentKind::File(path) => Some(path.clone()),
            DocumentKind::Daily(date) => self.storage().daily_file_path(&date.to_string()).ok(),
            DocumentKind::Skill(_) => None,
        };
        if let Some(path) = path {
            if let Some(backlinks) = self.workspace.backlinks(&path) {
                self.document_backlinks = backlinks;
            }
        }
    }

    pub fn mark_agent_reads_dirty(&mut self, paths: &[PathBuf]) {
        self.sessions.mark_reads_dirty(paths);
        if let Err(error) = self.agent.mark_reads_dirty(paths) {
            self.set_error(format!("Agent read-state error: {error:#}"));
        }
    }

    pub(super) fn handle_agent_file_moved(&mut self, from: &Path, to: &Path) {
        let from = self.resolve_agent_event_path(from);
        let to = self.resolve_agent_event_path(to);
        let document_retargeted = self.retarget_open_document(&from, &to);
        if document_retargeted || self.selected_file.as_deref() == Some(from.as_path()) {
            self.selected_file = Some(to.clone());
        }
        if self.pending_file.as_deref() == Some(from.as_path()) {
            self.pending_file = Some(to);
        }
    }

    pub(super) fn resolve_agent_event_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.storage().root.join(path)
        }
    }

    pub(super) fn retarget_open_document(&mut self, from: &Path, to: &Path) -> bool {
        self.document_render_lru.retarget_file(from, to);
        let Some(document) = self.document.as_mut() else {
            return false;
        };
        document.kind = match &document.kind {
            DocumentKind::File(path) if path == from => DocumentKind::File(to.to_path_buf()),
            DocumentKind::Skill(path) if path == from => DocumentKind::Skill(to.to_path_buf()),
            _ => return false,
        };
        document.title = if matches!(document.kind, DocumentKind::Skill(_)) {
            to.file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Skill".to_string())
        } else {
            to.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Document".to_string())
        };
        self.recompute_document_backlinks();
        true
    }

    pub(super) fn open_agent_prompt(&mut self, date: NaiveDate) {
        if self.daily_note_clone(date).is_none() {
            self.set_status("Daily note not found");
            return;
        }
        self.ai_source_date = Some(date);
        self.ai_prompt_input.clear();
        self.ai_prompt_cursor = 0;
        self.set_overlay(Overlay::AiPrompt);
    }

    pub(super) fn submit_agent_prompt(&mut self) {
        let Some(date) = self.ai_source_date.take() else {
            self.overlay = None;
            return;
        };
        let Ok(path) = self.storage().daily_file_path(&date.to_string()) else {
            self.overlay = None;
            self.set_status("Daily note not found");
            return;
        };
        if !path.is_file() {
            self.overlay = None;
            self.set_status("Daily note not found");
            return;
        }
        let display_path = path
            .strip_prefix(&self.storage().root)
            .unwrap_or(&path)
            .to_string_lossy();
        let requested = self.ai_prompt_input.trim();
        let (prompt, display_prompt) = if requested.is_empty() {
            (
                format!(
                    "The user wants you to format the daily note at: {display_path}\n\n{FORMAT_DAILY_NOTE_PROMPT}"
                ),
                format!("Format {display_path}"),
            )
        } else {
            (
                format!(
                    "The user wants you to work on the daily note at: {display_path}\n\n{requested}"
                ),
                requested.to_string(),
            )
        };
        self.overlay = None;
        self.dialog = None;
        if self.ai_running {
            self.buffer_agent_prompt(prompt, display_prompt);
        } else {
            self.start_agent(prompt, display_prompt);
        }
    }

    pub(super) fn submit_compose_to_agent(&mut self) {
        let Some(prompt) = self.compose_agent_prompt() else {
            self.set_status("Enter a prompt for Agent");
            return;
        };
        let display_prompt = self.input.trim().to_string();
        let accepted = if self.ai_running {
            self.buffer_agent_prompt(prompt, display_prompt)
        } else {
            self.start_agent(prompt, display_prompt)
        };
        if accepted {
            self.clear_compose_input();
        }
    }

    pub(super) fn buffer_agent_prompt(&mut self, prompt: String, display_prompt: String) -> bool {
        let queued = self
            .session_note
            .buffer_prompt(prompt, display_prompt.clone());
        if !queued {
            self.set_error("Agent input buffer is unavailable");
            return false;
        }
        self.agent_panel.push_prompt(display_prompt, true);
        self.agent_follow_tail = true;
        self.set_status("Prompt buffered for Agent");
        true
    }

    pub(super) fn compose_agent_prompt(&self) -> Option<String> {
        let content = self.input.trim();
        if content.is_empty() {
            return None;
        }
        let context =
            match self.center_view {
                CenterView::Daily => self.selected_date().and_then(|date| {
                    self.storage()
                        .daily_file_path(&date.to_string())
                        .ok()
                        .map(|path| ("daily note", path))
                }),
                CenterView::Document => self.document.as_ref().and_then(|document| match &document
                    .kind
                {
                    DocumentKind::File(path) => Some(("note", path.clone())),
                    DocumentKind::Skill(path) => Some(("skill", path.clone())),
                    DocumentKind::Daily(date) => self
                        .storage()
                        .daily_file_path(&date.to_string())
                        .ok()
                        .map(|path| ("daily note", path)),
                }),
                CenterView::Chat
                | CenterView::Todo
                | CenterView::Search
                | CenterView::DocumentSearch
                | CenterView::Tags
                | CenterView::Attachments => None,
            };
        Some(if let Some((kind, path)) = context {
            let display = path
                .strip_prefix(&self.storage().root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            #[cfg(windows)]
            let display = display.replace('\\', "/");
            format!("The user is currently viewing {kind}: {display}\n\n{content}")
        } else {
            content.to_string()
        })
    }

    pub(super) fn start_agent(&mut self, prompt: String, display_prompt: String) -> bool {
        if self.ai_running || self.ai_cancelling {
            self.set_status("AI is already working");
            return false;
        }
        self.agent_panel.push_prompt(display_prompt, false);
        self.agent_follow_tail = true;
        self.start_agent_worker(prompt)
    }

    pub(super) fn start_agent_worker(&mut self, prompt: String) -> bool {
        if self.ai_running || self.ai_cancelling {
            self.set_status("AI is already working");
            return false;
        }
        self.reload_thinking_display_config();
        self.reload_agent_effort();
        self.agent.clear_buffered_prompts();
        self.ai_running = true;
        self.agent_round = 0;
        self.set_status("AI is working...");
        if let Err(error) = self.sessions.retain(self.session_note.clone()) {
            self.ai_running = false;
            self.set_error(format!("Session save failed: {error}"));
            return false;
        }
        match self.session_note.start(prompt) {
            Ok(AgentSessionStart::Started(_)) => true,
            Ok(AgentSessionStart::AlreadyRunning) => {
                self.ai_running = false;
                self.set_status("AI is already working");
                false
            }
            Err(error) => {
                self.ai_running = false;
                if agent_debug_logging_enabled() {
                    eprintln!("[nole debug] Agent error: {error:#}");
                }
                self.set_error(format!("AI error: {error:#}"));
                false
            }
        }
    }

    /// Start the wake-up run for queued background-job deliveries via the
    /// session's core capability, adopting its prompt echo and event
    /// stream into the panel. Called whenever the Agent is or has just
    /// gone idle: an idle settlement (no run to observe it), or a delivery
    /// that settled while the previous run was already finishing its last
    /// answer. A delivery left queued here would strand its job's result
    pub(super) fn cancel_agent(&mut self) {
        if !self.ai_running {
            self.set_status("Agent is not running");
            return;
        }
        self.agent_session.cancel();
        self.ai_running = false;
        self.ai_cancelling = true;
        self.agent.clear_buffered_prompts();
        self.approval_request = None;
        self.clear_ask_user();
        self.clear_private_terminal_input();
        if self.overlay == Some(Overlay::Approval) {
            self.overlay = None;
        }
        self.agent_panel.settle_streaming();
        self.agent_panel.deactivate_tools();
        self.agent_panel.push_error("Cancelled");
        self.notifications.notify("Agent task cancelled");
        self.set_status("Agent task cancelled");
    }

    pub(super) fn persist_agent_session(&self) -> anyhow::Result<()> {
        self.session_note.persist()
    }

    pub(super) fn send_user_response(&mut self, response: AskUserResponse) -> anyhow::Result<()> {
        self.agent.respond_user(response.clone())?;
        self.session_note.clear_question();
        if let AskUserResponse::Answer(answer) = &response {
            self.agent_panel.apply_ask_answer(answer);
        }
        self.set_status(match response {
            AskUserResponse::Answer(_) => "Answer sent to Agent",
            AskUserResponse::Cancelled => "Agent question cancelled",
        });
        self.clear_ask_user();
        Ok(())
    }

    pub(super) fn clear_ask_user(&mut self) {
        self.ask_user_request = None;
        self.ask_user_input.clear();
        self.ask_user_cursor = 0;
        self.ask_user_option = 0;
        if self.overlay == Some(Overlay::AskUser) {
            self.overlay = None;
        }
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.purpose == DialogPurpose::AskUser)
        {
            self.dialog = None;
        }
    }

    pub(super) fn send_private_terminal_input(&mut self, submit: bool) -> anyhow::Result<()> {
        let decision = if submit {
            let private_value = std::mem::replace(
                &mut self.private_terminal_input,
                Zeroizing::new(String::new()),
            );
            PrivateTerminalInputDecision::Submit(private_value)
        } else {
            self.private_terminal_input.zeroize();
            PrivateTerminalInputDecision::Cancelled
        };
        if let Err(error) = self.agent.submit_private_terminal_input(decision) {
            self.clear_private_terminal_input();
            return Err(error.context("sending private input to Agent"));
        }
        self.session_note.clear_private_input();
        self.set_status(if submit {
            "Private input submitted to Agent terminal"
        } else {
            "Private terminal input cancelled"
        });
        self.clear_private_terminal_input();
        Ok(())
    }

    pub(super) fn clear_private_terminal_input(&mut self) {
        self.private_terminal_input.zeroize();
        self.private_terminal_input_cursor = 0;
        self.private_terminal_input_request = None;
        if self.overlay == Some(Overlay::PrivateTerminalInput) {
            self.overlay = None;
        }
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.purpose == DialogPurpose::PrivateTerminalInput)
        {
            self.dialog = None;
        }
    }

    pub(super) fn send_approval(&mut self, decision: ApprovalDecision) -> anyhow::Result<()> {
        self.agent.respond_approval(decision)?;
        self.session_note.clear_approval();
        self.set_status(match decision {
            ApprovalDecision::Approve => "Change approved",
            ApprovalDecision::Deny => "Change denied",
        });
        self.approval_request = None;
        if self.overlay == Some(Overlay::Approval) {
            self.overlay = None;
        }
        if self.dialog.as_ref().is_some_and(|dialog| {
            matches!(
                dialog.purpose,
                DialogPurpose::AgentApproval | DialogPurpose::AgentDestructiveApproval
            )
        }) {
            self.dialog = None;
        }
        Ok(())
    }

    pub(super) fn toggle_permission_mode(&mut self) {
        let mode = self.agent.permission_mode().cycled();
        self.agent.set_permission_mode(mode);
        if let Err(error) = self.session_note.persist() {
            self.set_error(error.to_string());
        }
        // Entering YOLO approves whatever approval is currently waiting.
        // AUTO and APPROVE leave a pending request at the gate for its user
        // decision; YOLO approves the request currently shown by the overlay.
        if mode == PermissionMode::Yolo && self.overlay == Some(Overlay::Approval) {
            let _ = self.send_approval(ApprovalDecision::Approve);
        }
        self.set_status(format!("Permission mode: {}", mode.label()));
    }
}
