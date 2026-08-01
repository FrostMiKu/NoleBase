//! Keyboard and mouse input: dispatch.

use super::super::*;

impl App {
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Command> {
        if is_terminal_toggle(key) {
            self.toggle_terminal();
            return None;
        }
        if self.overlay == Some(Overlay::Terminal) {
            self.write_terminal_key(key);
            return None;
        }
        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.purpose == DialogPurpose::CommandPalette)
            {
                self.close_dialog();
                self.command_matches.clear();
            } else if self.overlay.is_none() {
                self.open_command_palette();
            }
            return None;
        }
        if key.code == KeyCode::Tab {
            self.toggle_permission_mode();
            return None;
        }
        if self.overlay.is_some() {
            return self.handle_overlay(key);
        }

        // Pane shortcuts are global outside text-entry contexts.
        if !self.is_text_entry() {
            match key.code {
                KeyCode::Char('?') => {
                    self.open_help();
                    return None;
                }
                KeyCode::Char('f') => {
                    self.open_files();
                    return None;
                }
                KeyCode::Char('t') => {
                    self.open_todo();
                    return None;
                }
                KeyCode::Char('#') => {
                    self.open_tags();
                    return None;
                }
                _ => {}
            }
        }

        match self.focus {
            Focus::Compose => self.handle_compose(key),
            Focus::Files => self.handle_files(key),
            Focus::Views => self.handle_workspace_views(key),
            Focus::Agent => self.handle_agent(key),
            Focus::Center => match self.center_view {
                CenterView::Daily => self.handle_daily(key),
                CenterView::Chat => self.handle_chat(key),
                CenterView::Todo => self.handle_todo(key),
                CenterView::Document => self.handle_document(key),
                CenterView::Search | CenterView::DocumentSearch => self.handle_search(key),
                CenterView::Tags => self.handle_tags(key),
            },
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        if self.overlay == Some(Overlay::Terminal) {
            self.write_terminal_paste(text);
            return;
        }
        if self.overlay.is_some() {
            let purpose = self.dialog.as_ref().map(|dialog| dialog.purpose);
            let mode = self.dialog.as_ref().map(|dialog| dialog.mode);
            if matches!(
                purpose,
                Some(
                    DialogPurpose::AgentPrompt
                        | DialogPurpose::AskUser
                        | DialogPurpose::NewFile
                        | DialogPurpose::RenameFile
                        | DialogPurpose::TagRenameTarget
                        | DialogPurpose::CommandPalette
                )
            ) {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                if matches!(purpose, Some(DialogPurpose::AskUser)) {
                    self.select_custom_dialog_option();
                }
                if let Some(dialog) = self.dialog.as_mut() {
                    let text = if mode == Some(DialogMode::SingleLine)
                        || purpose == Some(DialogPurpose::CommandPalette)
                    {
                        text.replace('\n', "")
                    } else {
                        text
                    };
                    paste_into(&mut dialog.input, &mut dialog.cursor, &text);
                }
                self.sync_dialog_owner_state();
                if purpose == Some(DialogPurpose::CommandPalette) {
                    self.refresh_command_palette();
                }
            }
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match (self.focus, self.center_view, self.files_context) {
            (Focus::Compose, CenterView::Daily | CenterView::Chat | CenterView::Document, _) => {
                paste_into(&mut self.input, &mut self.input_cursor, &text)
            }
            (Focus::Center, CenterView::Search | CenterView::DocumentSearch, _) => {
                paste_into(
                    &mut self.search_query,
                    &mut self.search_cursor,
                    &text.replace('\n', ""),
                );
                self.recompute_search();
            }
            (Focus::Center, CenterView::Todo, _) => {
                paste_into(
                    &mut self.todo_query,
                    &mut self.todo_cursor,
                    &text.replace('\n', ""),
                );
                self.ensure_visible_todo_selection();
                self.todo_list_start = 0;
            }
            (Focus::Center, CenterView::Tags, _) => {
                paste_into(
                    &mut self.tag_query,
                    &mut self.tag_cursor,
                    &text.replace('\n', ""),
                );
                self.recompute_tags();
            }
            (Focus::Files, _, FilesContext::Search) => {
                paste_into(
                    &mut self.file_query,
                    &mut self.file_query_cursor,
                    &text.replace('\n', ""),
                );
                self.ensure_visible_file_selection();
            }
            (Focus::Files, _, FilesContext::NewTarget) => {
                paste_into(
                    &mut self.new_file_input,
                    &mut self.new_file_cursor,
                    &text.replace('\n', ""),
                );
            }
            (Focus::Files, _, FilesContext::Rename) => {
                paste_into(
                    &mut self.rename_input,
                    &mut self.rename_cursor,
                    &text.replace('\n', ""),
                );
            }
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) -> Option<Command> {
        if self.overlay == Some(Overlay::Terminal) {
            return None;
        }
        match event.kind {
            MouseEventKind::ScrollDown => {
                self.route_wheel(event.column, event.row, 1);
                None
            }
            MouseEventKind::ScrollUp => {
                self.route_wheel(event.column, event.row, -1);
                None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.handle_left_click(event.column, event.row)
            }
            // Right, middle, drag, move and button-up events are intentionally ignored.
            _ => None,
        }
    }

    pub fn handle_wheel(&mut self, column: u16, row: u16, delta: i32) {
        if self.overlay == Some(Overlay::Terminal) {
            if self.layout.overlay.is_none() || in_area(column, row, self.layout.overlay) {
                if let Some(terminal) = self.terminal.as_mut() {
                    terminal.scroll(delta);
                }
            }
            return;
        }
        self.route_wheel(column, row, delta);
    }

    pub(in crate::app) fn is_text_entry(&self) -> bool {
        self.focus == Focus::Compose
            || (self.focus == Focus::Center
                && matches!(
                    self.center_view,
                    CenterView::Todo
                        | CenterView::Search
                        | CenterView::DocumentSearch
                        | CenterView::Tags
                ))
            || (self.focus == Focus::Files
                && matches!(
                    self.files_context,
                    FilesContext::Search | FilesContext::NewTarget | FilesContext::Rename
                ))
    }
}
