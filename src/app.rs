//! Application state and event handling.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use crate::model::{Action, Message, TodoHitbox, TodoItem};
use crate::storage::Storage;

fn point_in_rect(col: u16, row: u16, area: ratatui::layout::Rect) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// Case-insensitive subsequence match (true "fuzzy"). Empty needle matches all.
fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let needle: Vec<char> = needle.to_lowercase().chars().collect();
    let mut hi = 0;
    for &n in &needle {
        match hay[hi..].iter().position(|&h| h == n) {
            Some(pos) => hi += pos + 1,
            None => return false,
        }
    }
    true
}

/// A request from the app to the terminal driver (which owns the TUI lifecycle).
#[derive(Debug)]
pub enum Command {
    Quit,
    /// Suspend the TUI, open `path` in `$EDITOR`, then reload on return.
    Edit(PathBuf),
}

/// Direction of cursor movement within the input buffer.
#[derive(Debug, Clone, Copy)]
enum CursorMove {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Input box not focused; message shortcuts are active.
    Normal,
    /// Input box focused; typing records a message.
    Insert,
    /// Choosing an existing markdown file to move into.
    SelectTarget,
    /// Entering a name for a new markdown file.
    NewFile,
    /// Asking before deleting a message.
    ConfirmDelete,
    /// Viewing a message body or file content.
    Preview,
    /// Browsing the file list (floating popup).
    FileList,
    /// Typing a fuzzy filter inside the file-list popup.
    FileSearch,
    /// Typing a new name to rename the selected file.
    FileRename,
    /// Confirming deletion of a file.
    FileDelete,
    /// Browsing and toggling the TODO.md task list.
    Todo,
}

#[derive(Debug, Clone)]
pub struct Preview {
    pub title: String,
    /// Raw markdown source rendered (as styled markdown) in the preview modal.
    pub source: String,
    pub scroll: u16,
}

pub struct App {
    pub storage: Storage,
    pub messages: Vec<Message>,
    pub input: String,
    /// Insertion point in `input`, as a char index.
    pub input_cursor: usize,
    pub mode: Mode,
    /// Mode to return to when the preview modal closes (set when preview opens).
    pub preview_return: Mode,
    pub selected: usize,
    pub scroll: u16,
    pub target_files: Vec<PathBuf>,
    pub target_index: usize,
    pub new_file_input: String,
    /// Pending message id for SelectTarget / NewFile / ConfirmDelete.
    pub pending_id: Option<String>,
    pub preview: Option<Preview>,
    /// Rebuilt each frame by the renderer.
    pub hitboxes: Vec<crate::model::ButtonHitbox>,
    /// Clickable rows in the file-list popup.
    pub file_hitboxes: Vec<crate::model::FileHitbox>,
    /// Files listed in the file-list popup (`~/.note/*.md`, excluding CHAT.md).
    pub sidebar_files: Vec<PathBuf>,
    /// Full (unfiltered) file listing; `sidebar_files` is the filtered view.
    pub all_files: Vec<PathBuf>,
    /// Fuzzy filter text typed in `FileSearch` mode.
    pub file_query: String,
    /// Buffer for `FileRename` mode.
    pub rename_input: String,
    /// Target file for `FileRename` / `FileDelete`.
    pub pending_file: Option<PathBuf>,
    pub sidebar_index: usize,
    /// Tasks parsed from TODO.md, shown in `Mode::Todo`.
    pub todo_items: Vec<TodoItem>,
    pub todo_index: usize,
    /// Clickable rows in the todo panel, rebuilt each frame.
    pub todo_hitboxes: Vec<TodoHitbox>,
    pub status: String,
}

impl App {
    pub fn new(storage: Storage) -> anyhow::Result<Self> {
        let messages = storage.load_messages().unwrap_or_default();
        let selected = messages.len().saturating_sub(1);
        Ok(Self {
            storage,
            messages,
            input: String::new(),
            input_cursor: 0,
            mode: Mode::Insert,
            preview_return: Mode::Normal,
            selected,
            scroll: u32::MAX as u16, // start at the bottom
            target_files: Vec::new(),
            target_index: 0,
            new_file_input: String::new(),
            pending_id: None,
            preview: None,
            hitboxes: Vec::new(),
            file_hitboxes: Vec::new(),
            sidebar_files: Vec::new(),
            all_files: Vec::new(),
            file_query: String::new(),
            rename_input: String::new(),
            pending_file: None,
            sidebar_index: 0,
            todo_items: Vec::new(),
            todo_index: 0,
            todo_hitboxes: Vec::new(),
            status: String::new(),
        })
    }

    /// Reload messages from disk (after external edits or mutations).
    pub fn reload(&mut self) {
        self.messages = self.storage.load_messages().unwrap_or_default();
        if self.selected >= self.messages.len() {
            self.selected = self.messages.len().saturating_sub(1);
        }
    }

    /// Open the file-list popup, refreshing the listing from disk.
    pub fn open_file_list(&mut self) {
        self.all_files = self.storage.list_markdown_files().unwrap_or_default();
        self.file_query.clear();
        self.sidebar_index = 0;
        self.refresh_file_filter();
        self.mode = Mode::FileList;
    }

    /// Open the TODO.md task panel, reloading tasks from disk.
    pub fn open_todo(&mut self) {
        self.todo_items = self.storage.load_todo_tasks();
        if self.todo_index >= self.todo_items.len() {
            self.todo_index = self.todo_items.len().saturating_sub(1);
        }
        self.mode = Mode::Todo;
    }

    /// Flip the `index`-th task's completion state and refresh the panel.
    fn toggle_todo(&mut self, index: usize) {
        match self.storage.toggle_todo_task(index) {
            Ok(true) => self.todo_items = self.storage.load_todo_tasks(),
            Ok(false) => self.set_status("No such task"),
            Err(e) => self.set_status(format!("Error: {e}")),
        }
    }

    /// Recompute `sidebar_files` from `all_files` and the current `file_query`,
    /// keeping `sidebar_index` in range.
    fn refresh_file_filter(&mut self) {
        let q = self.file_query.clone();
        self.sidebar_files = self
            .all_files
            .iter()
            .filter(|p| {
                let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                fuzzy_match(name, &q)
            })
            .cloned()
            .collect();
        if self.sidebar_index >= self.sidebar_files.len() {
            self.sidebar_index = self.sidebar_files.len().saturating_sub(1);
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.messages.get(self.selected).map(|m| m.id.as_str())
    }

    /// Handle a keyboard event. Returns a terminal-level command if any.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Command> {
        match self.mode {
            Mode::Insert => self.handle_insert(key),
            Mode::Normal => self.handle_normal(key),
            Mode::SelectTarget => self.handle_select_target(key),
            Mode::NewFile => self.handle_new_file(key),
            Mode::ConfirmDelete => self.handle_confirm_delete(key),
            Mode::Preview => self.handle_preview(key),
            Mode::FileList => self.handle_file_list(key),
            Mode::FileSearch => self.handle_file_search(key),
            Mode::FileRename => self.handle_file_rename(key),
            Mode::FileDelete => self.handle_file_delete(key),
            Mode::Todo => self.handle_todo(key),
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent) -> Option<Command> {
        match ev.kind {
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(1);
                None
            }
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
            MouseEventKind::Down(_) => {
                // A clicked todo row toggles that task.
                let todo_hit = self
                    .todo_hitboxes
                    .iter()
                    .find(|h| point_in_rect(ev.column, ev.row, h.area))
                    .map(|h| h.index);
                if let Some(index) = todo_hit {
                    self.todo_index = index;
                    self.toggle_todo(index);
                    return None;
                }
                // A clicked file row previews that file.
                let file_hit = self
                    .file_hitboxes
                    .iter()
                    .find(|h| point_in_rect(ev.column, ev.row, h.area))
                    .map(|h| h.path.clone());
                if let Some(path) = file_hit {
                    self.open_file_preview(&path);
                    return None;
                }
                // Pull the matched button out of the immutable borrow before
                // dispatching (which needs &mut self).
                let hit = self
                    .hitboxes
                    .iter()
                    .find(|h| point_in_rect(ev.column, ev.row, h.area))
                    .map(|h| (h.message_id.clone(), h.action));
                if let Some((id, action)) = hit {
                    return self.dispatch_action(&id, action);
                }
                // Clicking outside any button/overlay returns to insert focus.
                if !matches!(
                    self.mode,
                    Mode::Preview
                        | Mode::ConfirmDelete
                        | Mode::SelectTarget
                        | Mode::NewFile
                        | Mode::FileList
                        | Mode::FileSearch
                        | Mode::FileRename
                        | Mode::FileDelete
                        | Mode::Todo
                ) {
                    self.mode = Mode::Insert;
                }
                None
            }
            _ => None,
        }
    }

    fn handle_insert(&mut self, key: KeyEvent) -> Option<Command> {
        let mods = key.modifiers;
        let send_mods = KeyModifiers::CONTROL | KeyModifiers::ALT;
        match key.code {
            // Modifier-bearing Enter sends; bare Enter inserts a newline.
            // (Ctrl+J arrives as a bare Enter in raw mode, so it behaves as a
            // newline here — consistent with the plain Enter key.)
            KeyCode::Enter if mods.intersects(send_mods) => {
                self.send_message();
                None
            }
            KeyCode::Enter => {
                self.insert_char('\n');
                None
            }
            KeyCode::Tab | KeyCode::Esc => {
                self.mode = Mode::Normal;
                None
            }
            KeyCode::Backspace => {
                self.delete_backward();
                None
            }
            KeyCode::Delete => {
                self.delete_forward();
                None
            }
            KeyCode::Left => {
                self.move_cursor(CursorMove::Left);
                None
            }
            KeyCode::Right => {
                self.move_cursor(CursorMove::Right);
                None
            }
            KeyCode::Up => {
                self.move_cursor(CursorMove::Up);
                None
            }
            KeyCode::Down => {
                self.move_cursor(CursorMove::Down);
                None
            }
            KeyCode::Home => {
                self.move_cursor(CursorMove::LineStart);
                None
            }
            KeyCode::End => {
                self.move_cursor(CursorMove::LineEnd);
                None
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                self.insert_char(c);
                None
            }
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => {
                if self.input.is_empty() {
                    Some(Command::Quit)
                } else {
                    self.input.clear();
                    self.input_cursor = 0;
                    None
                }
            }
            _ => None,
        }
    }

    /// Byte offset of the `char_idx`-th char in `s` (or `s.len()` past the end).
    fn char_to_byte(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or_else(|| s.len())
    }

    fn insert_char(&mut self, c: char) {
        let b = Self::char_to_byte(&self.input, self.input_cursor);
        self.input.insert(b, c);
        self.input_cursor += 1;
    }

    fn delete_backward(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let prev = Self::char_to_byte(&self.input, self.input_cursor - 1);
        let cur = Self::char_to_byte(&self.input, self.input_cursor);
        self.input.replace_range(prev..cur, "");
        self.input_cursor -= 1;
    }

    fn delete_forward(&mut self) {
        let total = self.input.chars().count();
        if self.input_cursor >= total {
            return;
        }
        let cur = Self::char_to_byte(&self.input, self.input_cursor);
        let next = Self::char_to_byte(&self.input, self.input_cursor + 1);
        self.input.replace_range(cur..next, "");
    }

    fn move_cursor(&mut self, m: CursorMove) {
        let chars: Vec<char> = self.input.chars().collect();
        let total = chars.len();
        let i = self.input_cursor;
        // Index just after the '\n' that starts the current line (or 0).
        let line_start = {
            let mut j = i;
            while j > 0 && chars[j - 1] != '\n' {
                j -= 1;
            }
            j
        };
        match m {
            CursorMove::Left => self.input_cursor = i.saturating_sub(1),
            CursorMove::Right => self.input_cursor = (i + 1).min(total),
            CursorMove::LineStart => self.input_cursor = line_start,
            CursorMove::LineEnd => {
                let mut j = i;
                while j < total && chars[j] != '\n' {
                    j += 1;
                }
                self.input_cursor = j;
            }
            CursorMove::Up | CursorMove::Down => {
                let col = i - line_start;
                let target_line_start = if matches!(m, CursorMove::Up) {
                    if line_start == 0 {
                        return; // already on the first line
                    }
                    // Step back over the '\n', then to the previous line start.
                    let mut j = line_start - 1;
                    while j > 0 && chars[j - 1] != '\n' {
                        j -= 1;
                    }
                    j
                } else {
                    // Advance to the next '\n'; the next line starts after it.
                    let mut j = i;
                    while j < total && chars[j] != '\n' {
                        j += 1;
                    }
                    if j >= total {
                        return; // already on the last line
                    }
                    j + 1
                };
                let target_line_end = {
                    let mut j = target_line_start;
                    while j < total && chars[j] != '\n' {
                        j += 1;
                    }
                    j
                };
                self.input_cursor = (target_line_start + col).min(target_line_end);
            }
        }
    }

    fn handle_normal(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => Some(Command::Quit),
            KeyCode::Tab => {
                self.mode = Mode::Insert;
                None
            }
            KeyCode::Char('f') => {
                self.open_file_list();
                None
            }
            KeyCode::Char('i') | KeyCode::Enter => {
                self.mode = Mode::Insert;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                None
            }
            KeyCode::Char('G') => {
                self.selected = self.messages.len().saturating_sub(1);
                None
            }
            KeyCode::Char('g') => {
                self.selected = 0;
                None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(5);
                None
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(5);
                None
            }
            KeyCode::Char('t') => self.act(Action::Todo),
            KeyCode::Char('m') => self.act(Action::Move),
            KeyCode::Char('a') => self.act(Action::Archive),
            KeyCode::Char('T') => {
                self.open_todo();
                None
            }
            KeyCode::Char('n') => self.act(Action::New),
            KeyCode::Char('v') => self.act(Action::View),
            KeyCode::Char('e') => self.act(Action::Edit),
            KeyCode::Char('d') => self.act(Action::Delete),
            _ => None,
        }
    }

    fn handle_file_list(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q') | KeyCode::Char('f') => {
                self.mode = Mode::Normal;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.sidebar_index + 1 < self.sidebar_files.len() {
                    self.sidebar_index += 1;
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.sidebar_index > 0 {
                    self.sidebar_index -= 1;
                }
                None
            }
            KeyCode::Enter | KeyCode::Char('v') => {
                if let Some(path) = self.sidebar_files.get(self.sidebar_index).cloned() {
                    self.open_file_preview(&path);
                }
                None
            }
            KeyCode::Char('e') => self
                .sidebar_files
                .get(self.sidebar_index)
                .cloned()
                .map(Command::Edit),
            KeyCode::Char('/') => {
                self.file_query.clear();
                self.refresh_file_filter();
                self.mode = Mode::FileSearch;
                None
            }
            KeyCode::Char('r') => {
                if let Some(path) = self.sidebar_files.get(self.sidebar_index).cloned() {
                    self.rename_input = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    self.pending_file = Some(path);
                    self.mode = Mode::FileRename;
                }
                None
            }
            KeyCode::Char('d') => {
                if let Some(path) = self.sidebar_files.get(self.sidebar_index).cloned() {
                    self.pending_file = Some(path);
                    self.mode = Mode::FileDelete;
                }
                None
            }
            _ => None,
        }
    }

    fn handle_file_search(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            // Esc cancels the search (clears the filter); results stay live, so
            // arrows navigate them and Enter opens the highlighted one.
            KeyCode::Esc => {
                self.file_query.clear();
                self.refresh_file_filter();
                self.mode = Mode::FileList;
                None
            }
            // Arrow keys only (j/k are valid filter characters).
            KeyCode::Down => {
                if self.sidebar_index + 1 < self.sidebar_files.len() {
                    self.sidebar_index += 1;
                }
                None
            }
            KeyCode::Up => {
                if self.sidebar_index > 0 {
                    self.sidebar_index -= 1;
                }
                None
            }
            KeyCode::Enter => {
                if let Some(path) = self.sidebar_files.get(self.sidebar_index).cloned() {
                    self.open_file_preview(&path);
                }
                None
            }
            KeyCode::Backspace => {
                self.file_query.pop();
                self.refresh_file_filter();
                None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_query.push(c);
                self.refresh_file_filter();
                None
            }
            _ => None,
        }
    }

    fn handle_file_rename(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.pending_file = None;
                self.mode = Mode::FileList;
                None
            }
            KeyCode::Enter => {
                if let Some(from) = self.pending_file.take() {
                    let name = self.rename_input.clone();
                    match self.storage.rename_file(&from, &name) {
                        Ok(_) => {
                            self.set_status(format!("Renamed to {name}"));
                            self.all_files = self.storage.list_markdown_files().unwrap_or_default();
                            self.refresh_file_filter();
                        }
                        Err(e) => self.set_status(format!("Error: {e}")),
                    }
                }
                self.mode = Mode::FileList;
                None
            }
            KeyCode::Backspace => {
                self.rename_input.pop();
                None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.rename_input.push(c);
                None
            }
            _ => None,
        }
    }

    fn handle_file_delete(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(path) = self.pending_file.take() {
                    match self.storage.delete_file(&path) {
                        Ok(()) => {
                            self.set_status(format!(
                                "Deleted {}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            ));
                            self.all_files = self.storage.list_markdown_files().unwrap_or_default();
                            self.refresh_file_filter();
                        }
                        Err(e) => self.set_status(format!("Error: {e}")),
                    }
                }
                self.mode = Mode::FileList;
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_file = None;
                self.mode = Mode::FileList;
                None
            }
            _ => None,
        }
    }

    fn handle_todo(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('T') => {
                self.mode = Mode::Normal;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.todo_index + 1 < self.todo_items.len() {
                    self.todo_index += 1;
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.todo_index > 0 {
                    self.todo_index -= 1;
                }
                None
            }
            // Toggle the highlighted task (Enter / Space / x).
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('x') => {
                self.toggle_todo(self.todo_index);
                None
            }
            _ => None,
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.messages.is_empty() {
            return;
        }
        let next = self.selected as i32 + delta;
        self.selected = next.clamp(0, (self.messages.len() - 1) as i32) as usize;
    }

    /// Apply an action to the currently selected message.
    fn act(&mut self, action: Action) -> Option<Command> {
        let id = self.selected_id()?.to_string();
        self.dispatch_action(&id, action)
    }

    fn dispatch_action(&mut self, id: &str, action: Action) -> Option<Command> {
        match action {
            Action::Todo => {
                if let Some(m) = self.message_clone(id) {
                    match self.storage.move_to_todo(&m) {
                        Ok(()) => {
                            self.set_status("Moved to TODO.md".to_string());
                            self.reload();
                        }
                        Err(e) => self.set_status(format!("Error: {e}")),
                    }
                }
                None
            }
            Action::Move => {
                self.pending_id = Some(id.to_string());
                self.target_files = self.storage.list_markdown_files().unwrap_or_default();
                self.target_index = 0;
                self.mode = Mode::SelectTarget;
                None
            }
            Action::Archive => {
                // Same append-section + remove-from-chat path as Move, just with
                // a fixed target (ARCHIVE.md) — no extra storage method needed.
                if let Some(m) = self.message_clone(id) {
                    match self.storage.move_to_markdown(&self.storage.archive_path, &m) {
                        Ok(()) => {
                            self.set_status("Archived to ARCHIVE.md".to_string());
                            self.reload();
                        }
                        Err(e) => self.set_status(format!("Error: {e}")),
                    }
                }
                None
            }
            Action::New => {
                self.pending_id = Some(id.to_string());
                self.new_file_input.clear();
                self.mode = Mode::NewFile;
                None
            }
            Action::View => {
                if let Some(m) = self.message_clone(id) {
                    self.preview = Some(Preview {
                        title: format!("Message {}", m.created_at.format("%Y-%m-%d %H:%M")),
                        source: m.body.clone(),
                        scroll: 0,
                    });
                    self.preview_return = self.mode;
                    self.mode = Mode::Preview;
                }
                None
            }
            Action::Edit => Some(Command::Edit(self.storage.chat_path.clone())),
            Action::Delete => {
                self.pending_id = Some(id.to_string());
                self.mode = Mode::ConfirmDelete;
                None
            }
        }
    }

    fn handle_select_target(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Insert;
                self.pending_id = None;
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.target_index > 0 {
                    self.target_index -= 1;
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.target_index + 1 < self.target_files.len() {
                    self.target_index += 1;
                }
                None
            }
            KeyCode::Char('v') => {
                // Preview the highlighted file instead of moving.
                if let Some(path) = self.target_files.get(self.target_index).cloned() {
                    self.open_file_preview(&path);
                }
                None
            }
            KeyCode::Enter => {
                let Some(path) = self.target_files.get(self.target_index).cloned() else {
                    self.mode = Mode::Insert;
                    return None;
                };
                self.perform_move_to(&path);
                None
            }
            _ => None,
        }
    }

    fn handle_new_file(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Insert;
                self.pending_id = None;
                None
            }
            KeyCode::Enter => {
                let name = self.new_file_input.clone();
                if let Some(id) = self.pending_id.take() {
                    match self.storage.create_named_file(&name) {
                        Ok(path) => self.perform_move_to_id(&path, &id),
                        Err(e) => self.set_status(format!("Error: {e}")),
                    }
                }
                self.mode = Mode::Insert;
                None
            }
            KeyCode::Backspace => {
                self.new_file_input.pop();
                None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.new_file_input.push(c);
                None
            }
            _ => None,
        }
    }

    fn handle_confirm_delete(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(id) = self.pending_id.take() {
                    match self.storage.remove_message_by_id(&id) {
                        Ok(true) => {
                            self.set_status("Deleted");
                            self.reload();
                        }
                        Ok(false) => self.set_status("Message not found"),
                        Err(e) => self.set_status(format!("Error: {e}")),
                    }
                }
                self.mode = Mode::Insert;
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_id = None;
                self.mode = Mode::Insert;
                None
            }
            _ => None,
        }
    }

    fn handle_preview(&mut self, key: KeyEvent) -> Option<Command> {
        let Some(p) = self.preview.as_mut() else {
            self.mode = self.preview_return;
            return None;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.preview = None;
                self.mode = self.preview_return;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                p.scroll = p.scroll.saturating_add(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                p.scroll = p.scroll.saturating_sub(1);
                None
            }
            KeyCode::PageDown => {
                p.scroll = p.scroll.saturating_add(10);
                None
            }
            KeyCode::PageUp => {
                p.scroll = p.scroll.saturating_sub(10);
                None
            }
            _ => None,
        }
    }

    fn perform_move_to(&mut self, path: &Path) {
        let Some(id) = self.pending_id.take() else {
            self.mode = Mode::Insert;
            return;
        };
        self.perform_move_to_id(path, &id);
        self.mode = Mode::Insert;
    }

    fn perform_move_to_id(&mut self, path: &Path, id: &str) {
        let Some(m) = self.message_clone(id) else {
            self.set_status("Message not found");
            return;
        };
        match self.storage.move_to_markdown(path, &m) {
            Ok(()) => {
                self.set_status(format!(
                    "Moved to {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                self.reload();
            }
            Err(e) => self.set_status(format!("Error: {e}")),
        }
    }

    fn open_file_preview(&mut self, path: &Path) {
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Preview".into());
        let text = std::fs::read_to_string(path).unwrap_or_default();
        self.preview = Some(Preview {
            title,
            source: text,
            scroll: 0,
        });
        self.preview_return = self.mode;
        self.mode = Mode::Preview;
    }

    fn message_clone(&self, id: &str) -> Option<Message> {
        self.messages.iter().find(|m| m.id == id).cloned()
    }

    fn send_message(&mut self) {
        let body = self.input.trim();
        if body.is_empty() {
            return;
        }
        match self.storage.append_chat_message(body) {
            Ok(_) => {
                self.input.clear();
                self.input_cursor = 0;
                self.reload();
                self.selected = self.messages.len().saturating_sub(1);
                self.scroll = u32::MAX as u16; // jump to bottom
                self.set_status("Saved");
            }
            Err(e) => self.set_status(format!("Error: {e}")),
        }
    }

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use tempfile::tempdir;

    fn make_app() -> (App, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let st = Storage::new(dir.path()).unwrap();
        st.ensure_files().unwrap();
        st.create_named_file("Work").unwrap();
        st.create_named_file("Ideas").unwrap();
        (App::new(st).unwrap(), dir)
    }

    #[test]
    fn bare_enter_inserts_newline_in_insert_mode() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Insert;
        app.input = "hi".to_string();
        app.input_cursor = app.input.chars().count();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input, "hi\n", "bare Enter should insert a newline");
        assert_eq!(app.messages.len(), 0, "bare Enter must not send");
    }

    #[test]
    fn modifier_enter_sends_message() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Insert;
        app.input = "hi".to_string();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert!(app.input.is_empty(), "Alt+Enter should send");
        assert_eq!(app.messages.len(), 1);

        // Ctrl+Enter also sends.
        app.input = "again".to_string();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert_eq!(app.messages.len(), 2);
    }

    #[test]
    fn file_list_popup_navigation() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Normal;
        // 'f' opens the file-list popup.
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileList);
        // Files: Ideas.md, TODO.md, Work.md (sorted).
        assert!(app.sidebar_files.len() >= 3);

        // j/k move the selection.
        let start = app.sidebar_index;
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.sidebar_index, start + 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.sidebar_index, start);

        // Esc closes the popup.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn preview_from_normal_returns_to_normal() {
        let (mut app, _dir) = make_app();
        app.storage.append_chat_message("hello").unwrap();
        app.reload();
        app.mode = Mode::Normal;
        // 'v' views the selected message.
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Preview);
        // Esc returns to Normal (previously it wrongly went to Insert).
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn preview_from_filelist_returns_to_filelist() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileList);
        // Preview the selected file.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Preview);
        // Esc returns to the file list, not Insert.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileList);
    }

    #[test]
    fn insert_types_at_cursor_not_append() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Insert;
        app.input = "ab".to_string();
        app.input_cursor = 1; // between 'a' and 'b'
        app.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
        assert_eq!(app.input, "aXb");
        assert_eq!(app.input_cursor, 2);
    }

    #[test]
    fn backspace_deletes_before_cursor() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Insert;
        app.input = "abc".to_string();
        app.input_cursor = 3;
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)); // cursor -> 2
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)); // drop 'b'
        assert_eq!(app.input, "ac");
        assert_eq!(app.input_cursor, 1);
    }

    #[test]
    fn cursor_up_preserves_column() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Insert;
        app.input = "abcd\nef".to_string();
        app.input_cursor = 7; // end of line 2 (col 2)
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        // Line 1 ("abcd") is long enough; col 2 → index 2.
        assert_eq!(app.input_cursor, 2);
    }

    #[test]
    fn archive_moves_message_to_archive_file() {
        let (mut app, _dir) = make_app();
        app.storage.append_chat_message("to be archived").unwrap();
        app.reload();
        assert_eq!(app.messages.len(), 1);
        app.mode = Mode::Normal;
        // 'a' archives the selected message (reuses the move-to-markdown path).
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.messages.is_empty(), "message should leave the chat");
        let body = std::fs::read_to_string(&app.storage.archive_path).unwrap();
        assert!(body.contains("to be archived"), "message should land in ARCHIVE.md");
    }

    #[test]
    fn todo_panel_toggles_task() {
        let (mut app, _dir) = make_app();
        // Seed a task by moving a message to TODO.md.
        let m = app.storage.append_chat_message("buy milk").unwrap();
        app.storage.move_to_todo(&m).unwrap();
        app.reload();
        app.mode = Mode::Normal;

        // 'T' opens the todo panel.
        app.handle_key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Todo);
        assert_eq!(app.todo_items.len(), 1);
        assert!(!app.todo_items[0].checked);

        // Enter toggles it on, then off again.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.todo_items[0].checked);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.todo_items[0].checked);

        // Esc closes the panel.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
    }

    fn file_index(app: &App, stem: &str) -> usize {
        app.sidebar_files
            .iter()
            .position(|p| p.file_stem().and_then(|s| s.to_str()) == Some(stem))
            .unwrap()
    }

    #[test]
    fn fuzzy_filter_narrows_file_list() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        let total = app.sidebar_files.len();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileSearch);
        for c in "work".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // Only Work.md matches the subsequence "work".
        assert_eq!(app.sidebar_files.len(), 1);
        assert!(app.sidebar_files.len() < total);
        assert_eq!(
            app.sidebar_files[0]
                .file_stem()
                .and_then(|s| s.to_str()),
            Some("Work")
        );
    }

    #[test]
    fn file_search_navigates_and_previews_result() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileSearch);
        // Arrows move the selection within the live results.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        // Enter opens (previews) the highlighted result.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Preview);
        // Esc returns to the search, then a second Esc cancels it.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileSearch);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileList);
        assert!(app.file_query.is_empty());
    }

    #[test]
    fn rename_file_via_picker() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        app.sidebar_index = file_index(&app, "Work");
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileRename);
        assert_eq!(app.rename_input, "Work");
        app.rename_input.clear();
        for c in "Renamed".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileList);
        assert!(app
            .all_files
            .iter()
            .any(|p| p.file_name().and_then(|n| n.to_str()) == Some("Renamed.md")));
        assert!(!app
            .all_files
            .iter()
            .any(|p| p.file_stem().and_then(|s| s.to_str()) == Some("Work")));
    }

    #[test]
    fn delete_file_via_picker() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        app.sidebar_index = file_index(&app, "Ideas");
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileDelete);
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::FileList);
        assert!(!app
            .all_files
            .iter()
            .any(|p| p.file_stem().and_then(|s| s.to_str()) == Some("Ideas")));
    }
}
