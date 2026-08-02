//! Document browsing and actions: files.

use super::super::*;

impl App {
    pub fn visible_file_rows(&self) -> Vec<FileListRow> {
        let matches = |file: &NoteFile| {
            let name = file
                .path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            fuzzy_match(name, &self.file_query)
        };
        let notes = self
            .note_files
            .iter()
            .enumerate()
            .filter(|(_, file)| !file.archived && matches(file))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let archives = self
            .note_files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.archived && matches(file))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if self.files_context == FilesContext::MoveTarget {
            return notes.into_iter().map(FileListRow::File).collect();
        }

        let searching = self.files_context == FilesContext::Search && !self.file_query.is_empty();
        let mut rows = Vec::new();
        if !searching || !notes.is_empty() {
            rows.push(FileListRow::Group(FileGroup::Notes));
            if self.notes_expanded || searching {
                rows.extend(notes.into_iter().map(FileListRow::File));
            }
        }
        if !searching || !archives.is_empty() {
            rows.push(FileListRow::Group(FileGroup::Archives));
            if self.archives_expanded || searching {
                rows.extend(archives.into_iter().map(FileListRow::File));
            }
        }
        rows
    }

    pub fn open_files(&mut self) {
        self.reload_files();
        if let Some(path) = self.current_note_path() {
            self.sync_file_tree_to_note(&path);
        }
        self.focus = Focus::Files;
        if !matches!(
            self.files_context,
            FilesContext::MoveTarget | FilesContext::NewTarget | FilesContext::Rename
        ) {
            self.files_context = FilesContext::Browse;
        }
    }

    pub(in crate::app) fn open_selected_file(&mut self, return_to: DocumentReturn) {
        if let Some(path) = self.selected_file.clone() {
            self.open_file_document(&path, return_to);
        }
    }

    pub(in crate::app) fn current_note_path(&self) -> Option<PathBuf> {
        self.document
            .as_ref()
            .filter(|_| self.center_view == CenterView::Document)
            .and_then(|document| match &document.kind {
                DocumentKind::File(path) | DocumentKind::Skill(path) => Some(path.clone()),
                DocumentKind::Daily(_) => None,
            })
    }

    pub(in crate::app) fn rename_current_note(&mut self) {
        let Some(path) = self.current_note_path() else {
            self.set_status("No note is open");
            return;
        };
        self.rename_input = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        self.rename_cursor = self.rename_input.chars().count();
        self.pending_file = Some(path);
        self.files_context = FilesContext::Rename;
        self.open_file_name_dialog(DialogPurpose::RenameFile);
    }

    pub(in crate::app) fn delete_current_note(&mut self) {
        let Some(path) = self.current_note_path() else {
            self.set_status("No note is open");
            return;
        };
        self.pending_file = Some(path);
        self.set_overlay(Overlay::ConfirmDeleteFile);
    }

    pub(in crate::app) fn manage_current_note(&mut self, restore: bool) {
        let Some(path) = self.current_note_path() else {
            self.set_status("No note is open");
            return;
        };
        self.selected_file = Some(path);
        if restore {
            self.restore_selected_note();
        } else {
            self.archive_selected_note();
        }
    }

    pub(crate) fn current_note_archived(&self) -> Option<bool> {
        let path = self.current_note_path()?;
        self.note_files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.archived)
    }

    pub(in crate::app) fn handle_files(&mut self, key: KeyEvent) -> Option<Command> {
        match self.files_context {
            FilesContext::Browse => self.handle_file_browse(key),
            FilesContext::Search => self.handle_file_search(key),
            FilesContext::MoveTarget => self.handle_move_target(key),
            FilesContext::NewTarget => self.handle_new_target(key),
            FilesContext::Rename => self.handle_rename(key),
        }
    }

    pub(in crate::app) fn handle_file_browse(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q') => {
                if let Some(path) = self.current_note_path() {
                    self.sync_file_tree_to_note(&path);
                }
                self.focus = Focus::Center;
                None
            }
            code if is_down_key(code) => {
                self.move_file_selection(1);
                None
            }
            code if is_up_key(code) => {
                self.move_file_selection(-1);
                None
            }
            code if is_right_key(code) => {
                if let Some(path) = self.current_note_path() {
                    self.sync_file_tree_to_note(&path);
                }
                self.focus = Focus::Center;
                None
            }
            code if is_left_key(code) => {
                if let Some(group) = self.selected_file_group() {
                    match group {
                        FileGroup::Notes => self.notes_expanded = false,
                        FileGroup::Archives => self.archives_expanded = false,
                    }
                } else if let Some(file) = self.note_files.get(self.file_index) {
                    let group = if file.archived {
                        FileGroup::Archives
                    } else {
                        FileGroup::Notes
                    };
                    if let Some(row) = self
                        .visible_file_rows()
                        .iter()
                        .position(|item| *item == FileListRow::Group(group))
                    {
                        self.select_file_row(row);
                    }
                }
                None
            }
            KeyCode::Enter | KeyCode::Char('v') => {
                if let Some(group) = self.selected_file_group() {
                    self.toggle_file_group(group);
                } else {
                    self.open_selected_file(DocumentReturn::Daily);
                }
                None
            }
            KeyCode::Char('e') => self.selected_file.clone().map(Command::Edit),
            KeyCode::Char('/') => {
                self.file_query.clear();
                self.file_query_cursor = 0;
                self.files_context = FilesContext::Search;
                self.ensure_visible_file_selection();
                None
            }
            KeyCode::Char('r') => {
                if let Some(path) = self.selected_file.clone() {
                    self.rename_input = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("")
                        .to_string();
                    self.rename_cursor = self.rename_input.chars().count();
                    self.pending_file = Some(path);
                    self.files_context = FilesContext::Rename;
                    self.open_file_name_dialog(DialogPurpose::RenameFile);
                }
                None
            }
            KeyCode::Char('d') => {
                if let Some(path) = self.selected_file.clone() {
                    self.pending_file = Some(path);
                    self.set_overlay(Overlay::ConfirmDeleteFile);
                }
                None
            }
            KeyCode::Char('a') => {
                self.archive_selected_note();
                None
            }
            KeyCode::Char('u') => {
                self.restore_selected_note();
                None
            }
            _ => None,
        }
    }

    pub(in crate::app) fn handle_file_search(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.file_query.clear();
                self.file_query_cursor = 0;
                self.files_context = FilesContext::Browse;
                self.ensure_visible_file_selection();
                None
            }
            KeyCode::Down => {
                self.move_file_selection(1);
                None
            }
            KeyCode::Up => {
                self.move_file_selection(-1);
                None
            }
            KeyCode::Enter => {
                self.open_selected_file(DocumentReturn::Daily);
                None
            }
            _ => {
                let edit = edit_single_line(&mut self.file_query, &mut self.file_query_cursor, key);
                if edit.changed() {
                    self.ensure_visible_file_selection();
                }
                None
            }
        }
    }

    pub(in crate::app) fn handle_move_target(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.cancel_file_context();
                None
            }
            code if is_down_key(code) => {
                self.move_file_selection(1);
                None
            }
            code if is_up_key(code) => {
                self.move_file_selection(-1);
                None
            }
            KeyCode::Enter => {
                if let Some(path) = self.selected_file.clone() {
                    self.perform_move_to(&path);
                }
                None
            }
            _ => None,
        }
    }

    pub(in crate::app) fn begin_new_note(&mut self) {
        self.begin_new_note_with_template(false);
    }

    pub(in crate::app) fn begin_new_note_from_template(&mut self) {
        self.begin_new_note_with_template(true);
    }

    pub(in crate::app) fn begin_new_note_with_template(&mut self, from_template: bool) {
        self.pending_daily_date = None;
        self.new_note_from_template = from_template;
        self.new_file_input.clear();
        self.new_file_cursor = 0;
        self.files_context = FilesContext::NewTarget;
        self.focus = Focus::Files;
        self.open_file_name_dialog(DialogPurpose::NewFile);
    }

    pub(in crate::app) fn handle_new_target(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.cancel_file_context();
                None
            }
            KeyCode::Enter => {
                let name = self.new_file_input.clone();
                let created = if self.new_note_from_template {
                    self.storage.create_named_file_from_template(&name)
                } else {
                    self.storage.create_named_file(&name)
                };
                match created {
                    Ok(path) => {
                        self.new_note_from_template = false;
                        if let Some(date) = self.pending_daily_date {
                            self.perform_move_to_date(&path, date);
                        } else {
                            self.files_context = FilesContext::Browse;
                            self.reload_files();
                            self.selected_file = Some(path.clone());
                            self.open_file_document(&path, DocumentReturn::Daily);
                            self.set_status(format!("Created note {}", path.display()));
                        }
                    }
                    Err(error) => self.set_error(format!("Error: {error}")),
                }
                None
            }
            KeyCode::Backspace => {
                delete_backward(&mut self.new_file_input, &mut self.new_file_cursor);
                None
            }
            KeyCode::Delete => {
                delete_forward(&mut self.new_file_input, &mut self.new_file_cursor);
                None
            }
            KeyCode::Left => {
                self.new_file_cursor = self.new_file_cursor.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                self.new_file_cursor =
                    (self.new_file_cursor + 1).min(self.new_file_input.chars().count());
                None
            }
            KeyCode::Home => {
                self.new_file_cursor = 0;
                None
            }
            KeyCode::End => {
                self.new_file_cursor = self.new_file_input.chars().count();
                None
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                insert_char(
                    &mut self.new_file_input,
                    &mut self.new_file_cursor,
                    character,
                );
                None
            }
            _ => None,
        }
    }

    pub(in crate::app) fn handle_rename(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.pending_file = None;
                self.files_context = FilesContext::Browse;
                None
            }
            KeyCode::Enter => {
                if let Some(from) = self.pending_file.clone() {
                    let skill = self
                        .document
                        .as_ref()
                        .is_some_and(|document| {
                            matches!(&document.kind, DocumentKind::Skill(path) if path == &from)
                        });
                    let archived = self
                        .note_files
                        .iter()
                        .find(|file| file.path == from)
                        .is_some_and(|file| file.archived);
                    let result = if skill {
                        self.storage.rename_skill(&from, &self.rename_input)
                    } else if archived {
                        self.storage.rename_archived_file(&from, &self.rename_input)
                    } else {
                        self.storage.rename_file(&from, &self.rename_input)
                    };
                    match result {
                        Ok(to) => {
                            self.pending_file = None;
                            self.retarget_open_document(&from, &to);
                            if skill {
                                self.set_status("Skill renamed");
                            } else {
                                self.selected_file = Some(to);
                                self.set_status("Renamed");
                                self.reload_files();
                            }
                            self.files_context = FilesContext::Browse;
                        }
                        Err(error) => self.set_error(format!("Error: {error}")),
                    }
                }
                None
            }
            KeyCode::Backspace => {
                delete_backward(&mut self.rename_input, &mut self.rename_cursor);
                None
            }
            KeyCode::Delete => {
                delete_forward(&mut self.rename_input, &mut self.rename_cursor);
                None
            }
            KeyCode::Left => {
                self.rename_cursor = self.rename_cursor.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                self.rename_cursor =
                    (self.rename_cursor + 1).min(self.rename_input.chars().count());
                None
            }
            KeyCode::Home => {
                self.rename_cursor = 0;
                None
            }
            KeyCode::End => {
                self.rename_cursor = self.rename_input.chars().count();
                None
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                insert_char(&mut self.rename_input, &mut self.rename_cursor, character);
                None
            }
            _ => None,
        }
    }

    pub(in crate::app) fn move_daily_selection(&mut self, delta: i32) {
        let selected = move_index(self.selected, delta, self.daily_notes.len());
        if selected != self.selected {
            self.selected = selected;
            self.reveal_selected_daily = true;
        }
    }

    pub(in crate::app) fn move_file_selection(&mut self, delta: i32) {
        let visible = self.visible_file_rows();
        if visible.is_empty() {
            self.file_index = 0;
            self.selected_file = None;
            self.file_row = 0;
            return;
        }
        self.select_file_row(move_index(self.file_row, delta, visible.len()));
    }

    pub(in crate::app) fn ensure_visible_file_selection(&mut self) {
        let visible = self.visible_file_rows();
        if visible.is_empty() {
            self.selected_file = None;
            self.file_row = 0;
            return;
        }
        if let Some(path) = self.selected_file.as_ref() {
            if let Some(row) = visible.iter().position(|item| {
                matches!(item, FileListRow::File(index) if self.note_files.get(*index).is_some_and(|file| &file.path == path))
            }) {
                self.file_row = row;
                return;
            }
        }
        self.select_file_row(self.file_row.min(visible.len() - 1));
    }

    pub(in crate::app) fn sync_selected_file(&mut self) {
        self.selected_file = self
            .note_files
            .get(self.file_index)
            .map(|file| file.path.clone());
        if let Some(row) = self
            .visible_file_rows()
            .iter()
            .position(|row| matches!(row, FileListRow::File(index) if *index == self.file_index))
        {
            self.file_row = row;
        }
    }

    pub(in crate::app) fn select_file_row(&mut self, row: usize) {
        let rows = self.visible_file_rows();
        let Some(item) = rows.get(row).copied() else {
            return;
        };
        self.file_row = row;
        match item {
            FileListRow::File(index) => {
                self.file_index = index;
                self.selected_file = self.note_files.get(index).map(|file| file.path.clone());
            }
            FileListRow::Group(_) => self.selected_file = None,
        }
    }

    pub(in crate::app) fn selected_file_group(&self) -> Option<FileGroup> {
        self.visible_file_rows()
            .get(self.file_row)
            .and_then(|row| match row {
                FileListRow::Group(group) => Some(*group),
                FileListRow::File(_) => None,
            })
    }

    pub(in crate::app) fn toggle_file_group(&mut self, group: FileGroup) {
        match group {
            FileGroup::Notes => self.notes_expanded = !self.notes_expanded,
            FileGroup::Archives => self.archives_expanded = !self.archives_expanded,
        }
        self.ensure_visible_file_selection();
    }

    pub(in crate::app) fn archive_selected_note(&mut self) {
        let Some(path) = self.selected_file.clone() else {
            return;
        };
        if self
            .note_files
            .iter()
            .find(|file| file.path == path)
            .is_none_or(|file| file.archived)
        {
            self.set_status("Select a note to archive");
            return;
        }
        match self.storage.archive_note(&path) {
            Ok(to) => {
                self.retarget_open_document(&path, &to);
                self.selected_file = Some(to);
                self.archives_expanded = true;
                self.reload_files();
                self.set_status("Note archived");
            }
            Err(error) => self.set_error(format!("Error: {error}")),
        }
    }

    pub(in crate::app) fn restore_selected_note(&mut self) {
        let Some(path) = self.selected_file.clone() else {
            return;
        };
        if self
            .note_files
            .iter()
            .find(|file| file.path == path)
            .is_none_or(|file| !file.archived)
        {
            self.set_status("Select an archived note to restore");
            return;
        }
        match self.storage.restore_archived_note(&path) {
            Ok(to) => {
                self.retarget_open_document(&path, &to);
                self.selected_file = Some(to);
                self.notes_expanded = true;
                self.reload_files();
                self.set_status("Note restored");
            }
            Err(error) => self.set_error(format!("Error: {error}")),
        }
    }

    pub(in crate::app) fn cancel_file_context(&mut self) {
        self.pending_daily_date = None;
        self.new_note_from_template = false;
        self.pending_file = None;
        self.files_context = FilesContext::Browse;
        self.focus = Focus::Center;
    }

    pub(in crate::app) fn perform_move_to(&mut self, path: &Path) {
        let Some(date) = self.pending_daily_date else {
            self.cancel_file_context();
            return;
        };
        self.perform_move_to_date(path, date);
    }

    pub(in crate::app) fn perform_move_to_date(&mut self, path: &Path, date: NaiveDate) {
        let Some(note) = self.daily_note_clone(date) else {
            self.set_status("Daily note not found");
            return;
        };
        match self.storage.move_to_markdown(path, &note) {
            Ok(appended) => {
                self.record_undo(UndoOp::Move {
                    daily_note: note,
                    target: path.to_path_buf(),
                    appended,
                });
                self.set_status(format!(
                    "Moved to {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                self.pending_daily_date = None;
                self.files_context = FilesContext::Browse;
                self.focus = Focus::Center;
                self.center_view = CenterView::Daily;
                self.reload_workspace();
            }
            Err(error) => self.set_error(format!("Error: {error}")),
        }
    }
}
