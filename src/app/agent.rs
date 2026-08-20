//! Agent coordination: the event loop, prompt dispatch, session lifecycle,
//! and approval / ask-user / permission handling.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::*;

impl App {
    pub(in crate::app) fn scroll_agent_by(&mut self, delta: i32) {
        self.agent_follow_tail = false;
        self.agent_scroll = if delta > 0 {
            self.agent_scroll.saturating_add(delta as u16)
        } else {
            self.agent_scroll
                .saturating_sub(delta.unsigned_abs() as u16)
        };
    }

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
                        Some(entry)
                            if matches!(
                                entry.as_ref(),
                                AgentPanelEntry::Assistant {
                                    streaming: true,
                                    ..
                                }
                            ) =>
                        {
                            if let AgentPanelEntry::Assistant { text, .. } = Arc::make_mut(entry) {
                                text.push_str(&delta);
                            }
                        }
                        _ => self.agent_panel.push(Arc::new(AgentPanelEntry::Assistant {
                            text: delta,
                            streaming: true,
                            final_output: false,
                        })),
                    }
                }
                AgentEvent::AssistantMessageFinished { text, final_output } => {
                    if text.trim().is_empty() {
                        self.agent_panel.retain(|entry| {
                            !matches!(
                                entry.as_ref(),
                                AgentPanelEntry::Assistant {
                                    streaming: true,
                                    ..
                                }
                            )
                        });
                    } else if let Some(entry) = self.agent_panel.iter_mut().rev().find(|entry| {
                        matches!(
                            entry.as_ref(),
                            AgentPanelEntry::Assistant {
                                streaming: true,
                                ..
                            }
                        )
                    }) {
                        if let AgentPanelEntry::Assistant {
                            text: entry_text,
                            streaming,
                            final_output: entry_final,
                        } = Arc::make_mut(entry)
                        {
                            *entry_text = text;
                            *streaming = false;
                            *entry_final = final_output;
                        }
                    } else {
                        self.agent_panel.push(Arc::new(AgentPanelEntry::Assistant {
                            text,
                            streaming: false,
                            final_output,
                        }));
                    }
                }
                AgentEvent::ThinkingDelta(delta) => {
                    if delta.is_empty() {
                        continue;
                    }
                    match self.agent_panel.last_mut() {
                        Some(entry)
                            if matches!(
                                entry.as_ref(),
                                AgentPanelEntry::Thinking {
                                    streaming: true,
                                    ..
                                }
                            ) =>
                        {
                            if let AgentPanelEntry::Thinking { text, .. } = Arc::make_mut(entry) {
                                text.push_str(&delta);
                            }
                        }
                        _ => self.agent_panel.push(Arc::new(AgentPanelEntry::Thinking {
                            text: delta,
                            streaming: true,
                        })),
                    }
                }
                AgentEvent::ThinkingFinished => {
                    if let Some(entry) = self.agent_panel.iter_mut().rev().find(|entry| {
                        matches!(
                            entry.as_ref(),
                            AgentPanelEntry::Thinking {
                                streaming: true,
                                ..
                            }
                        )
                    }) {
                        if let AgentPanelEntry::Thinking { streaming, .. } = Arc::make_mut(entry) {
                            *streaming = false;
                        }
                    }
                }
                AgentEvent::BufferedInputConsumed(count) => {
                    for followup in self
                        .agent_panel
                        .iter_mut()
                        .filter_map(|entry| match entry.as_ref() {
                            AgentPanelEntry::Prompt { muted: true, .. } => Some(entry),
                            _ => None,
                        })
                        .take(count)
                    {
                        if let AgentPanelEntry::Prompt { muted, .. } = Arc::make_mut(followup) {
                            *muted = false;
                        }
                    }
                }
                AgentEvent::ToolStarted { id, message } => {
                    let index = self.agent_panel.len();
                    self.agent_panel.push(Arc::new(AgentPanelEntry::Tool {
                        text: message.clone(),
                        active: true,
                        preview: None,
                    }));
                    self.active_agent_tools.insert(id, index);
                    self.set_status(message);
                }
                AgentEvent::ToolFinished {
                    id,
                    message,
                    preview,
                } => {
                    let index = self.active_agent_tools.remove(&id).or_else(|| {
                        self.agent_panel.iter().rposition(|entry| {
                            matches!(entry.as_ref(), AgentPanelEntry::Tool { active: true, .. })
                        })
                    });
                    let entry = index.and_then(|index| self.agent_panel.get_mut(index));
                    if let Some(entry) = entry {
                        if let AgentPanelEntry::Tool {
                            text,
                            active,
                            preview: entry_preview,
                        } = Arc::make_mut(entry)
                        {
                            let answer = (message.starts_with("Completed Ask.")
                                && text.starts_with("Calling Ask..."))
                            .then(|| text.lines().nth(2).map(str::to_string))
                            .flatten();
                            *text = answer
                                .map(|answer| format!("{message}\n{answer}"))
                                .unwrap_or_else(|| message.clone());
                            *active = false;
                            *entry_preview = preview;
                        }
                    } else {
                        self.agent_panel.push(Arc::new(AgentPanelEntry::Tool {
                            text: message.clone(),
                            active: false,
                            preview,
                        }));
                    }
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
                    self.agent_panel.retain(|entry| {
                        !matches!(
                            entry.as_ref(),
                            AgentPanelEntry::Assistant {
                                streaming: true,
                                ..
                            } | AgentPanelEntry::Thinking {
                                streaming: true,
                                ..
                            }
                        )
                    });
                }
                AgentEvent::Round { current, limit } => {
                    self.agent_round = current;
                    self.agent_round_limit = limit;
                    if self.active_agent_tools.is_empty() {
                        self.set_status("AI is working...");
                    }
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
                    // The gate is the single decision source: whatever lands
                    // here already needs an explicit user decision. The UI must
                    // not re-decide based on its permission mode.
                    self.set_status(format!("Approval required: {}", request.title));
                    self.approval_request = Some(request);
                    self.approval_scroll = 0;
                    self.set_overlay(Overlay::Approval);
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
                    self.deactivate_agent_tools();
                    if self.ai_cancelling {
                        self.ai_cancelling = false;
                        self.ai_cancel = None;
                        continue;
                    }
                    self.ai_running = false;
                    self.ai_cancel = None;
                    if reason == AgentStopReason::ToolApprovalDenied {
                        self.agent_terminal.terminate();
                    }
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
                    self.deactivate_agent_tools();
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
                            self.notifications.notify("Agent finished");
                            self.set_status("Agent finished");
                        }
                        Err(error) => {
                            for entry in &mut self.agent_panel {
                                if let AgentPanelEntry::Assistant { streaming, .. } =
                                    Arc::make_mut(entry)
                                {
                                    *streaming = false;
                                }
                            }
                            self.agent_panel
                                .push(Arc::new(AgentPanelEntry::Error(format!(
                                    "Agent failed: {error}"
                                ))));
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
            self.agent_terminal.terminate();
            self.ai_running = false;
            self.ai_cancel = None;
            self.agent_panel.push(Arc::new(AgentPanelEntry::Error(
                "Agent worker stopped unexpectedly".to_string(),
            )));
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

    pub fn apply_attachment_index(
        &mut self,
        revision: u64,
        index: crate::attachment_index::AttachmentReferenceIndex,
    ) {
        self.attachment_usage.publish_snapshot(revision, index);
        if self.center_view == CenterView::Attachments {
            self.recompute_attachments();
        }
    }

    pub fn apply_wiki_link_index(&mut self, index: crate::wiki_link_index::WikiLinkIndex) {
        self.wiki_links.replace(index);
        self.recompute_document_backlinks();
    }

    /// Recompute [`App::document_backlinks`] from the published wiki-link index
    /// for the currently open managed note (daily, data, or archives). Skills
    /// and previews have no managed backing note, so they never get backlinks.
    pub(in crate::app) fn recompute_document_backlinks(&mut self) {
        self.document_backlinks.clear();
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let path = match &document.kind {
            DocumentKind::File(path) => Some(path.clone()),
            DocumentKind::Daily(date) => self.storage.daily_file_path(&date.to_string()).ok(),
            DocumentKind::Skill(_) => None,
        };
        let Some(path) = path else {
            return;
        };
        if let Some(backlinks) = self.wiki_links.with_index(|index| index.backlinks(&path)) {
            self.document_backlinks = backlinks;
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
        self.agent_panel.push(Arc::new(AgentPanelEntry::Prompt {
            text: display_prompt,
            muted: true,
        }));
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
                    self.storage
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
                        .storage
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
                .strip_prefix(&self.storage.root)
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
        self.agent_panel.push(Arc::new(AgentPanelEntry::Prompt {
            text: display_prompt,
            muted: false,
        }));
        self.agent_follow_tail = true;
        self.start_agent_worker(prompt)
    }

    pub(super) fn start_agent_worker(&mut self, prompt: String) -> bool {
        if self.ai_running || self.ai_cancelling {
            self.set_status("AI is already working");
            return false;
        }
        self.reload_thinking_display_config();
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
        for entry in self
            .agent_panel
            .iter_mut()
            .filter_map(|entry| match entry.as_ref() {
                AgentPanelEntry::Prompt { muted: true, .. } => Some(entry),
                _ => None,
            })
            .take(count)
        {
            if let AgentPanelEntry::Prompt { muted, .. } = Arc::make_mut(entry) {
                *muted = false;
            }
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
        self.agent_terminal.terminate();
        if let Ok(mut buffer) = self.agent_input_buffer.lock() {
            buffer.clear();
        }
        self.approval_request = None;
        self.clear_ask_user();
        if self.overlay == Some(Overlay::Approval) {
            self.overlay = None;
        }
        for entry in &mut self.agent_panel {
            match Arc::make_mut(entry) {
                AgentPanelEntry::Assistant { streaming, .. }
                | AgentPanelEntry::Thinking { streaming, .. } => *streaming = false,
                _ => {}
            }
        }
        self.deactivate_agent_tools();
        self.agent_panel
            .push(Arc::new(AgentPanelEntry::Error("Cancelled".to_string())));
        self.notifications.notify("Agent task cancelled");
        self.set_status("Agent task cancelled");
    }

    fn deactivate_agent_tools(&mut self) {
        self.active_agent_tools.clear();
        for entry in &mut self.agent_panel {
            if let AgentPanelEntry::Tool { active, .. } = Arc::make_mut(entry) {
                *active = false;
            }
        }
    }

    pub(super) fn clear_agent_session(&mut self) {
        let was_running = self.ai_running;
        if was_running {
            self.cancel_agent();
        }
        self.agent_terminal.terminate();
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
        self.active_agent_tools.clear();
        if let Ok(mut buffer) = self.agent_input_buffer.lock() {
            buffer.clear();
        }
        self.agent_scroll = 0;
        self.agent_follow_tail = false;
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
            &self
                .agent_panel
                .iter()
                .map(|entry| entry.as_ref().clone())
                .collect::<Vec<_>>(),
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
                    if let Some(entry) = self.agent_panel.iter_mut().rev().find(|entry| {
                        matches!(
                            entry.as_ref(),
                            AgentPanelEntry::Tool {
                                text,
                                active: true,
                                ..
                            } if text.starts_with("Calling Ask...")
                        )
                    }) {
                        if let AgentPanelEntry::Tool { text, .. } = Arc::make_mut(entry) {
                            if text.lines().count() < 3 {
                                text.push('\n');
                                text.push_str(&answer);
                            }
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
        if decision == ApprovalDecision::Deny {
            self.agent_terminal.terminate();
        }
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
        self.permission_mode = self.permission_mode.cycled();
        self.permission_mode_atomic
            .store(self.permission_mode.code(), Ordering::Relaxed);
        // Entering YOLO approves whatever approval is currently waiting.
        // Entering AUTO or APPROVE never blindly approves a pending request —
        // the gate decides whether it still needs a user decision.
        if self.permission_mode == PermissionMode::Yolo && self.overlay == Some(Overlay::Approval) {
            let _ = self.send_approval(ApprovalDecision::Approve);
        }
        self.set_status(format!("Permission mode: {}", self.permission_mode.label()));
    }
}
