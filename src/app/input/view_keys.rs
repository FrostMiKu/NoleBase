//! Keyboard and mouse input: view_keys.

use super::super::*;

impl App {
    pub(in crate::app) fn handle_compose(&mut self, key: KeyEvent) -> Option<Command> {
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Enter if modifiers.contains(KeyModifiers::CONTROL) => {
                self.submit_compose_to_agent();
                None
            }
            KeyCode::Enter if modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                insert_char(&mut self.input, &mut self.input_cursor, '\n');
                None
            }
            KeyCode::Enter => {
                if self.center_view == CenterView::Chat {
                    self.submit_compose_to_agent();
                } else {
                    self.send_message();
                }
                None
            }
            KeyCode::Tab | KeyCode::Esc => {
                self.focus = Focus::Center;
                None
            }
            KeyCode::Backspace => {
                delete_backward(&mut self.input, &mut self.input_cursor);
                None
            }
            KeyCode::Delete => {
                delete_forward(&mut self.input, &mut self.input_cursor);
                None
            }
            KeyCode::Left => self.move_input_cursor(CursorMove::Left),
            KeyCode::Right => self.move_input_cursor(CursorMove::Right),
            KeyCode::Up => self.move_input_cursor(CursorMove::Up),
            KeyCode::Down => self.move_input_cursor(CursorMove::Down),
            KeyCode::Home => self.move_input_cursor(CursorMove::LineStart),
            KeyCode::End => self.move_input_cursor(CursorMove::LineEnd),
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                insert_char(&mut self.input, &mut self.input_cursor, '\n');
                None
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.recall_last_append();
                None
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                if self.input.is_empty() {
                    Some(Command::Quit)
                } else {
                    self.input.clear();
                    self.input_cursor = 0;
                    None
                }
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                insert_char(&mut self.input, &mut self.input_cursor, character);
                None
            }
            _ => None,
        }
    }

    pub(in crate::app) fn move_input_cursor(&mut self, movement: CursorMove) -> Option<Command> {
        self.input_cursor = move_cursor(&self.input, self.input_cursor, movement);
        None
    }

    pub(in crate::app) fn handle_daily(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            code if is_cancel_key(code) => Some(Command::Quit),
            KeyCode::Tab | KeyCode::Char('i') | KeyCode::Enter => {
                self.focus = Focus::Compose;
                None
            }
            code if is_down_key(code) => {
                self.move_daily_selection(1);
                None
            }
            code if is_up_key(code) => {
                self.move_daily_selection(-1);
                None
            }
            code if is_left_key(code) => {
                self.open_files();
                None
            }
            code if is_right_key(code) => {
                self.open_workspace_views();
                None
            }
            KeyCode::Char('g') => {
                self.selected = 0;
                self.reveal_selected_daily = true;
                None
            }
            KeyCode::Char('G') => {
                self.selected = self.daily_notes.len().saturating_sub(1);
                self.reveal_selected_daily = true;
                None
            }
            KeyCode::PageDown => {
                self.reveal_selected_daily = false;
                self.scroll = self.scroll.saturating_add(DAILY_PAGE_STEP);
                None
            }
            KeyCode::PageUp => {
                self.reveal_selected_daily = false;
                self.scroll = self.scroll.saturating_sub(DAILY_PAGE_STEP);
                None
            }
            KeyCode::Char('/') => {
                self.open_search();
                None
            }
            KeyCode::Char('m') => self.act(Action::Move),
            KeyCode::Char('n') => self.act(Action::New),
            KeyCode::Char('v') => self.act(Action::View),
            KeyCode::Char('e') => self.act(Action::Edit),
            KeyCode::Char('d') => self.act(Action::Delete),
            KeyCode::Char('u') => {
                self.undo();
                None
            }
            _ => None,
        }
    }

    pub(in crate::app) fn handle_todo(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.activate_workspace_view(CenterView::Daily);
                None
            }
            KeyCode::Down => {
                self.move_todo_selection(1);
                None
            }
            KeyCode::Up => {
                self.move_todo_selection(-1);
                None
            }
            KeyCode::Enter => {
                if self.visible_todo_indices().contains(&self.todo_index) {
                    self.toggle_todo(self.todo_index);
                }
                None
            }
            _ => {
                let edit = edit_single_line(&mut self.todo_query, &mut self.todo_cursor, key);
                if edit.changed() {
                    self.ensure_visible_todo_selection();
                    self.todo_list_start = 0;
                }
                None
            }
        }
    }

    pub(in crate::app) fn handle_chat(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            code if is_cancel_key(code) => {
                self.activate_workspace_view(CenterView::Daily);
                None
            }
            KeyCode::Tab | KeyCode::Char('i') | KeyCode::Enter => {
                self.focus = Focus::Compose;
                None
            }
            code if is_up_key(code) => {
                self.scroll_agent_by(-1);
                None
            }
            code if is_down_key(code) => {
                self.scroll_agent_by(1);
                None
            }
            KeyCode::PageUp => {
                self.scroll_agent_by(-i32::from(AGENT_PAGE_STEP));
                None
            }
            KeyCode::PageDown => {
                self.scroll_agent_by(i32::from(AGENT_PAGE_STEP));
                None
            }
            code if is_left_key(code) => {
                self.open_files();
                None
            }
            code if is_right_key(code) => {
                self.open_workspace_views();
                None
            }
            KeyCode::Char('C') => self.execute_app_command(AppCommand::ClearAgentSession),
            KeyCode::Char('c') if self.ai_running => {
                self.execute_app_command(AppCommand::InterruptAgent)
            }
            _ => None,
        }
    }

    pub(in crate::app) fn handle_workspace_views(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            code if is_cancel_key(code) || is_left_key(code) => {
                self.focus = Focus::Center;
                None
            }
            code if is_up_key(code) && self.workspace_view_index == 0 => {
                self.focus = Focus::Agent;
                None
            }
            code if is_up_key(code) => {
                self.move_workspace_view_selection(-1);
                None
            }
            code if is_down_key(code) => {
                self.move_workspace_view_selection(1);
                None
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let view = WorkspaceView::ALL
                    .get(self.workspace_view_index)
                    .map(|view| view.center_view);
                if let Some(view) = view {
                    self.activate_workspace_view(view);
                }
                None
            }
            _ => None,
        }
    }

    pub(in crate::app) fn handle_agent(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('C') => self.execute_app_command(AppCommand::ClearAgentSession),
            code if is_cancel_key(code) || is_left_key(code) => {
                self.focus = Focus::Center;
                None
            }
            code if is_up_key(code) && self.agent_scroll == 0 => {
                self.focus = Focus::Center;
                None
            }
            code if is_up_key(code) => {
                self.scroll_agent_by(-1);
                None
            }
            code if is_down_key(code) => {
                self.scroll_agent_by(1);
                None
            }
            KeyCode::PageUp => {
                self.scroll_agent_by(-i32::from(AGENT_PAGE_STEP));
                None
            }
            KeyCode::PageDown => {
                self.scroll_agent_by(i32::from(AGENT_PAGE_STEP));
                None
            }
            KeyCode::Char('c') if self.ai_running => {
                self.execute_app_command(AppCommand::InterruptAgent)
            }
            _ => None,
        }
    }

    pub(in crate::app) fn handle_search(&mut self, key: KeyEvent) -> Option<Command> {
        let document_search = self.center_view == CenterView::DocumentSearch;
        match key.code {
            KeyCode::Esc => {
                if document_search && self.document.is_some() {
                    self.center_view = CenterView::Document;
                    self.focus = Focus::Center;
                } else {
                    self.activate_workspace_view(CenterView::Daily);
                }
                None
            }
            KeyCode::Down => {
                self.move_search_selection(1);
                None
            }
            KeyCode::Up => {
                self.move_search_selection(-1);
                None
            }
            KeyCode::Enter => {
                self.jump_to_search_result(self.search_index);
                None
            }
            _ => {
                let edit = edit_single_line(&mut self.search_query, &mut self.search_cursor, key);
                if edit.changed() {
                    self.recompute_search();
                }
                None
            }
        }
    }

    pub(in crate::app) fn handle_tags(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                if self.active_tag.is_some() {
                    self.active_tag = None;
                    self.recompute_tags();
                } else if WorkspaceView::index_of(self.tags_return_view).is_some() {
                    self.activate_workspace_view(self.tags_return_view);
                } else {
                    self.center_view = self.tags_return_view;
                    self.focus = Focus::Center;
                }
                None
            }
            KeyCode::Down => {
                if self.active_tag.is_some() {
                    self.move_tag_note_selection(1);
                } else {
                    self.move_tag_selection(1);
                }
                None
            }
            KeyCode::Up => {
                if self.active_tag.is_some() {
                    self.move_tag_note_selection(-1);
                } else {
                    self.move_tag_selection(-1);
                }
                None
            }
            KeyCode::Enter => {
                if self.active_tag.is_some() {
                    self.open_tag_note_at(self.tag_note_index);
                } else if let Some(name) = self
                    .tag_results
                    .get(self.tag_index)
                    .map(|tag| tag.name.clone())
                {
                    self.open_tag_documents(&name);
                }
                None
            }
            _ => {
                if self.active_tag.is_none() {
                    let edit = edit_single_line(&mut self.tag_query, &mut self.tag_cursor, key);
                    if edit.changed() {
                        self.recompute_tags();
                    }
                }
                None
            }
        }
    }

    pub(in crate::app) fn handle_attachments(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.activate_workspace_view(CenterView::Daily);
                None
            }
            KeyCode::Down => {
                self.move_attachment_selection(1);
                None
            }
            KeyCode::Up => {
                self.move_attachment_selection(-1);
                None
            }
            KeyCode::Enter => self.open_attachment_at(self.attachment_index),
            KeyCode::Char('d') => {
                self.request_delete_attachment();
                None
            }
            _ => {
                let edit =
                    edit_single_line(&mut self.attachment_query, &mut self.attachment_cursor, key);
                if edit.changed() {
                    self.recompute_attachments();
                }
                None
            }
        }
    }

    pub(in crate::app) fn handle_document(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            code if is_cancel_key(code) => {
                self.close_document();
                None
            }
            code if is_down_key(code) => {
                self.scroll_document(1);
                None
            }
            code if is_up_key(code) => {
                self.scroll_document(-1);
                None
            }
            code if is_left_key(code) => {
                self.open_files();
                None
            }
            code if is_right_key(code) => {
                self.open_todo();
                None
            }
            KeyCode::PageDown => {
                self.scroll_document(DOCUMENT_PAGE_STEP as i32);
                None
            }
            KeyCode::PageUp => {
                self.scroll_document(-(DOCUMENT_PAGE_STEP as i32));
                None
            }
            KeyCode::Char('i') | KeyCode::Enter => {
                self.focus = Focus::Compose;
                None
            }
            KeyCode::Char('e') => match self.document.as_ref().map(|doc| &doc.kind) {
                Some(DocumentKind::File(path)) | Some(DocumentKind::Skill(path)) => {
                    Some(Command::Edit(path.clone()))
                }
                Some(DocumentKind::Daily(date)) => self.daily_edit_command(*date),
                None => None,
            },
            KeyCode::Char('a') if self.current_note_archived() == Some(false) => {
                self.manage_current_note(false);
                None
            }
            KeyCode::Char('u') if self.current_note_archived() == Some(true) => {
                self.manage_current_note(true);
                None
            }
            KeyCode::Char('d')
                if self.document.as_ref().is_some_and(|document| {
                    matches!(
                        document.kind,
                        DocumentKind::File(_) | DocumentKind::Skill(_)
                    )
                }) =>
            {
                self.delete_current_note();
                None
            }
            KeyCode::Char('r')
                if self.document.as_ref().is_some_and(|document| {
                    matches!(
                        document.kind,
                        DocumentKind::File(_) | DocumentKind::Skill(_)
                    )
                }) =>
            {
                self.rename_current_note();
                None
            }
            KeyCode::Char('/') => {
                self.open_document_search();
                None
            }
            _ => None,
        }
    }
}
