use super::*;

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

    /// Paste into whichever orthogonal state currently owns a text buffer.
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

    pub(super) fn is_text_entry(&self) -> bool {
        self.focus == Focus::Compose
            || (self.focus == Focus::Center
                && matches!(
                    self.center_view,
                    CenterView::Search | CenterView::DocumentSearch | CenterView::Tags
                ))
            || (self.focus == Focus::Files
                && matches!(
                    self.files_context,
                    FilesContext::Search | FilesContext::NewTarget | FilesContext::Rename
                ))
    }

    pub(super) fn handle_compose(&mut self, key: KeyEvent) -> Option<Command> {
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

    pub(super) fn move_input_cursor(&mut self, movement: CursorMove) -> Option<Command> {
        self.input_cursor = move_cursor(&self.input, self.input_cursor, movement);
        None
    }

    pub(super) fn activate_link(&mut self, target: LinkTarget) -> Option<Command> {
        match target {
            LinkTarget::External(target) => Some(Command::OpenLink(target)),
            LinkTarget::EmbeddedFile(target) => {
                match self.storage.validate_embedded_file(&target) {
                    Ok(path) => Some(Command::OpenPath(path)),
                    Err(error) => {
                        self.set_error(format!("Embed error: {error}"));
                        None
                    }
                }
            }
            LinkTarget::WikiLink(target) => {
                let requested = target.trim().to_string();
                let mut candidates = self
                    .storage
                    .list_note_files()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|note| wiki_name_matches(&note.path, &requested))
                    .map(|note| WikiLinkCandidate {
                        path: note.path,
                        archived: false,
                    })
                    .collect::<Vec<_>>();
                candidates.extend(
                    self.storage
                        .list_archived_note_files()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|note| wiki_name_matches(&note.path, &requested))
                        .map(|note| WikiLinkCandidate {
                            path: note.path,
                            archived: true,
                        }),
                );
                if candidates.is_empty() {
                    match self.storage.create_named_file(&requested) {
                        Ok(path) => {
                            self.reload_files();
                            self.open_file_document(&path, DocumentReturn::Daily);
                            self.set_status(format!("Created note {}", path.display()));
                        }
                        Err(error) => self.set_error(format!("Wiki note error: {error}")),
                    }
                } else if candidates.len() == 1 {
                    self.open_wiki_candidate(&candidates[0]);
                } else {
                    self.wiki_link_target = Some(requested);
                    self.wiki_link_candidates = candidates;
                    self.wiki_link_index = 0;
                    self.set_overlay(Overlay::WikiLinkChoice);
                }
                None
            }
        }
    }

    pub(super) fn open_wiki_candidate(&mut self, candidate: &WikiLinkCandidate) {
        let source = if candidate.archived {
            self.storage.read_archived_note_file(&candidate.path)
        } else {
            self.storage.read_note_file(&candidate.path)
        };
        match source {
            Ok(source) => {
                let title = candidate
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Document".to_string());
                self.show_document(
                    DocumentKind::File(candidate.path.clone()),
                    title,
                    source,
                    DocumentReturn::Daily,
                );
                self.center_view = CenterView::Document;
                self.focus = Focus::Center;
                self.overlay = None;
                self.dialog = None;
                self.wiki_link_target = None;
                self.wiki_link_candidates.clear();
                self.wiki_link_index = 0;
            }
            Err(error) => self.set_error(format!("Wiki note error: {error}")),
        }
    }

    pub(super) fn handle_daily(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(Command::Quit),
            KeyCode::Tab | KeyCode::Char('i') | KeyCode::Enter => {
                self.focus = Focus::Compose;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_daily_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_daily_selection(-1);
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.open_files();
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
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
                self.scroll = self.scroll.saturating_add(5);
                None
            }
            KeyCode::PageUp => {
                self.reveal_selected_daily = false;
                self.scroll = self.scroll.saturating_sub(5);
                None
            }
            KeyCode::Char('/') => {
                self.open_search();
                None
            }
            KeyCode::Char('m') => self.act(Action::Move),
            KeyCode::Char('a') => self.act(Action::Archive),
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

    pub(super) fn handle_todo(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.activate_workspace_view(CenterView::Daily);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_todo_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_todo_selection(-1);
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.open_files();
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.open_workspace_views();
                None
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('x') => {
                self.toggle_todo(self.todo_index);
                None
            }
            _ => None,
        }
    }

    pub(super) fn handle_chat(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.activate_workspace_view(CenterView::Daily);
                None
            }
            KeyCode::Tab | KeyCode::Char('i') | KeyCode::Enter => {
                self.focus = Focus::Compose;
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.agent_scroll = self.agent_scroll.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.agent_scroll = self.agent_scroll.saturating_add(1);
                None
            }
            KeyCode::PageUp => {
                self.agent_scroll = self.agent_scroll.saturating_sub(8);
                None
            }
            KeyCode::PageDown => {
                self.agent_scroll = self.agent_scroll.saturating_add(8);
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.open_files();
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
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

    pub(super) fn handle_workspace_views(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Char('h') => {
                self.focus = Focus::Center;
                None
            }
            KeyCode::Up | KeyCode::Char('k') if self.workspace_view_index == 0 => {
                self.focus = Focus::Agent;
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_workspace_view_selection(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
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

    pub(super) fn handle_agent(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('C') => self.execute_app_command(AppCommand::ClearAgentSession),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Char('h') => {
                self.focus = Focus::Center;
                None
            }
            KeyCode::Up | KeyCode::Char('k') if self.agent_scroll == 0 => {
                self.focus = Focus::Center;
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.agent_scroll = self.agent_scroll.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.agent_scroll = self.agent_scroll.saturating_add(1);
                None
            }
            KeyCode::PageUp => {
                self.agent_scroll = self.agent_scroll.saturating_sub(8);
                None
            }
            KeyCode::PageDown => {
                self.agent_scroll = self.agent_scroll.saturating_add(8);
                None
            }
            KeyCode::Char('c') if self.ai_running => {
                self.execute_app_command(AppCommand::InterruptAgent)
            }
            _ => None,
        }
    }

    pub(super) fn handle_search(&mut self, key: KeyEvent) -> Option<Command> {
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

    pub(super) fn handle_tags(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                if WorkspaceView::index_of(self.tags_return_view).is_some() {
                    self.activate_workspace_view(self.tags_return_view);
                } else {
                    self.center_view = self.tags_return_view;
                    self.focus = Focus::Center;
                }
                None
            }
            KeyCode::Down => {
                self.move_tag_selection(1);
                None
            }
            KeyCode::Up => {
                self.move_tag_selection(-1);
                None
            }
            KeyCode::Enter => {
                if let Some(name) = self
                    .tag_results
                    .get(self.tag_index)
                    .map(|tag| tag.name.clone())
                {
                    self.open_tag_search(&name);
                }
                None
            }
            _ => {
                let edit = edit_single_line(&mut self.tag_query, &mut self.tag_cursor, key);
                if edit.changed() {
                    self.recompute_tags();
                }
                None
            }
        }
    }

    pub(super) fn handle_document(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.close_document();
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_document(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_document(-1);
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.open_files();
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.open_todo();
                None
            }
            KeyCode::PageDown => {
                self.scroll_document(10);
                None
            }
            KeyCode::PageUp => {
                self.scroll_document(-10);
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

    pub(super) fn route_wheel(&mut self, column: u16, row: u16, delta: i32) {
        if self.overlay.is_some() {
            if self.layout.overlay.is_none() || in_area(column, row, self.layout.overlay) {
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.scroll = if delta > 0 {
                        dialog.scroll.saturating_add(delta as u16)
                    } else {
                        dialog.scroll.saturating_sub(delta.unsigned_abs() as u16)
                    };
                    match dialog.purpose {
                        DialogPurpose::Help => self.help_scroll = dialog.scroll,
                        DialogPurpose::AgentApproval => self.approval_scroll = dialog.scroll,
                        _ => {}
                    }
                }
            }
            return;
        }
        if matches!(
            self.files_context,
            FilesContext::NewTarget | FilesContext::Rename
        ) {
            return;
        }

        if in_area(column, row, self.layout.files) {
            self.move_file_selection(delta);
        } else if in_area(column, row, self.layout.views) {
            self.move_workspace_view_selection(delta);
        } else if in_area(column, row, self.layout.agent) {
            self.agent_scroll = if delta > 0 {
                self.agent_scroll.saturating_add(delta as u16)
            } else {
                self.agent_scroll
                    .saturating_sub(delta.unsigned_abs() as u16)
            };
        } else if in_area(column, row, self.layout.center) {
            match self.center_view {
                CenterView::Daily => {
                    self.reveal_selected_daily = false;
                    self.scroll = if delta > 0 {
                        self.scroll.saturating_add(delta as u16)
                    } else {
                        self.scroll.saturating_sub(delta.unsigned_abs() as u16)
                    };
                }
                CenterView::Chat => {
                    self.agent_scroll = if delta > 0 {
                        self.agent_scroll.saturating_add(delta as u16)
                    } else {
                        self.agent_scroll
                            .saturating_sub(delta.unsigned_abs() as u16)
                    };
                }
                CenterView::Todo => self.move_todo_selection(delta),
                CenterView::Document => self.scroll_document(delta),
                CenterView::Search | CenterView::DocumentSearch => {
                    self.move_search_selection(delta)
                }
                CenterView::Tags => self.move_tag_selection(delta),
            }
        }
    }

    pub(super) fn handle_left_click(&mut self, column: u16, row: u16) -> Option<Command> {
        if self.overlay.is_some() {
            if let Some(index) = self
                .dialog_hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| hitbox.index)
                .or_else(|| {
                    self.wiki_link_hitboxes
                        .iter()
                        .find(|hitbox| point_in_rect(column, row, hitbox.area))
                        .map(|hitbox| hitbox.index)
                })
            {
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.selected = index;
                }
                self.sync_dialog_owner_state();
                if self
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.purpose == DialogPurpose::CommandPalette)
                {
                    return self.execute_selected_palette_command();
                }
                if self
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.purpose == DialogPurpose::WikiLinkChoice)
                {
                    if let Some(candidate) = self.wiki_link_candidates.get(index).cloned() {
                        self.open_wiki_candidate(&candidate);
                    }
                }
            }
            return None;
        }
        if matches!(
            self.files_context,
            FilesContext::NewTarget | FilesContext::Rename
        ) {
            return None;
        }

        if let Some(target) = self
            .link_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.target.clone())
        {
            return self.activate_link(target);
        }

        if let Some(name) = self
            .tag_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.name.clone())
        {
            self.open_tag_search(&name);
            return None;
        }

        if matches!(
            self.center_view,
            CenterView::Search | CenterView::DocumentSearch
        ) {
            if let Some(index) = self
                .search_hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| hitbox.index)
            {
                self.search_index = index;
                self.jump_to_search_result(index);
                return None;
            }
        }

        if let Some(index) = self
            .todo_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.index)
        {
            self.activate_workspace_view(CenterView::Todo);
            self.todo_index = index;
            self.toggle_todo(index);
            return None;
        }

        if let Some(index) = self
            .workspace_view_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.index)
        {
            if let Some(center_view) = WorkspaceView::ALL.get(index).map(|view| view.center_view) {
                self.activate_workspace_view(center_view);
            }
            return None;
        }

        if let Some(group) = self
            .file_group_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.group)
        {
            self.focus = Focus::Files;
            if let Some(row) = self
                .visible_file_rows()
                .iter()
                .position(|item| *item == FileListRow::Group(group))
            {
                self.select_file_row(row);
                self.toggle_file_group(group);
            }
            return None;
        }

        if let Some(path) = self
            .file_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.path.clone())
        {
            if let Some(index) = self.note_files.iter().position(|file| file.path == path) {
                self.file_index = index;
                self.sync_selected_file();
                self.focus = Focus::Files;
                match self.files_context {
                    FilesContext::Browse | FilesContext::Search => {
                        self.open_selected_file(DocumentReturn::Daily)
                    }
                    FilesContext::MoveTarget => self.perform_move_to(&path),
                    FilesContext::NewTarget | FilesContext::Rename => {}
                }
            }
            return None;
        }

        if self.center_view == CenterView::Daily {
            if let Some((date, action)) = self
                .hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| (hitbox.date, hitbox.action))
            {
                return self.dispatch_action(date, action);
            }
        }

        if in_area(column, row, self.layout.compose)
            && matches!(
                self.center_view,
                CenterView::Daily | CenterView::Chat | CenterView::Document
            )
        {
            self.focus = Focus::Compose;
        } else if in_area(column, row, self.layout.files) {
            self.focus = Focus::Files;
        } else if in_area(column, row, self.layout.views) {
            self.focus = Focus::Views;
        } else if in_area(column, row, self.layout.agent) {
            self.focus = Focus::Agent;
        } else if in_area(column, row, self.layout.center) {
            self.focus = Focus::Center;
        }
        None
    }

    pub(super) fn send_message(&mut self) {
        let original_input = self.input.clone();
        let body = self.input.trim().to_string();
        if body.is_empty() {
            return;
        }
        let document_kind = self
            .document
            .as_ref()
            .filter(|_| self.center_view == CenterView::Document)
            .map(|document| document.kind.clone());
        let result = match document_kind {
            Some(DocumentKind::File(path)) => {
                self.append_to_open_note(&path, &body, &original_input)
            }
            Some(DocumentKind::Daily(date)) => {
                self.append_to_open_daily(&date.to_string(), &body, &original_input)
            }
            Some(DocumentKind::Skill(path)) => {
                self.append_to_open_skill(&path, &body, &original_input)
            }
            None => self.append_to_today(&body, &original_input),
        };
        if let Err(error) = result {
            self.set_error(format!("Error: {error}"));
        }
    }

    pub(super) fn append_to_open_note(
        &mut self,
        path: &Path,
        body: &str,
        original_input: &str,
    ) -> anyhow::Result<()> {
        let receipt = self.storage.append_document_tracked(path, body)?;
        self.record_undo(UndoOp::Append {
            receipt,
            input: original_input.to_string(),
        });
        let source = self.storage.read_document_file(path)?;
        if let Some(document) = self.document.as_mut() {
            document.replace_source(source);
            document.scroll = u16::MAX;
            document.target_line = None;
        }
        self.input.clear();
        self.input_cursor = 0;
        self.reload_files();
        self.status.clear();
        Ok(())
    }

    pub(super) fn append_to_open_skill(
        &mut self,
        path: &Path,
        body: &str,
        original_input: &str,
    ) -> anyhow::Result<()> {
        let receipt = self.storage.append_document_tracked(path, body)?;
        self.record_undo(UndoOp::Append {
            receipt,
            input: original_input.to_string(),
        });
        let skill = self.storage.read_skill(path)?;
        if let Some(document) = self.document.as_mut() {
            document.replace_source(skill.body);
            document.scroll = u16::MAX;
            document.target_line = None;
        }
        self.input.clear();
        self.input_cursor = 0;
        self.set_status("Appended to skill");
        Ok(())
    }

    pub(super) fn append_to_open_daily(
        &mut self,
        date: &str,
        body: &str,
        original_input: &str,
    ) -> anyhow::Result<()> {
        let (note, receipt) = self.storage.append_daily_tracked(date, body)?;
        self.record_undo(UndoOp::Append {
            receipt,
            input: original_input.to_string(),
        });
        if let Some(document) = self.document.as_mut() {
            document.replace_source(note.body);
        }
        self.input.clear();
        self.input_cursor = 0;
        self.reload();
        self.reload_todos();
        self.notifications
            .notify(format!("Appended to Daily {date}"));
        self.set_status("Appended without leaving the document");
        Ok(())
    }

    pub(super) fn append_to_today(
        &mut self,
        body: &str,
        original_input: &str,
    ) -> anyhow::Result<()> {
        let (_, receipt) = self.storage.append_to_today_tracked(body)?;
        self.record_undo(UndoOp::Append {
            receipt,
            input: original_input.to_string(),
        });
        self.input.clear();
        self.input_cursor = 0;
        self.reload();
        self.reload_todos();
        self.selected = self.daily_notes.len().saturating_sub(1);
        self.scroll = u16::MAX;
        self.reveal_selected_daily = true;
        self.set_status("Saved");
        Ok(())
    }

    pub(super) fn daily_note_clone(&self, date: NaiveDate) -> Option<DailyNote> {
        self.daily_notes
            .iter()
            .find(|note| note.date == date)
            .cloned()
    }

    pub(super) fn record_undo(&mut self, operation: UndoOp) {
        const CAPACITY: usize = 50;
        if self.undo_stack.len() == CAPACITY {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(operation);
    }

    pub(super) fn recall_last_append(&mut self) {
        let Some(operation) = self.undo_stack.pop() else {
            self.set_status("Nothing to recall");
            return;
        };
        let UndoOp::Append { receipt, input } = operation else {
            self.undo_stack.push(operation);
            self.set_status("Nothing to recall");
            return;
        };

        match self.storage.undo_append(&receipt) {
            Ok(()) => {
                self.restore_recalled_input(input);
                self.reload_workspace();
                self.selected = self.daily_notes.len().saturating_sub(1);
                self.scroll = u16::MAX;
                self.set_status("Recalled last append");
            }
            Err(error) => {
                self.undo_stack.push(UndoOp::Append { receipt, input });
                self.set_error(format!("Recall error: {error}"));
            }
        }
    }

    pub(super) fn restore_recalled_input(&mut self, recalled: String) {
        if self.input.is_empty() {
            self.input = recalled;
        } else {
            let current = std::mem::take(&mut self.input);
            self.input = recalled;
            if !self.input.ends_with('\n') && !current.starts_with('\n') {
                self.input.push('\n');
            }
            self.input.push_str(&current);
        }
        self.input_cursor = self.input.chars().count();
    }

    pub(super) fn undo(&mut self) {
        let Some(operation) = self.undo_stack.pop() else {
            self.set_status("Nothing to undo");
            return;
        };
        let status = match operation {
            UndoOp::Append { receipt, input } => match self.storage.undo_append(&receipt) {
                Ok(()) => {
                    self.restore_recalled_input(input);
                    "Recalled last append".to_string()
                }
                Err(error) => format!("Undo error: {error}"),
            },
            UndoOp::Delete(note) => match self.storage.restore_daily(&note) {
                Ok(()) => "Undid delete".to_string(),
                Err(error) => format!("Undo error: {error}"),
            },
            UndoOp::Move {
                daily_note,
                target,
                appended,
            } => match self.storage.restore_daily(&daily_note) {
                Ok(()) => {
                    let name = target
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if self
                        .storage
                        .remove_first_occurrence(&target, &appended)
                        .unwrap_or(false)
                    {
                        format!("Undid move to {name}")
                    } else {
                        format!("Undid move (couldn't tidy {name})")
                    }
                }
                Err(error) => format!("Undo error: {error}"),
            },
            UndoOp::Archive(note) => {
                match self.storage.restore_archived_daily(&note.date.to_string()) {
                    Ok(()) => "Undid archive".to_string(),
                    Err(error) => format!("Undo error: {error}"),
                }
            }
        };
        if status.starts_with("Undo error:") {
            self.set_error(status);
        } else {
            self.set_status(status);
        }
        self.reload_workspace();
        self.selected = self.daily_notes.len().saturating_sub(1);
        self.scroll = u16::MAX;
    }
}
