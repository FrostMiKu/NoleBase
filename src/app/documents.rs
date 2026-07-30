use super::*;

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

    /// Original daily-task indices in display order: open tasks first, completed
    /// tasks second, with source order preserved inside each group.
    pub fn visible_todo_indices(&self) -> Vec<usize> {
        self.todo_items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| (!item.checked).then_some(index))
            .chain(
                self.todo_items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| item.checked.then_some(index)),
            )
            .collect()
    }

    pub fn selected_date(&self) -> Option<NaiveDate> {
        self.daily_notes.get(self.selected).map(|note| note.date)
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

    pub fn open_todo(&mut self) {
        self.reload_todos();
        self.todo_index = self.visible_todo_indices().first().copied().unwrap_or(0);
        self.focus = Focus::Todo;
    }

    pub fn open_search(&mut self) {
        self.search_query.clear();
        self.search_results.clear();
        self.search_index = 0;
        self.center_view = CenterView::Search;
        self.focus = Focus::Center;
    }

    pub(super) fn open_document_search(&mut self) {
        self.search_query.clear();
        self.search_results.clear();
        self.search_index = 0;
        self.center_view = CenterView::DocumentSearch;
        self.focus = Focus::Center;
    }

    pub fn open_help(&mut self) {
        self.help_scroll = 0;
        self.set_overlay(Overlay::Help);
    }

    pub(super) fn current_note_path(&self) -> Option<PathBuf> {
        self.document
            .as_ref()
            .filter(|_| self.center_view == CenterView::Document)
            .and_then(|document| match &document.kind {
                DocumentKind::File(path) => Some(path.clone()),
                DocumentKind::Daily(_) => None,
            })
    }

    pub(crate) fn current_note_archived(&self) -> Option<bool> {
        let path = self.current_note_path()?;
        self.note_files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.archived)
    }

    pub(super) fn rename_current_note(&mut self) {
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

    pub(super) fn delete_current_note(&mut self) {
        let Some(path) = self.current_note_path() else {
            self.set_status("No note is open");
            return;
        };
        self.pending_file = Some(path);
        self.set_overlay(Overlay::ConfirmDeleteFile);
    }

    pub(super) fn manage_current_note(&mut self, restore: bool) {
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

    /// Open a caller-defined command dialog. The caller can inspect the
    /// resulting value with [`App::take_dialog_result`] after the dialog
    /// closes.
    pub(super) fn handle_files(&mut self, key: KeyEvent) -> Option<Command> {
        match self.files_context {
            FilesContext::Browse => self.handle_file_browse(key),
            FilesContext::Search => self.handle_file_search(key),
            FilesContext::MoveTarget => self.handle_move_target(key),
            FilesContext::NewTarget => self.handle_new_target(key),
            FilesContext::Rename => self.handle_rename(key),
        }
    }

    pub(super) fn handle_file_browse(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q') => {
                if let Some(path) = self.current_note_path() {
                    self.sync_file_tree_to_note(&path);
                }
                self.focus = Focus::Center;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_file_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_file_selection(-1);
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.center_view == CenterView::Daily {
                    self.focus = Focus::Center;
                } else if let Some(group) = self.selected_file_group() {
                    let expanded = match group {
                        FileGroup::Notes => &mut self.notes_expanded,
                        FileGroup::Archives => &mut self.archives_expanded,
                    };
                    if *expanded {
                        if let Some(path) = self.current_note_path() {
                            self.sync_file_tree_to_note(&path);
                        }
                        self.focus = Focus::Center;
                    } else {
                        *expanded = true;
                    }
                } else {
                    self.open_selected_file(DocumentReturn::Daily);
                }
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
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

    pub(super) fn handle_file_search(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.file_query.clear();
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
            KeyCode::Backspace => {
                self.file_query.pop();
                self.ensure_visible_file_selection();
                None
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_query.push(character);
                self.ensure_visible_file_selection();
                None
            }
            _ => None,
        }
    }

    pub(super) fn handle_move_target(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.cancel_file_context();
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_file_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
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

    pub(super) fn begin_new_note(&mut self) {
        self.begin_new_note_with_template(false);
    }

    pub(super) fn begin_new_note_from_template(&mut self) {
        self.begin_new_note_with_template(true);
    }

    pub(super) fn begin_new_note_with_template(&mut self, from_template: bool) {
        self.pending_daily_date = None;
        self.new_note_from_template = from_template;
        self.new_file_input.clear();
        self.new_file_cursor = 0;
        self.files_context = FilesContext::NewTarget;
        self.focus = Focus::Files;
        self.open_file_name_dialog(DialogPurpose::NewFile);
    }

    pub(super) fn handle_new_target(&mut self, key: KeyEvent) -> Option<Command> {
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

    pub(super) fn handle_rename(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.pending_file = None;
                self.files_context = FilesContext::Browse;
                None
            }
            KeyCode::Enter => {
                if let Some(from) = self.pending_file.clone() {
                    let archived = self
                        .note_files
                        .iter()
                        .find(|file| file.path == from)
                        .is_some_and(|file| file.archived);
                    let result = if archived {
                        self.storage.rename_archived_file(&from, &self.rename_input)
                    } else {
                        self.storage.rename_file(&from, &self.rename_input)
                    };
                    match result {
                        Ok(to) => {
                            self.pending_file = None;
                            self.retarget_open_document(&from, &to);
                            self.selected_file = Some(to);
                            self.set_status("Renamed");
                            self.reload_files();
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

    pub(super) fn move_daily_selection(&mut self, delta: i32) {
        if !self.daily_notes.is_empty() {
            let selected = (self.selected as i32 + delta)
                .clamp(0, self.daily_notes.len().saturating_sub(1) as i32)
                as usize;
            if selected != self.selected {
                self.selected = selected;
                self.reveal_selected_daily = true;
            }
        }
    }

    pub(super) fn move_file_selection(&mut self, delta: i32) {
        let visible = self.visible_file_rows();
        if visible.is_empty() {
            self.file_index = 0;
            self.selected_file = None;
            self.file_row = 0;
            return;
        }
        let next = (self.file_row.min(visible.len() - 1) as i32 + delta)
            .clamp(0, visible.len() as i32 - 1) as usize;
        self.select_file_row(next);
    }

    pub(super) fn ensure_visible_file_selection(&mut self) {
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

    pub(super) fn sync_selected_file(&mut self) {
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

    pub(super) fn select_file_row(&mut self, row: usize) {
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

    pub(super) fn selected_file_group(&self) -> Option<FileGroup> {
        self.visible_file_rows()
            .get(self.file_row)
            .and_then(|row| match row {
                FileListRow::Group(group) => Some(*group),
                FileListRow::File(_) => None,
            })
    }

    pub(super) fn toggle_file_group(&mut self, group: FileGroup) {
        match group {
            FileGroup::Notes => self.notes_expanded = !self.notes_expanded,
            FileGroup::Archives => self.archives_expanded = !self.archives_expanded,
        }
        self.ensure_visible_file_selection();
    }

    pub(super) fn move_todo_selection(&mut self, delta: i32) {
        let visible = self.visible_todo_indices();
        if visible.is_empty() {
            self.todo_index = 0;
            return;
        }
        let position = visible
            .iter()
            .position(|index| *index == self.todo_index)
            .unwrap_or(0);
        let next = (position as i32 + delta).clamp(0, visible.len() as i32 - 1) as usize;
        self.todo_index = visible[next];
    }

    pub(super) fn move_search_selection(&mut self, delta: i32) {
        if !self.search_results.is_empty() {
            self.search_index = (self.search_index as i32 + delta)
                .clamp(0, self.search_results.len().saturating_sub(1) as i32)
                as usize;
        }
    }

    pub(super) fn scroll_document(&mut self, delta: i32) {
        if let Some(document) = self.document.as_mut() {
            document.scroll = if delta > 0 {
                document.scroll.saturating_add(delta as u16)
            } else {
                document.scroll.saturating_sub(delta.unsigned_abs() as u16)
            };
        }
    }

    pub(super) fn toggle_todo(&mut self, index: usize) {
        match self.storage.toggle_todo_task(index) {
            Ok(true) => {
                self.reload_todos();
                self.reload_files();
            }
            Ok(false) => self.set_status("No such task"),
            Err(error) => self.set_error(format!("Error: {error}")),
        }
    }

    pub(super) fn recompute_search(&mut self) {
        let query = self.search_query.trim().to_lowercase();
        let mut results = Vec::new();
        if !query.is_empty() && self.center_view == CenterView::DocumentSearch {
            if let Some(document) = &self.document {
                results.extend(
                    document
                        .source
                        .lines()
                        .enumerate()
                        .filter(|(_, line)| line.to_lowercase().contains(&query))
                        .map(|(index, line)| SearchHit::DocumentLine {
                            line_no: index + 1,
                            text: line.trim().to_string(),
                        }),
                );
            }
        } else if !query.is_empty() {
            if let Some(indexed) = self
                .workspace_index
                .with_index(|index| index.search(&query))
            {
                results = indexed;
            } else {
                self.set_status("Workspace index is still building");
            }
        }
        self.search_results = results;
        self.search_index = self
            .search_index
            .min(self.search_results.len().saturating_sub(1));
    }

    pub(super) fn open_tag_search(&mut self, name: &str) {
        self.close_dialog();
        self.search_query = format!("#{name}");
        self.search_index = 0;
        self.center_view = CenterView::Search;
        self.focus = Focus::Center;
        self.recompute_search();
    }

    pub(super) fn jump_to_search_result(&mut self, index: usize) {
        let Some(hit) = self.search_results.get(index).cloned() else {
            return;
        };
        if self.center_view == CenterView::DocumentSearch {
            if let SearchHit::DocumentLine { line_no, .. } = hit {
                if let Some(document) = self.document.as_mut() {
                    document.target_line = Some(line_no);
                    self.center_view = CenterView::Document;
                    self.focus = Focus::Center;
                    self.set_status(format!("Found on line {line_no}"));
                }
            }
            return;
        }
        match hit {
            SearchHit::FileLine { path, line_no, .. } => {
                if let Some(date) = self.storage.daily_date_for_path(&path) {
                    self.open_daily_document(date, DocumentReturn::Search);
                } else {
                    self.open_file_document(&path, DocumentReturn::Search);
                }
                if let Some(document) = self.document.as_mut() {
                    document.target_line = Some(line_no);
                }
            }
            SearchHit::DocumentLine { .. } => {}
        }
    }

    pub(super) fn open_selected_file(&mut self, return_to: DocumentReturn) {
        if let Some(path) = self.selected_file.clone() {
            self.open_file_document(&path, return_to);
        }
    }

    pub(super) fn archive_selected_note(&mut self) {
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

    pub(super) fn restore_selected_note(&mut self) {
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
        let daily_date = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| {
                path.extension().and_then(|ext| ext.to_str()) == Some("md")
                    && chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d").is_ok()
            });
        if let Some(date) = daily_date {
            match self.storage.restore_archived_daily(date) {
                Ok(()) => {
                    self.document_render_lru
                        .remove(&DocumentKind::File(path.clone()));
                    if self
                        .document
                        .as_ref()
                        .is_some_and(|document| document.kind == DocumentKind::File(path.clone()))
                    {
                        self.document = None;
                        self.center_view = CenterView::Daily;
                        self.focus = Focus::Center;
                    }
                    self.reload_workspace();
                    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
                        .expect("daily archive name was already validated");
                    if let Some(index) = self.daily_notes.iter().position(|note| note.date == date)
                    {
                        self.selected = index;
                        self.reveal_selected_daily = true;
                    }
                    self.set_status("Daily note restored");
                }
                Err(error) => self.set_error(format!("Error: {error}")),
            }
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

    pub(super) fn open_file_document(&mut self, path: &Path, return_to: DocumentReturn) {
        match self.storage.read_document_file(path) {
            Ok(source) => {
                self.sync_file_tree_to_note(path);
                let title = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Document".to_string());
                self.show_document(
                    DocumentKind::File(path.to_path_buf()),
                    title,
                    source,
                    return_to,
                );
                self.center_view = CenterView::Document;
                self.focus = Focus::Center;
            }
            Err(error) => self.set_error(format!("Error: {error}")),
        }
    }

    pub(super) fn sync_file_tree_to_note(&mut self, path: &Path) {
        let Some(index) = self.note_files.iter().position(|file| file.path == path) else {
            return;
        };
        if self.note_files[index].archived {
            self.archives_expanded = true;
        } else {
            self.notes_expanded = true;
        }
        self.file_index = index;
        self.selected_file = Some(path.to_path_buf());
        self.ensure_visible_file_selection();
    }

    pub(super) fn open_daily_document(&mut self, date: NaiveDate, return_to: DocumentReturn) {
        let Some(note) = self.daily_note_clone(date) else {
            return;
        };
        self.show_document(
            DocumentKind::Daily(note.date),
            format!("Daily {}", note.date),
            note.body,
            return_to,
        );
        self.center_view = CenterView::Document;
        self.focus = Focus::Center;
    }

    pub(super) fn show_document(
        &mut self,
        kind: DocumentKind,
        title: String,
        source: String,
        return_to: DocumentReturn,
    ) {
        self.stash_current_document();
        let render_cache = self.document_render_lru.take(&kind, &source);
        self.document = Some(Document {
            kind,
            title,
            source,
            scroll: 0,
            target_line: None,
            return_to,
            render_cache,
        });
    }

    pub(super) fn stash_current_document(&mut self) {
        let Some(mut document) = self.document.take() else {
            return;
        };
        if let Some(render) = document.render_cache.take() {
            self.document_render_lru
                .insert(document.kind, document.source, render);
        }
    }

    pub(super) fn close_document(&mut self) {
        let Some(document) = self.document.as_ref() else {
            self.center_view = CenterView::Daily;
            self.focus = Focus::Center;
            return;
        };
        let return_to = document.return_to;
        self.stash_current_document();
        match return_to {
            DocumentReturn::Search => {
                self.center_view = CenterView::Search;
                self.focus = Focus::Center;
            }
            DocumentReturn::Daily => {
                self.center_view = CenterView::Daily;
                self.focus = Focus::Center;
            }
        }
    }

    pub(super) fn act(&mut self, action: Action) -> Option<Command> {
        let date = self.selected_date()?;
        self.dispatch_action(date, action)
    }

    pub(super) fn dispatch_action(&mut self, date: NaiveDate, action: Action) -> Option<Command> {
        match action {
            Action::Ai => {
                self.open_agent_prompt(date);
                None
            }
            Action::Move => {
                self.pending_daily_date = Some(date);
                self.file_query.clear();
                self.reload_files();
                self.files_context = FilesContext::MoveTarget;
                self.ensure_visible_file_selection();
                self.focus = Focus::Files;
                None
            }
            Action::Archive => {
                if let Some(note) = self.daily_note_clone(date) {
                    match self.storage.archive_daily(&note.date.to_string()) {
                        Ok(_) => {
                            self.record_undo(UndoOp::Archive(note));
                            self.set_status("Daily note archived");
                            self.reload_workspace();
                        }
                        Err(error) => self.set_error(format!("Error: {error}")),
                    }
                }
                None
            }
            Action::New => {
                self.pending_daily_date = Some(date);
                self.new_file_input.clear();
                self.new_file_cursor = 0;
                self.files_context = FilesContext::NewTarget;
                self.focus = Focus::Files;
                self.open_file_name_dialog(DialogPurpose::NewFile);
                None
            }
            Action::View => {
                self.open_daily_document(date, DocumentReturn::Daily);
                None
            }
            Action::Edit => self.daily_edit_command(date),
            Action::Delete => {
                self.pending_daily_date = Some(date);
                self.set_overlay(Overlay::ConfirmDeleteDaily);
                None
            }
        }
    }

    pub(super) fn daily_edit_command(&self, date: NaiveDate) -> Option<Command> {
        self.storage
            .daily_file_path(&date.to_string())
            .ok()
            .filter(|path| path.is_file())
            .map(Command::Edit)
    }

    pub(super) fn cancel_file_context(&mut self) {
        self.pending_daily_date = None;
        self.new_note_from_template = false;
        self.pending_file = None;
        self.files_context = FilesContext::Browse;
        self.focus = Focus::Center;
    }

    pub(super) fn perform_move_to(&mut self, path: &Path) {
        let Some(date) = self.pending_daily_date else {
            self.cancel_file_context();
            return;
        };
        self.perform_move_to_date(path, date);
    }

    pub(super) fn perform_move_to_date(&mut self, path: &Path, date: NaiveDate) {
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
