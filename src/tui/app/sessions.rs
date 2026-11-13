//! Selecting and managing Session notes while their runtimes keep working.
use super::*;
use uuid::Uuid;

impl App {
    pub(in crate::tui::app) fn selected_agent_session(&self) -> Option<Uuid> {
        match self.visible_file_rows().get(self.file_row) {
            Some(FileListRow::Agent(id)) => Some(*id),
            _ => None,
        }
    }

    pub(crate) fn open_agent_session(&mut self, id: Uuid) {
        let runtime = match self.sessions.get(id) {
            Ok(runtime) => runtime,
            Err(error) => {
                self.set_error(error.to_string());
                return;
            }
        };
        // Subscribe before reading the snapshot; attach serializes both with
        // the recorder so stream deltas begin after this snapshot.
        let (panel, scope, events) = runtime.attach();
        let (_, _, usage, tokens, duration) = scope.into_parts();
        self.agent = runtime.service.clone();
        self.agent_session = runtime.session.clone();
        self.agent_events_rx = events;
        self.ai_running = runtime.session.is_running();
        self.ai_cancelling = false;
        self.agent_panel = panel;
        self.agent_usage = usage;
        self.agent_timed_output_tokens = tokens;
        self.agent_response_duration = duration;
        self.agent_scroll = 0;
        self.agent_stats_scroll = 0;
        self.agent_follow_tail = true;
        self.agent_context_window = 0;
        self.agent_context_capacity = 0;
        self.agent_round = 0;
        self.agent_retry_count = 0;
        let (approval, question, private_input) = runtime.pending_interactions();
        self.approval_request = approval;
        self.ask_user_request = question;
        self.private_terminal_input_request = private_input;
        self.session_note = runtime;
        self.center_view = CenterView::Chat;
        self.focus = Focus::Center;
        self.close_dialog();
        self.private_terminal_input.zeroize();
        self.private_terminal_input_cursor = 0;
        self.ask_user_input.clear();
        self.ask_user_cursor = 0;
        self.ask_user_option = 0;
        self.ai_prompt_input.clear();
        self.ai_prompt_cursor = 0;
        if let Err(error) = self.session_note.mark_read() {
            self.set_error(error.to_string());
        }
        self.agent_notes = self.sessions.list();
        self.restore_session_interaction();
    }

    fn restore_session_interaction(&mut self) {
        if self.approval_request.is_some() {
            self.set_overlay(Overlay::Approval);
        } else if self.ask_user_request.is_some() {
            self.set_overlay(Overlay::AskUser);
        } else if self.private_terminal_input_request.is_some() {
            self.set_overlay(Overlay::PrivateTerminalInput);
        }
    }

    pub(in crate::tui::app) fn begin_new_agent_session(&mut self) {
        self.pending_agent_session = None;
        self.open_dialog(DialogState::new(
            "New Agent session",
            "Name",
            DialogMode::SingleLine,
            DialogPurpose::NewAgentSession,
            Vec::new(),
        ));
    }

    pub(in crate::tui::app) fn rename_agent_session(&mut self, id: Uuid) {
        let Ok(runtime) = self.sessions.get(id) else {
            return;
        };
        self.pending_agent_session = Some(id);
        let mut dialog = DialogState::new(
            "Rename Agent session",
            "Name",
            DialogMode::SingleLine,
            DialogPurpose::RenameAgentSession,
            Vec::new(),
        );
        dialog.input = runtime.summary().title;
        dialog.cursor = dialog.input.chars().count();
        self.open_dialog(dialog);
    }

    pub(in crate::tui::app) fn delete_agent_session(&mut self, id: Uuid) {
        let Ok(runtime) = self.sessions.get(id) else {
            return;
        };
        self.pending_agent_session = Some(id);
        self.open_dialog(DialogState::new(
            "Delete Agent session",
            format!("Delete {}?", runtime.summary().title),
            DialogMode::Confirm,
            DialogPurpose::DeleteAgentSession,
            Vec::new(),
        ));
    }

    pub(in crate::tui::app) fn handle_session_dialog(&mut self, key: KeyEvent) -> Option<Command> {
        let dialog = self.dialog.as_ref()?.clone();
        if key.code == KeyCode::Esc
            || (dialog.purpose == DialogPurpose::DeleteAgentSession
                && matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N')))
        {
            self.pending_agent_session = None;
            self.close_dialog();
            return None;
        }
        if key.code != KeyCode::Enter
            && !(dialog.purpose == DialogPurpose::DeleteAgentSession
                && matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')))
        {
            if dialog.mode == DialogMode::SingleLine {
                self.edit_dialog_single_line(key);
            }
            return None;
        }
        let outcome = match dialog.purpose {
            DialogPurpose::NewAgentSession => self
                .sessions
                .create(&dialog.input)
                .map(|runtime| Some(runtime.summary().id)),
            DialogPurpose::RenameAgentSession => self
                .pending_agent_session
                .ok_or_else(|| anyhow::anyhow!("Choose a Session"))
                .and_then(|id| self.sessions.get(id)?.rename(&dialog.input))
                .map(|_| None),
            DialogPurpose::DeleteAgentSession => self
                .pending_agent_session
                .ok_or_else(|| anyhow::anyhow!("Choose a Session"))
                .and_then(|id| {
                    self.sessions.delete(id)?;
                    if self.session_note.summary().id == id {
                        let next = self.sessions.list().first().map(|note| note.id);
                        match next {
                            Some(id) => Ok(Some(id)),
                            None => Ok(None),
                        }
                    } else {
                        Ok(None)
                    }
                }),
            _ => Ok(None),
        };
        match outcome {
            Ok(id) => {
                self.pending_agent_session = None;
                self.close_dialog();
                self.agent_notes = self.sessions.list();
                if let Some(id) = id {
                    self.open_agent_session(id);
                } else if self.sessions.get(self.session_note.summary().id).is_err() {
                    if let Ok(draft) = self.sessions.draft("New session") {
                        self.agent = draft.service.clone();
                        self.agent_session = draft.session.clone();
                        self.agent_events_rx = draft.subscribe();
                        self.session_note = draft;
                        self.ai_running = false;
                        self.ai_cancelling = false;
                        self.agent_panel.clear();
                        self.agent_usage = TokenUsage::default();
                        self.agent_response_duration = Duration::ZERO;
                        self.agent_timed_output_tokens = 0;
                        self.clear_ask_user();
                        self.clear_private_terminal_input();
                        self.approval_request = None;
                    }
                }
                self.ensure_visible_file_selection();
            }
            Err(error) => self.set_error(error.to_string()),
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_session::AgentPanelEntry;
    #[test]
    fn switching_sessions_preserves_active_tool_identity_and_clears_only_the_opened_unread_marker()
    {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new_for_tests(storage).unwrap();
        let a = app.sessions.create("Research").unwrap();
        let b = app.sessions.create("Writing").unwrap();
        let a_id = a.summary().id;
        let b_id = b.summary().id;
        app.open_agent_session(a_id);
        a.observe(&AgentEvent::ToolStarted {
            id: "read-1".into(),
            message: "Reading".into(),
        });
        app.poll_agent();
        app.open_agent_session(b_id);
        a.observe(&AgentEvent::ToolUpdated {
            id: "read-1".into(),
            message: "Reading more".into(),
        });
        app.open_agent_session(a_id);
        a.observe(&AgentEvent::ToolFinished {
            id: "read-1".into(),
            message: "Read complete".into(),
            preview: None,
        });
        app.poll_agent();
        let tools = app
            .agent_panel
            .snapshot()
            .into_iter()
            .filter(|entry| matches!(entry, AgentPanelEntry::Tool { .. }))
            .collect::<Vec<_>>();
        assert_eq!(tools.len(), 1);
        assert!(
            matches!(&tools[0], AgentPanelEntry::Tool { active: false, text, .. } if text == "Read complete")
        );
        app.open_agent_session(b_id);
        a.observe(&AgentEvent::Finished(Ok("Done A".into())));
        b.observe(&AgentEvent::Finished(Ok("Done B".into())));
        assert!(a.summary().unread && b.summary().unread);
        app.open_agent_session(a_id);
        assert!(!a.summary().unread);
        assert!(b.summary().unread);
        a.observe(&AgentEvent::Finished(Ok("More A".into())));
        assert!(a.summary().unread);
        app.terminal_focused = false;
        app.poll_agent();
        assert!(a.summary().unread);
        app.terminal_focused = true;
        app.poll_agent();
        assert!(!a.summary().unread);
    }
    #[test]
    fn deleting_the_last_session_leaves_the_agents_category_empty_and_allows_a_new_conversation() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new_for_tests(storage).unwrap();
        assert!(app.sessions.list().is_empty());
        let note = app.sessions.create("Delete").unwrap();
        let id = note.summary().id;
        app.open_agent_session(id);
        app.delete_agent_session(id);
        app.handle_session_dialog(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.sessions.list().is_empty());
        assert!(app.agent_panel.is_empty());
        assert_ne!(app.session_note.summary().id, id);
        app.sessions.retain(app.session_note.clone()).unwrap();
        assert_eq!(app.sessions.list().len(), 1);
    }
}
