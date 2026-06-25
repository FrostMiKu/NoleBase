//! Application state and event handling.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use crate::model::{Action, Message};
use crate::storage::Storage;

fn point_in_rect(col: u16, row: u16, area: ratatui::layout::Rect) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// A request from the app to the terminal driver (which owns the TUI lifecycle).
#[derive(Debug)]
pub enum Command {
    Quit,
    /// Suspend the TUI, open `path` in `$EDITOR`, then reload on return.
    Edit(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub mode: Mode,
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
    pub sidebar_index: usize,
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
            mode: Mode::Insert,
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
            sidebar_index: 0,
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
        self.sidebar_files = self.storage.list_markdown_files().unwrap_or_default();
        if self.sidebar_index >= self.sidebar_files.len() {
            self.sidebar_index = 0;
        }
        self.mode = Mode::FileList;
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
                self.input.push('\n');
                None
            }
            KeyCode::Tab | KeyCode::Esc => {
                self.mode = Mode::Normal;
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                self.input.push(c);
                None
            }
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => {
                if self.input.is_empty() {
                    Some(Command::Quit)
                } else {
                    self.input.clear();
                    None
                }
            }
            _ => None,
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
            self.mode = Mode::Insert;
            return None;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.preview = None;
                self.mode = Mode::Insert;
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
}
