//! Application state and event handling.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use crate::model::{Action, Message, SearchHit, SearchHitbox, TodoHitbox, TodoItem};
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

/// First non-blank line of `body` containing `query_lower` (case-insensitive),
/// falling back to the first non-blank line — used to preview a search match.
fn best_line(body: &str, query_lower: &str) -> String {
    for line in body.lines() {
        let t = line.trim();
        if !t.is_empty() && t.to_lowercase().contains(query_lower) {
            return t.to_string();
        }
    }
    body.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

// --- cursor-editing primitives (shared by the compose box and the message
// editor; operate on an arbitrary buffer + char-index cursor) ---

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or_else(|| s.len())
}

fn insert_char(buf: &mut String, cursor: &mut usize, c: char) {
    let b = char_to_byte(buf, *cursor);
    buf.insert(b, c);
    *cursor += 1;
}

fn delete_backward(buf: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let prev = char_to_byte(buf, *cursor - 1);
    let cur = char_to_byte(buf, *cursor);
    buf.replace_range(prev..cur, "");
    *cursor -= 1;
}

fn delete_forward(buf: &mut String, cursor: &mut usize) {
    let total = buf.chars().count();
    if *cursor >= total {
        return;
    }
    let cur = char_to_byte(buf, *cursor);
    let next = char_to_byte(buf, *cursor + 1);
    buf.replace_range(cur..next, "");
}

/// Insert `s` at the cursor, advancing it by the number of chars inserted.
fn paste_into(buf: &mut String, cursor: &mut usize, s: &str) {
    let n = s.chars().count();
    let b = char_to_byte(buf, *cursor);
    buf.insert_str(b, s);
    *cursor += n;
}

fn move_cursor(buf: &str, cursor: usize, m: CursorMove) -> usize {
    let chars: Vec<char> = buf.chars().collect();
    let total = chars.len();
    let i = cursor;
    let line_start = {
        let mut j = i;
        while j > 0 && chars[j - 1] != '\n' {
            j -= 1;
        }
        j
    };
    match m {
        CursorMove::Left => i.saturating_sub(1),
        CursorMove::Right => (i + 1).min(total),
        CursorMove::LineStart => line_start,
        CursorMove::LineEnd => {
            let mut j = i;
            while j < total && chars[j] != '\n' {
                j += 1;
            }
            j
        }
        CursorMove::Up | CursorMove::Down => {
            let col = i - line_start;
            let target_line_start = if matches!(m, CursorMove::Up) {
                if line_start == 0 {
                    return i; // already on the first line
                }
                let mut j = line_start - 1;
                while j > 0 && chars[j - 1] != '\n' {
                    j -= 1;
                }
                j
            } else {
                let mut j = i;
                while j < total && chars[j] != '\n' {
                    j += 1;
                }
                if j >= total {
                    return i; // already on the last line
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
            (target_line_start + col).min(target_line_end)
        }
    }
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

/// A chat-removing operation recorded so `u` can reverse it.
#[derive(Debug, Clone)]
enum UndoOp {
    Delete(Message),
    Move {
        msg: Message,
        target: PathBuf,
        appended: String,
    },
    /// Body edit: holds the pre-edit message so undo restores the old body.
    Edit(Message),
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
    /// Full-text content search across messages and note files.
    Search,
    /// Keybinding reference overlay.
    Help,
    /// Editing a single message's body in-app.
    EditMessage,
    /// Asking before discarding unsaved message edits.
    ConfirmDiscardEdit,
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
    /// Full-text search state.
    pub search_query: String,
    pub search_results: Vec<SearchHit>,
    pub search_index: usize,
    /// Clickable rows in the search panel, rebuilt each frame.
    pub search_hitboxes: Vec<SearchHitbox>,
    /// Stack of recent chat-removing operations reversible with `u`.
    undo_stack: Vec<UndoOp>,
    /// Vertical scroll of the help overlay.
    pub help_scroll: u16,
    /// In-app single-message editor state.
    pub edit_input: String,
    pub edit_cursor: usize,
    pub edit_id: String,
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
            search_query: String::new(),
            search_results: Vec::new(),
            search_index: 0,
            search_hitboxes: Vec::new(),
            undo_stack: Vec::new(),
            help_scroll: 0,
            edit_input: String::new(),
            edit_cursor: 0,
            edit_id: String::new(),
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

    /// Open the content-search panel with an empty query.
    pub fn open_search(&mut self) {
        self.search_query.clear();
        self.search_results.clear();
        self.search_index = 0;
        self.mode = Mode::Search;
    }

    /// Open the keybinding reference overlay.
    pub fn open_help(&mut self) {
        self.help_scroll = 0;
        self.mode = Mode::Help;
    }

    /// Recompute `search_results` for the current query: matching chat messages
    /// first, then matching lines across note files.
    fn recompute_search(&mut self) {
        let q = self.search_query.trim().to_lowercase();
        let mut results: Vec<SearchHit> = Vec::new();
        if !q.is_empty() {
            for m in &self.messages {
                if m.body.to_lowercase().contains(&q) {
                    results.push(SearchHit::Message {
                        id: m.id.clone(),
                        text: best_line(&m.body, &q),
                    });
                }
            }
            results.extend(self.storage.search_file_lines(&q));
        }
        self.search_results = results;
        if self.search_index >= self.search_results.len() {
            self.search_index = self.search_results.len().saturating_sub(1);
        }
    }

    /// Open the `index`-th search result in the preview modal (Esc returns to
    /// the search panel).
    fn jump_to_search_result(&mut self, index: usize) {
        let Some(hit) = self.search_results.get(index).cloned() else {
            return;
        };
        match hit {
            SearchHit::Message { id, .. } => {
                if let Some(m) = self.message_clone(&id) {
                    self.preview = Some(Preview {
                        title: format!("Message {}", m.created_at.format("%Y-%m-%d %H:%M")),
                        source: m.body.clone(),
                        scroll: 0,
                    });
                    self.preview_return = Mode::Search;
                    self.mode = Mode::Preview;
                }
            }
            SearchHit::FileLine { path, .. } => {
                // open_file_preview records preview_return = Search itself.
                self.open_file_preview(&path);
            }
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
            Mode::Search => self.handle_search(key),
            Mode::Help => self.handle_help(key),
            Mode::EditMessage => self.handle_editmessage(key),
            Mode::ConfirmDiscardEdit => self.handle_confirm_discard_edit(key),
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
                // A clicked search result opens it.
                let search_hit = self
                    .search_hitboxes
                    .iter()
                    .find(|h| point_in_rect(ev.column, ev.row, h.area))
                    .map(|h| h.index);
                if let Some(index) = search_hit {
                    self.search_index = index;
                    self.jump_to_search_result(index);
                    return None;
                }
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
                        | Mode::Search
                        | Mode::Help
                        | Mode::EditMessage
                        | Mode::ConfirmDiscardEdit
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
        match key.code {
            // Enter sends. Enter with Shift/Ctrl/Alt inserts a newline —
            // Shift+Enter is the usual chat convention; Ctrl/Alt+Enter are
            // reliable fallbacks, since many terminals don't distinguish
            // Shift+Enter from a bare Enter.
            KeyCode::Enter
                if mods.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_char('\n');
                None
            }
            KeyCode::Enter => {
                self.send_message();
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
            // Ctrl+J (the raw LF byte) inserts a newline — a reliable fallback
            // for terminals that don't distinguish Shift+Enter from Enter.
            KeyCode::Char('j') if mods.contains(KeyModifiers::CONTROL) => {
                self.insert_char('\n');
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
    fn insert_char(&mut self, c: char) {
        insert_char(&mut self.input, &mut self.input_cursor, c);
    }

    fn delete_backward(&mut self) {
        delete_backward(&mut self.input, &mut self.input_cursor);
    }

    fn delete_forward(&mut self) {
        delete_forward(&mut self.input, &mut self.input_cursor);
    }

    fn move_cursor(&mut self, m: CursorMove) {
        self.input_cursor = move_cursor(&self.input, self.input_cursor, m);
    }

    /// Insert pasted text at the cursor (used for bracketed-paste events).
    /// Normalizes CRLF/CR to LF. Only acts in text-entry modes.
    pub fn handle_paste(&mut self, s: &str) {
        let text = s.replace("\r\n", "\n").replace('\r', "\n");
        match self.mode {
            Mode::Insert => paste_into(&mut self.input, &mut self.input_cursor, &text),
            Mode::EditMessage => paste_into(&mut self.edit_input, &mut self.edit_cursor, &text),
            _ => {}
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
            KeyCode::Char('/') => {
                self.open_search();
                None
            }
            KeyCode::Char('u') => {
                self.undo();
                None
            }
            KeyCode::Char('?') => {
                self.open_help();
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

    fn handle_search(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                None
            }
            // Arrow keys only (letters are query input).
            KeyCode::Down => {
                if self.search_index + 1 < self.search_results.len() {
                    self.search_index += 1;
                }
                None
            }
            KeyCode::Up => {
                if self.search_index > 0 {
                    self.search_index -= 1;
                }
                None
            }
            KeyCode::Enter => {
                self.jump_to_search_result(self.search_index);
                None
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.recompute_search();
                None
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_query.push(c);
                self.recompute_search();
                None
            }
            _ => None,
        }
    }

    fn edit_has_changes(&self) -> bool {
        self.message_clone(&self.edit_id)
            .is_none_or(|message| message.body != self.edit_input)
    }

    fn handle_confirm_discard_edit(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.mode = Mode::Normal;
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.mode = Mode::EditMessage;
                None
            }
            _ => None,
        }
    }

    fn handle_help(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help_scroll = self.help_scroll.saturating_add(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
                None
            }
            KeyCode::PageDown => {
                self.help_scroll = self.help_scroll.saturating_add(8);
                None
            }
            KeyCode::PageUp => {
                self.help_scroll = self.help_scroll.saturating_sub(8);
                None
            }
            _ => None,
        }
    }

    fn handle_editmessage(&mut self, key: KeyEvent) -> Option<Command> {
        let mods = key.modifiers;
        match key.code {
            // Enter saves (mirrors the compose box: Enter commits). Enter with
            // any modifier, or Ctrl+J, inserts a newline instead.
            KeyCode::Enter
                if mods.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                insert_char(&mut self.edit_input, &mut self.edit_cursor, '\n');
                None
            }
            KeyCode::Enter => {
                self.save_edit();
                None
            }
            KeyCode::Char('j') if mods.contains(KeyModifiers::CONTROL) => {
                insert_char(&mut self.edit_input, &mut self.edit_cursor, '\n');
                None
            }
            KeyCode::Esc => {
                if self.edit_has_changes() {
                    self.mode = Mode::ConfirmDiscardEdit;
                } else {
                    self.mode = Mode::Normal;
                }
                None
            }
            KeyCode::Backspace => {
                delete_backward(&mut self.edit_input, &mut self.edit_cursor);
                None
            }
            KeyCode::Delete => {
                delete_forward(&mut self.edit_input, &mut self.edit_cursor);
                None
            }
            KeyCode::Left => {
                self.edit_cursor = move_cursor(&self.edit_input, self.edit_cursor, CursorMove::Left);
                None
            }
            KeyCode::Right => {
                self.edit_cursor = move_cursor(&self.edit_input, self.edit_cursor, CursorMove::Right);
                None
            }
            KeyCode::Up => {
                self.edit_cursor = move_cursor(&self.edit_input, self.edit_cursor, CursorMove::Up);
                None
            }
            KeyCode::Down => {
                self.edit_cursor = move_cursor(&self.edit_input, self.edit_cursor, CursorMove::Down);
                None
            }
            KeyCode::Home => {
                self.edit_cursor = move_cursor(&self.edit_input, self.edit_cursor, CursorMove::LineStart);
                None
            }
            KeyCode::End => {
                self.edit_cursor = move_cursor(&self.edit_input, self.edit_cursor, CursorMove::LineEnd);
                None
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                insert_char(&mut self.edit_input, &mut self.edit_cursor, c);
                None
            }
            _ => None,
        }
    }

    /// Save the in-app edit: record undo (old body), rewrite the block.
    fn save_edit(&mut self) {
        let Some(m) = self.message_clone(&self.edit_id) else {
            self.set_status("Message not found");
            self.mode = Mode::Normal;
            return;
        };
        let old = m.clone();
        let mut updated = m;
        updated.body = self.edit_input.clone();
        match self.storage.replace_message(&updated) {
            Ok(true) => {
                self.record_undo(UndoOp::Edit(old));
                self.set_status("Saved");
                self.reload();
                if let Some(idx) = self.messages.iter().position(|x| x.id == self.edit_id) {
                    self.selected = idx;
                }
                self.scroll = u32::MAX as u16;
            }
            Ok(false) => self.set_status("Message not found"),
            Err(e) => self.set_status(format!("Error: {e}")),
        }
        self.mode = Mode::Normal;
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
                        Ok(appended) => {
                            self.record_undo(UndoOp::Move {
                                msg: m,
                                target: self.storage.todo_path.clone(),
                                appended,
                            });
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
                        Ok(appended) => {
                            self.record_undo(UndoOp::Move {
                                msg: m,
                                target: self.storage.archive_path.clone(),
                                appended,
                            });
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
            Action::Edit => {
                // Open the in-app single-message editor (avoids editing the
                // whole CHAT.md and risking the note-msg block markers).
                if let Some(m) = self.message_clone(id) {
                    self.edit_id = m.id.clone();
                    self.edit_input = m.body.clone();
                    self.edit_cursor = self.edit_input.chars().count();
                    self.mode = Mode::EditMessage;
                }
                None
            }
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
                    let msg = self.message_clone(&id);
                    match self.storage.remove_message_by_id(&id) {
                        Ok(true) => {
                            if let Some(m) = msg {
                                self.record_undo(UndoOp::Delete(m));
                            }
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
            Ok(appended) => {
                self.record_undo(UndoOp::Move {
                    msg: m,
                    target: path.to_path_buf(),
                    appended,
                });
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

    /// Record a chat-removing operation for `u` to reverse (capped stack).
    fn record_undo(&mut self, op: UndoOp) {
        const CAP: usize = 50;
        if self.undo_stack.len() >= CAP {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(op);
    }

    /// Reverse the most recent chat-removing operation.
    fn undo(&mut self) {
        let Some(op) = self.undo_stack.pop() else {
            self.set_status("Nothing to undo");
            return;
        };
        let status = match op {
            UndoOp::Delete(msg) => match self.storage.restore_message_to_chat(&msg) {
                Ok(()) => "Undid delete".to_string(),
                Err(e) => format!("Undo error: {e}"),
            },
            UndoOp::Move {
                msg,
                target,
                appended,
            } => match self.storage.restore_message_to_chat(&msg) {
                Ok(()) => {
                    let name = target
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let cleaned = self
                        .storage
                        .remove_first_occurrence(&target, &appended)
                        .unwrap_or(false);
                    if cleaned {
                        format!("Undid move to {name}")
                    } else {
                        format!("Undid move (couldn't tidy {name})")
                    }
                }
                Err(e) => format!("Undo error: {e}"),
            },
            UndoOp::Edit(msg) => match self.storage.replace_message(&msg) {
                Ok(true) => "Undid edit".to_string(),
                Ok(false) => "Undid edit (message gone)".to_string(),
                Err(e) => format!("Undo error: {e}"),
            },
        };
        self.set_status(status);
        self.reload();
        self.selected = self.messages.len().saturating_sub(1);
        self.scroll = u32::MAX as u16;
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
    fn bare_enter_sends_in_insert_mode() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Insert;
        app.input = "hi".to_string();
        app.input_cursor = app.input.chars().count();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.input.is_empty(), "bare Enter should send (clear input)");
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn modifier_enter_inserts_newline() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Insert;
        app.input = "hi".to_string();
        app.input_cursor = app.input.chars().count();

        // Shift+Enter inserts a newline (the requested behaviour).
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.input, "hi\n", "Shift+Enter should insert a newline");
        assert_eq!(app.messages.len(), 0, "Shift+Enter must not send");

        // Ctrl/Alt+Enter also insert a newline (reliable fallbacks).
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert_eq!(app.input, "hi\n\n\n");
        assert_eq!(app.messages.len(), 0);

        // Ctrl+J (the LF byte) inserts a newline too.
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "hi\n\n\n\n");
    }

    #[test]
    fn paste_inserts_at_cursor_and_normalizes_newlines() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Insert;
        app.input = "ab".to_string();
        app.input_cursor = 1; // between 'a' and 'b'
        // CRLF must become a single LF.
        app.handle_paste("X\r\nY");
        assert_eq!(app.input, "aX\nYb");
        assert_eq!(app.input_cursor, 4);
    }

    #[test]
    fn paste_works_in_editor_and_is_ignored_in_command_modes() {
        let (mut app, _dir) = make_app();
        // EditMessage accepts paste at the cursor.
        app.mode = Mode::EditMessage;
        app.edit_id = "x".into();
        app.edit_input = "ab".to_string();
        app.edit_cursor = 2;
        app.handle_paste("Z");
        assert_eq!(app.edit_input, "abZ");

        // Normal mode ignores paste.
        app.mode = Mode::Normal;
        app.handle_paste("ignored");
        assert!(app.input.is_empty());
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
    fn undo_delete_restores_message() {
        let (mut app, _dir) = make_app();
        let m = app.storage.append_chat_message("oops").unwrap();
        app.reload();
        app.mode = Mode::Normal;
        // delete via 'd' then confirm 'y'.
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::ConfirmDelete);
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.messages.is_empty());

        // 'u' restores it with the same id.
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].body, "oops");
        assert_eq!(app.messages[0].id, m.id);

        // A second 'u' has nothing to undo.
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn undo_move_restores_chat_and_cleans_target() {
        let (mut app, _dir) = make_app();
        app.storage.append_chat_message("file me").unwrap();
        app.reload();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.messages.is_empty(), "archived out of chat");
        let before = std::fs::read_to_string(&app.storage.archive_path).unwrap();
        assert!(before.contains("file me"));

        // 'u' brings it back to chat AND removes the filed copy.
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].body, "file me");
        let after = std::fs::read_to_string(&app.storage.archive_path).unwrap();
        assert!(!after.contains("file me"), "filed copy should be removed on undo");
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

    #[test]
    fn search_finds_message_and_file_and_jumps() {
        let (mut app, _dir) = make_app();
        app.storage
            .append_chat_message("remember to rust the bike")
            .unwrap();
        app.reload();
        std::fs::write(
            app.storage.root.join("Work.md"),
            "# Work\n\ndeep rust notes\n",
        )
        .unwrap();

        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Search);
        for c in "rust".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(app
            .search_results
            .iter()
            .any(|h| matches!(h, SearchHit::Message { .. })));
        assert!(app
            .search_results
            .iter()
            .any(|h| matches!(h, SearchHit::FileLine { .. })));

        // Enter opens a result; Esc returns to the search, then closes it.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Preview);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Search);
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn help_overlay_opens_and_closes() {
        let (mut app, _dir) = make_app();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Help);
        assert_eq!(app.help_scroll, 0);
        // Scrolling advances the offset.
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.help_scroll, 1);
        // '?' closes it (Esc works too).
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn edit_message_saves_and_undo_restores() {
        let (mut app, _dir) = make_app();
        let m = app.storage.append_chat_message("hello").unwrap();
        app.reload();
        app.mode = Mode::Normal;
        // 'e' opens the in-app editor seeded with the body.
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::EditMessage);
        assert_eq!(app.edit_input, "hello");

        // Replace the body.
        app.edit_input.clear();
        app.edit_cursor = 0;
        for c in "world".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // Enter saves.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].body, "world");
        assert_eq!(app.messages[0].id, m.id, "id preserved");

        // 'u' restores the pre-edit body.
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        assert_eq!(app.messages[0].body, "hello");
    }

    #[test]
    fn edit_message_esc_requires_confirmation_before_discarding() {
        let (mut app, _dir) = make_app();
        app.storage.append_chat_message("keep").unwrap();
        app.reload();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.edit_input.clear();
        app.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::ConfirmDiscardEdit);
        assert_eq!(app.messages[0].body, "keep", "prompt must not save edits");

        // Esc from the prompt keeps the buffer and returns to editing.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::EditMessage);
        assert_eq!(app.edit_input, "X");

        // Confirming the second prompt discards the edit.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.messages[0].body, "keep", "cancel must not save edits");
    }

    #[test]
    fn edit_message_esc_closes_immediately_when_unchanged() {
        let (mut app, _dir) = make_app();
        app.storage.append_chat_message("keep").unwrap();
        app.reload();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn edit_message_discard_confirmation_n_keeps_editing() {
        let (mut app, _dir) = make_app();
        app.storage.append_chat_message("keep").unwrap();
        app.reload();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        assert_eq!(app.mode, Mode::EditMessage);
        assert_eq!(app.edit_input, "keep!");
    }

    #[test]
    fn edit_message_newline_keys_dont_save() {
        let (mut app, _dir) = make_app();
        app.storage.append_chat_message("hi").unwrap();
        app.reload();
        app.mode = Mode::Normal;
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        app.edit_cursor = app.edit_input.chars().count();
        // Shift+Enter and Ctrl+J insert newlines without saving.
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(app.mode, Mode::EditMessage, "must not have saved");
        assert_eq!(app.edit_input, "hi\n\n");
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
