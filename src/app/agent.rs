//! Agent coordination: the event loop, prompt dispatch, session lifecycle,
//! and approval / ask-user / permission handling.

use super::*;

impl App {
    pub fn poll_agent(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(observable) = &mut self.active_agent {
            loop {
                match observable.events.try_recv() {
                    Ok(event) => events.push(event),
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                        disconnected = true;
                        break;
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                        observable.cancel.cancel();
                        self.agent_worker
                            .cancellation_token()
                            .store(true, Ordering::Relaxed);
                        events.push(AgentEvent::Finished(Err(
                            "Agent event stream lagged; task cancelled".to_string(),
                        )));
                        break;
                    }
                }
            }
        }
        for event in events {
            if self.ai_cancelling
                && !matches!(&event, AgentEvent::Stopped(_) | AgentEvent::Finished(_))
            {
                continue;
            }
            match event {
                AgentEvent::AssistantDelta(delta) => {
                    if delta.is_empty() {
                        continue;
                    }
                    match self.agent_panel.last_mut() {
                        Some(AgentPanelEntry::Assistant {
                            text, streaming, ..
                        }) if *streaming => text.push_str(&delta),
                        _ => self.agent_panel.push(AgentPanelEntry::Assistant {
                            text: delta,
                            streaming: true,
                            final_output: false,
                        }),
                    }
                    self.agent_scroll = u16::MAX;
                }
                AgentEvent::AssistantMessageFinished { text, final_output } => {
                    if text.trim().is_empty() {
                        self.agent_panel.retain(|entry| {
                            !matches!(
                                entry,
                                AgentPanelEntry::Assistant {
                                    streaming: true,
                                    ..
                                }
                            )
                        });
                    } else if let Some(AgentPanelEntry::Assistant {
                        text: entry_text,
                        streaming,
                        final_output: entry_final,
                    }) = self.agent_panel.iter_mut().rev().find(|entry| {
                        matches!(
                            entry,
                            AgentPanelEntry::Assistant {
                                streaming: true,
                                ..
                            }
                        )
                    }) {
                        *entry_text = text;
                        *streaming = false;
                        *entry_final = final_output;
                    } else {
                        self.agent_panel.push(AgentPanelEntry::Assistant {
                            text,
                            streaming: false,
                            final_output,
                        });
                    }
                }
                AgentEvent::BufferedInputConsumed(count) => {
                    for followup in self
                        .agent_panel
                        .iter_mut()
                        .filter_map(|entry| match entry {
                            AgentPanelEntry::Prompt { muted, .. } if *muted => Some(muted),
                            _ => None,
                        })
                        .take(count)
                    {
                        *followup = false;
                    }
                }
                AgentEvent::ToolStarted(message) => {
                    self.agent_panel.push(AgentPanelEntry::Tool {
                        text: message.clone(),
                        active: true,
                    });
                    self.agent_scroll = u16::MAX;
                    self.set_status(message);
                }
                AgentEvent::ToolFinished(message) => {
                    if let Some(AgentPanelEntry::Tool { text, active }) =
                        self.agent_panel.iter_mut().rev().find(|entry| {
                            matches!(entry, AgentPanelEntry::Tool { active: true, .. })
                        })
                    {
                        let answer = (message.starts_with("Completed Ask User.")
                            && text.starts_with("Calling Ask User..."))
                        .then(|| text.lines().nth(2).map(str::to_string))
                        .flatten();
                        *text = answer
                            .map(|answer| format!("{message}\n{answer}"))
                            .unwrap_or_else(|| message.clone());
                        *active = false;
                    } else {
                        self.agent_panel.push(AgentPanelEntry::Tool {
                            text: message.clone(),
                            active: false,
                        });
                    }
                    self.agent_scroll = u16::MAX;
                    self.set_status(message);
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
                }
                AgentEvent::Round { current, limit } => {
                    self.agent_round = current;
                    self.agent_round_limit = limit;
                }
                AgentEvent::ConversationUpdated(conversation) => {
                    self.agent_conversation = conversation;
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
                    if self.permission_mode == PermissionMode::Bypass {
                        let _ = self.send_approval(ApprovalDecision::Approve);
                    } else {
                        self.set_status(format!("Approval required: {}", request.title));
                        self.approval_request = Some(request);
                        self.approval_scroll = 0;
                        self.set_overlay(Overlay::Approval);
                    }
                }
                AgentEvent::AskUser(request) => {
                    self.set_status(if request.kind == AskUserKind::RoundLimit {
                        "Agent reached its request-round limit"
                    } else {
                        "Agent is waiting for your answer"
                    });
                    self.ask_user_option = 0;
                    self.ask_user_input.clear();
                    self.ask_user_cursor = 0;
                    self.ask_user_request = Some(request);
                    self.set_overlay(Overlay::AskUser);
                }
                AgentEvent::Stopped(reason) => {
                    self.active_agent = None;
                    if self.ai_cancelling {
                        self.ai_cancelling = false;
                        self.ai_cancel = None;
                        continue;
                    }
                    self.ai_running = false;
                    self.ai_cancel = None;
                    self.agent_scroll = u16::MAX;
                    let (notification, status) = match reason {
                        AgentStopReason::RequestRoundLimit => (
                            "Agent stopped at the request-round limit",
                            "Agent paused at the request-round limit",
                        ),
                        AgentStopReason::ToolApprovalDenied => (
                            "Agent stopped after tool approval was denied",
                            "Agent stopped after tool approval was denied",
                        ),
                    };
                    self.notifications.notify(notification);
                    self.set_status(status);
                    self.clear_ask_user();
                    self.reload_workspace();
                    if let Err(error) = self.persist_agent_session() {
                        self.set_error(format!("Agent session save error: {error}"));
                    }
                    let pending = self
                        .agent_input_buffer
                        .lock()
                        .map(|mut buffer| std::mem::take(&mut *buffer))
                        .unwrap_or_default();
                    if !pending.is_empty() {
                        self.mark_buffered_prompts_consumed(pending.len());
                        self.start_agent_worker(pending.join("\n\n"));
                    }
                }
                AgentEvent::Finished(result) => {
                    self.active_agent = None;
                    if self.ai_cancelling {
                        self.ai_cancelling = false;
                        self.ai_cancel = None;
                        continue;
                    }
                    let completed_successfully = result.is_ok();
                    self.ai_running = false;
                    self.ai_cancel = None;
                    match result {
                        Ok(_) => {
                            self.agent_scroll = u16::MAX;
                            self.notifications.notify("Agent finished");
                            self.set_status("Agent finished");
                        }
                        Err(error) => {
                            for entry in &mut self.agent_panel {
                                if let AgentPanelEntry::Assistant { streaming, .. } = entry {
                                    *streaming = false;
                                }
                            }
                            self.agent_panel
                                .push(AgentPanelEntry::Error(format!("Agent failed: {error}")));
                            self.agent_scroll = u16::MAX;
                            self.set_error(format!("AI error: {error}"));
                        }
                    }
                    self.clear_ask_user();
                    self.reload_workspace();
                    if let Err(error) = self.persist_agent_session() {
                        self.set_error(format!("Agent session save error: {error}"));
                    }
                    if completed_successfully {
                        let pending = self
                            .agent_input_buffer
                            .lock()
                            .map(|mut buffer| std::mem::take(&mut *buffer))
                            .unwrap_or_default();
                        if !pending.is_empty() {
                            self.mark_buffered_prompts_consumed(pending.len());
                            self.start_agent_worker(pending.join("\n\n"));
                        }
                    }
                }
            }
        }
        if disconnected && self.ai_running {
            self.ai_running = false;
            self.ai_cancel = None;
            self.agent_panel.push(AgentPanelEntry::Error(
                "Agent worker stopped unexpectedly".to_string(),
            ));
            self.agent_scroll = u16::MAX;
            self.clear_ask_user();
            self.set_error("AI error: worker stopped unexpectedly");
        }
    }

    pub fn apply_workspace_index(&mut self, index: WorkspaceIndex) {
        self.workspace_index.replace(index);
        if self.status == "Workspace index is still building" {
            self.status.clear();
        }
        if self.center_view == CenterView::Search && !self.search_query.trim().is_empty() {
            self.recompute_search();
        } else if self.center_view == CenterView::Tags {
            self.recompute_tags();
        }
    }

    pub fn invalidate_agent_reads(&mut self, paths: &[PathBuf]) {
        if let Err(error) = self.agent_worker.invalidate_reads(paths) {
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
            self.storage.root.join(path)
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
        let Ok(path) = self.storage.daily_file_path(&date.to_string()) else {
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
            .strip_prefix(&self.storage.root)
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
            self.input.clear();
            self.input_cursor = 0;
        }
    }

    pub(super) fn buffer_agent_prompt(&mut self, prompt: String, display_prompt: String) -> bool {
        let queued = {
            match self.agent_input_buffer.lock() {
                Ok(mut buffer) => {
                    buffer.push(prompt);
                    true
                }
                Err(_) => false,
            }
        };
        if !queued {
            self.set_error("Agent input buffer is unavailable");
            return false;
        }
        self.agent_panel.push(AgentPanelEntry::Prompt {
            text: display_prompt,
            muted: true,
        });
        self.agent_scroll = u16::MAX;
        self.set_status("Prompt buffered for Agent");
        true
    }

    pub(super) fn compose_agent_prompt(&self) -> Option<String> {
        let content = self.input.trim();
        if content.is_empty() {
            return None;
        }
        let document = self
            .document
            .as_ref()
            .filter(|_| self.center_view == CenterView::Document)
            .and_then(|document| match &document.kind {
                DocumentKind::File(path) => Some(("note", path)),
                DocumentKind::Skill(path) => Some(("skill", path)),
                DocumentKind::Daily(_) => None,
            });
        Some(if let Some((kind, path)) = document {
            let display = path
                .strip_prefix(&self.storage.root)
                .unwrap_or(path)
                .to_string_lossy();
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
        self.agent_panel.push(AgentPanelEntry::Prompt {
            text: display_prompt,
            muted: false,
        });
        self.agent_scroll = u16::MAX;
        self.start_agent_worker(prompt)
    }

    pub(super) fn start_agent_worker(&mut self, prompt: String) -> bool {
        if self.ai_running || self.ai_cancelling {
            self.set_status("AI is already working");
            return false;
        }
        if let Ok(mut buffer) = self.agent_input_buffer.lock() {
            buffer.clear();
        }
        self.ai_running = true;
        self.agent_round = 0;
        self.agent_round_limit = 0;
        self.set_status("AI is working...");
        let cancelled = self.agent_worker.cancellation_token();
        cancelled.store(false, Ordering::Relaxed);
        self.ai_cancel = Some(cancelled.clone());
        match self
            .agent_worker
            .run(prompt, self.agent_conversation.clone())
        {
            Ok(observable) => {
                self.active_agent = Some(observable);
                true
            }
            Err(error) => {
                self.ai_running = false;
                self.ai_cancel = None;
                if agent_debug_logging_enabled() {
                    eprintln!("[nole debug] Agent error: {error:#}");
                }
                self.set_error(format!("AI error: {error:#}"));
                false
            }
        }
    }

    pub(super) fn mark_buffered_prompts_consumed(&mut self, count: usize) {
        for muted in self
            .agent_panel
            .iter_mut()
            .filter_map(|entry| match entry {
                AgentPanelEntry::Prompt { muted, .. } if *muted => Some(muted),
                _ => None,
            })
            .take(count)
        {
            *muted = false;
        }
    }

    pub(super) fn cancel_agent(&mut self) {
        if !self.ai_running {
            self.set_status("Agent is not running");
            return;
        }
        if let Some(cancelled) = self.ai_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        if let Some(observable) = &self.active_agent {
            observable.cancel.cancel();
        }
        self.ai_running = false;
        self.ai_cancelling = true;
        if let Ok(mut buffer) = self.agent_input_buffer.lock() {
            buffer.clear();
        }
        self.approval_request = None;
        self.clear_ask_user();
        if self.overlay == Some(Overlay::Approval) {
            self.overlay = None;
        }
        for entry in &mut self.agent_panel {
            if let AgentPanelEntry::Assistant { streaming, .. } = entry {
                *streaming = false;
            }
        }
        for entry in self.agent_panel.iter_mut().rev() {
            if let AgentPanelEntry::Tool { active, .. } = entry {
                if *active {
                    *active = false;
                }
            }
        }
        self.agent_panel
            .push(AgentPanelEntry::Error("Cancelled".to_string()));
        self.agent_scroll = u16::MAX;
        self.notifications.notify("Agent task cancelled");
        self.set_status("Agent task cancelled");
    }

    pub(super) fn clear_agent_session(&mut self) {
        let was_running = self.ai_running;
        if was_running {
            self.cancel_agent();
        }
        let had_saved_session = match self.storage.clear_agent_session() {
            Ok(had_saved_session) => had_saved_session,
            Err(error) => {
                self.set_error(format!("Agent session clear error: {error}"));
                return;
            }
        };
        let had_history = self.agent_conversation.clear();
        if let Err(error) = self.agent_worker.clear_read_state() {
            self.set_error(format!("Agent state clear error: {error:#}"));
        }
        let had_panel_content = !self.agent_panel.is_empty();
        self.agent_panel.clear();
        if let Ok(mut buffer) = self.agent_input_buffer.lock() {
            buffer.clear();
        }
        self.agent_scroll = 0;
        self.agent_usage = TokenUsage::default();
        self.agent_context_window = 0;
        self.agent_context_capacity = 0;
        self.agent_timed_output_tokens = 0;
        self.agent_response_duration = Duration::ZERO;
        self.agent_retry_count = 0;
        self.agent_round = 0;
        self.agent_round_limit = 0;
        if was_running || had_saved_session || had_history || had_panel_content {
            self.set_status("Agent session cleared");
        } else {
            self.set_status("Agent session is already empty");
        }
    }

    pub(super) fn persist_agent_session(&self) -> anyhow::Result<()> {
        let session = AgentSession::from_parts(
            &self.agent_conversation,
            &self.agent_panel,
            self.agent_usage,
            self.agent_timed_output_tokens,
            self.agent_response_duration,
        );
        self.storage.write_agent_session(&session)
    }

    pub(super) fn send_user_response(&mut self, response: AskUserResponse) -> anyhow::Result<()> {
        let round_limit = self
            .ask_user_request
            .as_ref()
            .is_some_and(|request| request.kind == AskUserKind::RoundLimit);
        let sender = self
            .ai_user_sender
            .as_ref()
            .context("Agent user-response channel is unavailable")?;
        sender
            .send(response.clone())
            .context("sending response to Agent")?;
        if !round_limit {
            if let AskUserResponse::Answer(answer) = &response {
                let answer = answer.split_whitespace().collect::<Vec<_>>().join(" ");
                if !answer.is_empty() {
                    if let Some(AgentPanelEntry::Tool { text, active: true }) =
                        self.agent_panel.iter_mut().rev().find(|entry| {
                            matches!(
                                entry,
                                AgentPanelEntry::Tool { text, active: true }
                                    if text.starts_with("Calling Ask User...")
                            )
                        })
                    {
                        if text.lines().count() < 3 {
                            text.push('\n');
                            text.push_str(&answer);
                            self.agent_scroll = u16::MAX;
                        }
                    }
                }
            }
        }
        self.set_status(if round_limit {
            match &response {
                AskUserResponse::Answer(answer) if answer == "Continue" => "Agent continuing",
                _ => "Agent stopping at the request-round limit",
            }
        } else {
            match response {
                AskUserResponse::Answer(_) => "Answer sent to Agent",
                AskUserResponse::Cancelled => "Agent question cancelled",
            }
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

    pub(super) fn send_approval(&mut self, decision: ApprovalDecision) -> anyhow::Result<()> {
        let sender = self
            .ai_approval_sender
            .as_ref()
            .context("Agent approval channel is unavailable")?;
        sender
            .send(decision)
            .context("sending Agent approval decision")?;
        self.set_status(match decision {
            ApprovalDecision::Approve => "Change approved",
            ApprovalDecision::Deny => "Change denied",
        });
        self.approval_request = None;
        if self.overlay == Some(Overlay::Approval) {
            self.overlay = None;
        }
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.purpose == DialogPurpose::AgentApproval)
        {
            self.dialog = None;
        }
        Ok(())
    }

    pub(super) fn toggle_permission_mode(&mut self) {
        self.permission_mode = self.permission_mode.toggled();
        self.permission_bypass.store(
            self.permission_mode == PermissionMode::Bypass,
            Ordering::Relaxed,
        );
        if self.permission_mode == PermissionMode::Bypass && self.overlay == Some(Overlay::Approval)
        {
            let _ = self.send_approval(ApprovalDecision::Approve);
        }
        self.set_status(format!("Permission mode: {}", self.permission_mode.label()));
    }
}
