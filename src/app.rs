//! Application state and event handling.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;

use anyhow::Context;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::agent::{
    Agent, AgentEvent, ApprovalDecision, ApprovalRequest, AskUserRequest, AskUserResponse,
    PermissionMode,
};
use crate::model::{
    Action, ButtonHitbox, DialogOptionHitbox, FileGroup, FileGroupHitbox, FileHitbox, FileListRow,
    LinkHitbox, LinkTarget, Message, NoteFile, SearchHit, SearchHitbox, TodoHitbox, TodoItem,
    WikiLinkCandidate, WikiLinkHitbox,
};
use crate::notification::NotificationService;
use crate::storage::Storage;

fn point_in_rect(col: u16, row: u16, area: Rect) -> bool {
    col >= area.x
        && col < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn in_area(col: u16, row: u16, area: Option<Rect>) -> bool {
    area.is_some_and(|area| point_in_rect(col, row, area))
}

fn wiki_name_matches(path: &Path, requested: &str) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(requested))
        || path
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(requested))
}

/// Case-insensitive subsequence matching. An empty query matches every file.
fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let needle: Vec<char> = needle.to_lowercase().chars().collect();
    let mut offset = 0;
    for wanted in needle {
        let Some(found) = hay[offset..]
            .iter()
            .position(|candidate| *candidate == wanted)
        else {
            return false;
        };
        offset += found + 1;
    }
    true
}

fn best_line(body: &str, query_lower: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.to_lowercase().contains(query_lower))
        .or_else(|| body.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("")
        .to_string()
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn insert_char(buffer: &mut String, cursor: &mut usize, character: char) {
    buffer.insert(char_to_byte(buffer, *cursor), character);
    *cursor += 1;
}

fn delete_backward(buffer: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = char_to_byte(buffer, *cursor - 1);
    let end = char_to_byte(buffer, *cursor);
    buffer.replace_range(start..end, "");
    *cursor -= 1;
}

fn delete_forward(buffer: &mut String, cursor: &mut usize) {
    if *cursor >= buffer.chars().count() {
        return;
    }
    let start = char_to_byte(buffer, *cursor);
    let end = char_to_byte(buffer, *cursor + 1);
    buffer.replace_range(start..end, "");
}

fn paste_into(buffer: &mut String, cursor: &mut usize, text: &str) {
    buffer.insert_str(char_to_byte(buffer, *cursor), text);
    *cursor += text.chars().count();
}

fn move_cursor(buffer: &str, cursor: usize, movement: CursorMove) -> usize {
    let chars: Vec<char> = buffer.chars().collect();
    let total = chars.len();
    let mut line_start = cursor;
    while line_start > 0 && chars[line_start - 1] != '\n' {
        line_start -= 1;
    }

    match movement {
        CursorMove::Left => cursor.saturating_sub(1),
        CursorMove::Right => (cursor + 1).min(total),
        CursorMove::LineStart => line_start,
        CursorMove::LineEnd => {
            let mut end = cursor;
            while end < total && chars[end] != '\n' {
                end += 1;
            }
            end
        }
        CursorMove::Up | CursorMove::Down => {
            let column = cursor - line_start;
            let target_start = if movement == CursorMove::Up {
                if line_start == 0 {
                    return cursor;
                }
                let mut start = line_start - 1;
                while start > 0 && chars[start - 1] != '\n' {
                    start -= 1;
                }
                start
            } else {
                let mut end = cursor;
                while end < total && chars[end] != '\n' {
                    end += 1;
                }
                if end == total {
                    return cursor;
                }
                end + 1
            };
            let mut target_end = target_start;
            while target_end < total && chars[target_end] != '\n' {
                target_end += 1;
            }
            (target_start + column).min(target_end)
        }
    }
}

/// A request for the terminal owner, which controls suspension and process exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Edit(PathBuf),
    EditMessage { id: String, body: String },
    OpenLink(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorMove {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
}

#[derive(Debug, Clone)]
enum UndoOp {
    Delete(Message),
    Archive(Message),
    Move {
        message: Message,
        target: PathBuf,
        appended: String,
    },
    Edit(Message),
}

/// Keyboard focus is independent from the content shown in the center pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Center,
    Compose,
    Files,
    Todo,
    Agent,
}

/// Content currently occupying the center pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterView {
    Chat,
    Document,
    Search,
    DocumentSearch,
}

/// Interaction taking place inside the files pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesContext {
    Browse,
    Search,
    MoveTarget,
    NewTarget,
    Rename,
}

/// Modal state. Removing an overlay exposes the unchanged state beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    ConfirmDeleteMessage,
    ConfirmDeleteFile,
    Help,
    AiPrompt,
    Approval,
    AskUser,
    WikiLinkChoice,
    /// A caller-provided command dialog. Its mode and purpose live in
    /// [`DialogState`], allowing new command-style interactions without
    /// adding another overlay variant.
    #[allow(dead_code)]
    Dialog,
}

/// The interaction model used by every modal dialog in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    Confirm,
    SingleSelect,
    MultiSelect,
    SelectOrInput,
    FreeText,
    Approval,
    Informational,
}

/// Business purpose of a dialog. The mode controls interaction while the
/// purpose controls the result that is sent to the owning subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogPurpose {
    DeleteMessage,
    DeleteFile,
    AgentPrompt,
    AgentApproval,
    AskUser,
    WikiLinkChoice,
    Help,
    NewFile,
    RenameFile,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogOption {
    pub label: String,
    pub hint: Option<String>,
}

impl DialogOption {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: None,
        }
    }

    pub fn with_hint(label: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: Some(hint.into()),
        }
    }
}

/// State shared by confirmations, selectors, text prompts, approvals and
/// Agent questions. `options` can be used by both single- and multi-select
/// dialogs; `checked` stores the multi-select state by option index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogState {
    pub title: String,
    pub message: String,
    pub mode: DialogMode,
    pub purpose: DialogPurpose,
    pub options: Vec<DialogOption>,
    pub selected: usize,
    pub checked: Vec<bool>,
    pub input: String,
    pub cursor: usize,
    pub scroll: u16,
}

impl DialogState {
    pub fn new(
        title: impl Into<String>,
        message: impl Into<String>,
        mode: DialogMode,
        purpose: DialogPurpose,
        options: Vec<DialogOption>,
    ) -> Self {
        let checked = vec![false; options.len()];
        Self {
            title: title.into(),
            message: message.into(),
            mode,
            purpose,
            options,
            selected: 0,
            checked,
            input: String::new(),
            cursor: 0,
            scroll: 0,
        }
    }

    pub fn selected_option(&self) -> Option<&DialogOption> {
        self.options.get(self.selected)
    }

    pub fn selected_options(&self) -> Vec<String> {
        self.options
            .iter()
            .enumerate()
            .filter_map(|(index, option)| {
                self.checked
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                    .then_some(option.label.clone())
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogResult {
    Confirm(bool),
    Selected(String),
    SelectedMany(Vec<String>),
    Text(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentKind {
    File(PathBuf),
    Message(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentReturn {
    Chat,
    Search,
}

/// A file or message rendered as markdown in the center pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub kind: DocumentKind,
    pub title: String,
    pub source: String,
    pub scroll: u16,
    /// One-based source line to reveal on the next render.
    pub target_line: Option<usize>,
    pub return_to: DocumentReturn,
}

/// Screen geometry recorded by the renderer after each layout pass.
///
/// Mouse wheel events use these rectangles instead of the keyboard focus, so a
/// wheel over Files/Todo/Center always affects the pane under the pointer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutSnapshot {
    pub files: Option<Rect>,
    pub center: Option<Rect>,
    pub compose: Option<Rect>,
    pub todo: Option<Rect>,
    pub agent: Option<Rect>,
    pub overlay: Option<Rect>,
}

pub struct App {
    pub storage: Storage,

    pub focus: Focus,
    pub center_view: CenterView,
    pub files_context: FilesContext,
    pub overlay: Option<Overlay>,
    pub document: Option<Document>,

    pub messages: Vec<Message>,
    pub selected: usize,
    pub scroll: u16,
    /// Set only when navigation should bring the selected card back on screen.
    pub reveal_selected_message: bool,

    pub input: String,
    /// Insertion point in `input`, as a character index.
    pub input_cursor: usize,

    /// The single source of truth for the files pane, sorted recent-first.
    pub note_files: Vec<NoteFile>,
    /// Absolute index into `note_files` (including while a filter is active).
    pub file_index: usize,
    /// Stable selection retained across file reloads and recent-first reordering.
    pub selected_file: Option<PathBuf>,
    pub file_row: usize,
    pub notes_expanded: bool,
    pub archives_expanded: bool,
    pub file_query: String,
    pub rename_input: String,
    pub rename_cursor: usize,
    pub new_file_input: String,
    pub new_file_cursor: usize,

    /// Message being moved/created/deleted by a contextual interaction.
    pub pending_id: Option<String>,
    /// File awaiting rename or delete confirmation.
    pub pending_file: Option<PathBuf>,

    pub todo_items: Vec<TodoItem>,
    pub todo_index: usize,

    pub search_query: String,
    pub search_results: Vec<SearchHit>,
    pub search_index: usize,

    pub help_scroll: u16,
    pub status: String,
    pub animation_tick: u64,
    pub layout: LayoutSnapshot,

    /// Rebuilt every frame by the renderer.
    pub hitboxes: Vec<ButtonHitbox>,
    pub link_hitboxes: Vec<LinkHitbox>,
    pub file_hitboxes: Vec<FileHitbox>,
    pub file_group_hitboxes: Vec<FileGroupHitbox>,
    pub todo_hitboxes: Vec<TodoHitbox>,
    pub search_hitboxes: Vec<SearchHitbox>,
    pub wiki_link_hitboxes: Vec<WikiLinkHitbox>,
    pub dialog_hitboxes: Vec<DialogOptionHitbox>,

    /// The one modal state shared by all command-style dialogs.
    pub dialog: Option<DialogState>,
    /// Result of a caller-provided [`Overlay::Dialog`]. Business dialogs
    /// deliver their result directly to their existing subsystem channels.
    pub dialog_result: Option<DialogResult>,

    ai_events: Option<Receiver<AgentEvent>>,
    ai_approval_sender: Option<mpsc::Sender<ApprovalDecision>>,
    ai_user_sender: Option<mpsc::Sender<AskUserResponse>>,
    pub ai_running: bool,
    pub permission_mode: PermissionMode,
    permission_bypass: Arc<AtomicBool>,
    pub agent_prompt: String,
    pub agent_output: Vec<String>,
    pub agent_output_final: bool,
    pub agent_scroll: u16,
    pub ai_prompt_input: String,
    pub ai_prompt_cursor: usize,
    ai_source_id: Option<String>,
    pub approval_request: Option<ApprovalRequest>,
    pub approval_scroll: u16,
    pub ask_user_request: Option<AskUserRequest>,
    pub ask_user_input: String,
    pub ask_user_cursor: usize,
    pub ask_user_option: usize,
    pub notifications: NotificationService,
    pub wiki_link_target: Option<String>,
    pub wiki_link_candidates: Vec<WikiLinkCandidate>,
    pub wiki_link_index: usize,

    ai_cancel: Option<Arc<AtomicBool>>,

    undo_stack: Vec<UndoOp>,
}

impl App {
    pub fn new(storage: Storage) -> anyhow::Result<Self> {
        let messages = storage.load_messages()?;
        let selected = messages.len().saturating_sub(1);
        let mut note_files = storage.list_note_files()?;
        let first_note = note_files.first().map(|file| file.path.clone());
        note_files.extend(storage.list_archived_note_files()?);
        let file_row = usize::from(first_note.is_some());
        let todo_items = storage.load_todo_tasks();
        Ok(Self {
            storage,
            focus: Focus::Compose,
            center_view: CenterView::Chat,
            files_context: FilesContext::Browse,
            overlay: None,
            document: None,
            messages,
            selected,
            scroll: u16::MAX,
            reveal_selected_message: true,
            input: String::new(),
            input_cursor: 0,
            note_files,
            file_index: 0,
            selected_file: first_note,
            file_row,
            notes_expanded: true,
            archives_expanded: false,
            file_query: String::new(),
            rename_input: String::new(),
            rename_cursor: 0,
            new_file_input: String::new(),
            new_file_cursor: 0,
            pending_id: None,
            pending_file: None,
            todo_items,
            todo_index: 0,
            search_query: String::new(),
            search_results: Vec::new(),
            search_index: 0,
            help_scroll: 0,
            status: String::new(),
            animation_tick: 0,
            layout: LayoutSnapshot::default(),
            hitboxes: Vec::new(),
            link_hitboxes: Vec::new(),
            file_hitboxes: Vec::new(),
            file_group_hitboxes: Vec::new(),
            todo_hitboxes: Vec::new(),
            search_hitboxes: Vec::new(),
            wiki_link_hitboxes: Vec::new(),
            dialog_hitboxes: Vec::new(),
            dialog: None,
            dialog_result: None,
            ai_events: None,
            ai_approval_sender: None,
            ai_user_sender: None,
            ai_running: false,
            permission_mode: PermissionMode::Approve,
            permission_bypass: Arc::new(AtomicBool::new(false)),
            agent_prompt: String::new(),
            agent_output: Vec::new(),
            agent_output_final: false,
            agent_scroll: 0,
            ai_prompt_input: String::new(),
            ai_prompt_cursor: 0,
            ai_source_id: None,
            approval_request: None,
            approval_scroll: 0,
            ask_user_request: None,
            ask_user_input: String::new(),
            ask_user_cursor: 0,
            ask_user_option: 0,
            notifications: NotificationService::default(),
            wiki_link_target: None,
            wiki_link_candidates: Vec::new(),
            wiki_link_index: 0,
            ai_cancel: None,
            undo_stack: Vec::new(),
        })
    }

    pub fn reload(&mut self) {
        let selected_id = self.selected_id().map(str::to_owned);
        match self.storage.load_messages() {
            Ok(messages) => {
                self.messages = messages;
                self.selected = selected_id
                    .as_deref()
                    .and_then(|id| self.messages.iter().position(|message| message.id == id))
                    .unwrap_or_else(|| self.selected.min(self.messages.len().saturating_sub(1)));
            }
            Err(error) => self.set_status(format!("Reload error: {error}")),
        }
    }

    pub fn advance_animation(&mut self) {
        if self.ai_running {
            self.animation_tick = self.animation_tick.wrapping_add(1);
        }
    }

    pub fn reload_files(&mut self) {
        let selected = self.selected_file.clone();
        match self.combined_note_files() {
            Ok(files) => self.note_files = files,
            Err(error) => {
                self.set_status(format!("Reload error: {error}"));
                return;
            }
        }
        self.file_index = selected
            .as_ref()
            .and_then(|path| self.note_files.iter().position(|file| &file.path == path))
            .unwrap_or(0)
            .min(self.note_files.len().saturating_sub(1));
        self.sync_selected_file();
        self.ensure_visible_file_selection();
    }

    fn combined_note_files(&self) -> anyhow::Result<Vec<NoteFile>> {
        let mut files = self.storage.list_note_files()?;
        files.extend(self.storage.list_archived_note_files()?);
        Ok(files)
    }

    pub fn reload_todos(&mut self) {
        self.todo_items = self.storage.load_todo_tasks();
        self.todo_index = self.todo_index.min(self.todo_items.len().saturating_sub(1));
    }

    /// Reload everything that may have changed while `$EDITOR` was running.
    pub fn reload_workspace(&mut self) {
        self.reload();
        self.reload_files();
        self.reload_todos();
        if matches!(
            self.center_view,
            CenterView::Search | CenterView::DocumentSearch
        ) {
            self.recompute_search();
        }
        let document_path = self.document.as_ref().and_then(|document| {
            if let DocumentKind::File(path) = &document.kind {
                Some(path.clone())
            } else {
                None
            }
        });
        if let Some(path) = document_path {
            match self.storage.read_document_file(&path) {
                Ok(updated) => {
                    if let Some(document) = self.document.as_mut() {
                        document.source = updated;
                    }
                }
                Err(_) if self.ai_running && !path.exists() => {
                    // The watcher can observe a move before the Agent event
                    // channel reports its destination. Keep the page open
                    // until that mapping arrives or the task finishes.
                }
                Err(error) => {
                    self.document = None;
                    self.center_view = CenterView::Chat;
                    self.focus = Focus::Center;
                    self.set_status(format!("Reload error: {error}"));
                }
            }
        }
    }

    /// Collect background Agent events without blocking the TUI.
    pub fn poll_agent(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.ai_events {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for event in events {
            match event {
                AgentEvent::Progress(message) => {
                    self.agent_output.push(message.clone());
                    self.agent_output_final = false;
                    self.agent_scroll = u16::MAX;
                    self.set_status(message);
                }
                AgentEvent::Notification(message) => {
                    self.notifications.notify(message);
                    self.set_status("Agent sent a notification");
                }
                AgentEvent::FileMoved { from, to } => {
                    self.handle_agent_file_moved(&from, &to);
                }
                AgentEvent::Approval(request) => {
                    if self.permission_mode == PermissionMode::Bypass {
                        let _ = self.send_approval(ApprovalDecision::Approve);
                    } else {
                        self.set_status(format!("Approval required: {}", request.title));
                        self.approval_request = Some(request);
                        self.approval_scroll = 0;
                        self.set_overlay(Overlay::Approval);
                    }
                }
                AgentEvent::AskUser(request) => {
                    self.set_status("Agent is waiting for your answer");
                    self.ask_user_option = 0;
                    self.ask_user_input.clear();
                    self.ask_user_cursor = 0;
                    self.ask_user_request = Some(request);
                    self.set_overlay(Overlay::AskUser);
                }
                AgentEvent::Finished(result) => {
                    self.ai_running = false;
                    self.ai_cancel = None;
                    match result {
                        Ok(output) => {
                            self.agent_output.clear();
                            self.agent_output_final = true;
                            self.agent_scroll = 0;
                            if !output.trim().is_empty() {
                                self.agent_output.push(output);
                            }
                            self.set_status("Agent finished");
                        }
                        Err(error) => {
                            self.agent_output.clear();
                            self.agent_output_final = false;
                            self.agent_scroll = 0;
                            self.set_status(format!("AI error: {error}"));
                        }
                    }
                    self.clear_ask_user();
                    self.reload_workspace();
                }
            }
        }
        if disconnected && self.ai_running {
            self.ai_running = false;
            self.ai_cancel = None;
            self.agent_output.clear();
            self.agent_output_final = false;
            self.agent_scroll = 0;
            self.clear_ask_user();
            self.set_status("AI error: worker stopped unexpectedly");
        }
        if disconnected && !self.ai_running {
            self.ai_events = None;
            self.ai_approval_sender = None;
            self.ai_user_sender = None;
        }
    }

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

    pub fn selected_id(&self) -> Option<&str> {
        self.messages
            .get(self.selected)
            .map(|message| message.id.as_str())
    }

    pub fn open_files(&mut self) {
        self.reload_files();
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

    fn open_document_search(&mut self) {
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

    /// Open a caller-defined command dialog. The caller can inspect the
    /// resulting value with [`App::take_dialog_result`] after the dialog
    /// closes.
    #[allow(dead_code)]
    pub fn open_dialog(&mut self, dialog: DialogState) {
        self.dialog_result = None;
        self.dialog = Some(dialog);
        self.overlay = Some(Overlay::Dialog);
    }

    #[allow(dead_code)]
    pub fn take_dialog_result(&mut self) -> Option<DialogResult> {
        self.dialog_result.take()
    }

    fn set_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        self.dialog = Some(self.dialog_for_overlay(overlay));
    }

    fn open_file_name_dialog(&mut self, purpose: DialogPurpose) {
        let (title, input, cursor) = match purpose {
            DialogPurpose::NewFile => (
                "New file · Enter create",
                self.new_file_input.clone(),
                self.new_file_cursor,
            ),
            DialogPurpose::RenameFile => (
                "Rename file · Enter save",
                self.rename_input.clone(),
                self.rename_cursor,
            ),
            _ => return,
        };
        let mut dialog =
            DialogState::new(title, "Name  ", DialogMode::FreeText, purpose, Vec::new());
        dialog.input = input;
        dialog.cursor = cursor;
        self.open_dialog(dialog);
    }

    pub(crate) fn ensure_file_input_dialog(&mut self) {
        let purpose = match self.files_context {
            FilesContext::NewTarget => Some(DialogPurpose::NewFile),
            FilesContext::Rename => Some(DialogPurpose::RenameFile),
            _ => None,
        };
        match purpose {
            Some(purpose) => {
                let needs_open = self.overlay != Some(Overlay::Dialog)
                    || self
                        .dialog
                        .as_ref()
                        .is_none_or(|dialog| dialog.purpose != purpose);
                if needs_open {
                    self.open_file_name_dialog(purpose);
                } else {
                    let current = self.dialog.as_ref().map(|dialog| dialog.input.as_str());
                    let expected = match purpose {
                        DialogPurpose::NewFile => self.new_file_input.as_str(),
                        DialogPurpose::RenameFile => self.rename_input.as_str(),
                        _ => "",
                    };
                    if current != Some(expected) {
                        self.open_file_name_dialog(purpose);
                    }
                }
            }
            None => {
                if self.dialog.as_ref().is_some_and(|dialog| {
                    matches!(
                        dialog.purpose,
                        DialogPurpose::NewFile | DialogPurpose::RenameFile
                    )
                }) {
                    self.overlay = None;
                    self.dialog = None;
                }
            }
        }
    }

    fn dialog_for_overlay(&self, overlay: Overlay) -> DialogState {
        match overlay {
            Overlay::ConfirmDeleteMessage => DialogState::new(
                "Delete message",
                "Delete this message?",
                DialogMode::Confirm,
                DialogPurpose::DeleteMessage,
                Vec::new(),
            ),
            Overlay::ConfirmDeleteFile => {
                let name = self
                    .pending_file
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "this file".to_string());
                DialogState::new(
                    "Delete file",
                    format!("Delete {name}?"),
                    DialogMode::Confirm,
                    DialogPurpose::DeleteFile,
                    Vec::new(),
                )
            }
            Overlay::Help => DialogState::new(
                "Help",
                String::new(),
                DialogMode::Informational,
                DialogPurpose::Help,
                Vec::new(),
            ),
            Overlay::AiPrompt => {
                let mut dialog = DialogState::new(
                    "Agent prompt",
                    "",
                    DialogMode::FreeText,
                    DialogPurpose::AgentPrompt,
                    Vec::new(),
                );
                dialog.input = self.ai_prompt_input.clone();
                dialog.cursor = self.ai_prompt_cursor;
                dialog
            }
            Overlay::Approval => {
                let request = self.approval_request.as_ref();
                let mut dialog = DialogState::new(
                    request
                        .map(|request| request.title.clone())
                        .unwrap_or_else(|| "Approve change".to_string()),
                    request
                        .map(|request| request.diff.clone())
                        .unwrap_or_default(),
                    DialogMode::Approval,
                    DialogPurpose::AgentApproval,
                    Vec::new(),
                );
                dialog.scroll = self.approval_scroll;
                dialog
            }
            Overlay::AskUser => {
                let request = self.ask_user_request.as_ref();
                let mut dialog = DialogState::new(
                    "Agent question",
                    request
                        .map(|request| request.question.clone())
                        .unwrap_or_default(),
                    DialogMode::SelectOrInput,
                    DialogPurpose::AskUser,
                    request
                        .map(|request| {
                            request
                                .options
                                .iter()
                                .cloned()
                                .map(DialogOption::new)
                                .collect()
                        })
                        .unwrap_or_default(),
                );
                dialog.selected = self.ask_user_option;
                dialog.input = self.ask_user_input.clone();
                dialog.cursor = self.ask_user_cursor;
                dialog
            }
            Overlay::WikiLinkChoice => {
                let target = self.wiki_link_target.as_deref().unwrap_or("wikilink");
                let options = self
                    .wiki_link_candidates
                    .iter()
                    .map(|candidate| {
                        let filename = candidate
                            .path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "(unnamed)".to_string());
                        let extension = candidate
                            .path
                            .extension()
                            .map(|extension| extension.to_string_lossy().to_ascii_uppercase())
                            .unwrap_or_else(|| "?".to_string());
                        let hint = if candidate.archived {
                            format!("Archived · {extension}")
                        } else {
                            extension
                        };
                        DialogOption::with_hint(filename, hint)
                    })
                    .collect();
                let mut dialog = DialogState::new(
                    format!("Choose wikilink · [[{target}]]"),
                    String::new(),
                    DialogMode::SingleSelect,
                    DialogPurpose::WikiLinkChoice,
                    options,
                );
                dialog.selected = self.wiki_link_index;
                dialog
            }
            Overlay::Dialog => match self.dialog.as_ref().map(|dialog| dialog.purpose) {
                Some(DialogPurpose::NewFile) => {
                    let mut dialog = DialogState::new(
                        "New file · Enter create",
                        "Name  ",
                        DialogMode::FreeText,
                        DialogPurpose::NewFile,
                        Vec::new(),
                    );
                    dialog.input = self.new_file_input.clone();
                    dialog.cursor = self.new_file_cursor;
                    dialog
                }
                Some(DialogPurpose::RenameFile) => {
                    let mut dialog = DialogState::new(
                        "Rename file · Enter save",
                        "Name  ",
                        DialogMode::FreeText,
                        DialogPurpose::RenameFile,
                        Vec::new(),
                    );
                    dialog.input = self.rename_input.clone();
                    dialog.cursor = self.rename_cursor;
                    dialog
                }
                _ => self.dialog.clone().unwrap_or_else(|| {
                    DialogState::new(
                        "Dialog",
                        String::new(),
                        DialogMode::Informational,
                        DialogPurpose::Custom,
                        Vec::new(),
                    )
                }),
            },
        }
    }

    /// Ensure dialogs opened by older call sites or tests are represented by
    /// the shared state. Normal application paths call `set_overlay`, while
    /// this fallback keeps direct overlay assignment deterministic.
    pub fn ensure_dialog_state(&mut self) {
        let Some(overlay) = self.overlay else {
            self.dialog = None;
            return;
        };
        let purpose = match overlay {
            Overlay::ConfirmDeleteMessage => DialogPurpose::DeleteMessage,
            Overlay::ConfirmDeleteFile => DialogPurpose::DeleteFile,
            Overlay::Help => DialogPurpose::Help,
            Overlay::AiPrompt => DialogPurpose::AgentPrompt,
            Overlay::Approval => DialogPurpose::AgentApproval,
            Overlay::AskUser => DialogPurpose::AskUser,
            Overlay::WikiLinkChoice => DialogPurpose::WikiLinkChoice,
            Overlay::Dialog => self
                .dialog
                .as_ref()
                .map_or(DialogPurpose::Custom, |dialog| dialog.purpose),
        };
        let stale =
            self.dialog.as_ref().is_none_or(|dialog| {
                if dialog.purpose != purpose {
                    return true;
                }
                match purpose {
                    DialogPurpose::AgentPrompt => dialog.input != self.ai_prompt_input,
                    DialogPurpose::NewFile => dialog.input != self.new_file_input,
                    DialogPurpose::RenameFile => dialog.input != self.rename_input,
                    DialogPurpose::AskUser => {
                        dialog.input != self.ask_user_input
                            || self.ask_user_request.as_ref().is_some_and(|request| {
                                dialog.options.len() != request.options.len()
                                    || dialog
                                        .options
                                        .iter()
                                        .zip(request.options.iter())
                                        .any(|(option, expected)| option.label != *expected)
                            })
                    }
                    DialogPurpose::AgentApproval => {
                        self.approval_request.as_ref().is_some_and(|request| {
                            dialog.message != request.diff || dialog.title != request.title
                        })
                    }
                    DialogPurpose::WikiLinkChoice => {
                        dialog.options.len() != self.wiki_link_candidates.len()
                            || self.wiki_link_candidates.iter().enumerate().any(
                                |(index, candidate)| {
                                    let expected = candidate
                                        .path
                                        .file_name()
                                        .map(|name| name.to_string_lossy().into_owned())
                                        .unwrap_or_default();
                                    dialog
                                        .options
                                        .get(index)
                                        .is_none_or(|option| option.label != expected)
                                },
                            )
                    }
                    _ => false,
                }
            });
        if stale {
            self.dialog = Some(self.dialog_for_overlay(overlay));
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Command> {
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
                KeyCode::Char('T') => {
                    self.open_todo();
                    return None;
                }
                _ => {}
            }
        }

        match self.focus {
            Focus::Compose => self.handle_compose(key),
            Focus::Files => self.handle_files(key),
            Focus::Todo => self.handle_todo(key),
            Focus::Agent => self.handle_agent(key),
            Focus::Center => match self.center_view {
                CenterView::Chat => self.handle_chat(key),
                CenterView::Document => self.handle_document(key),
                CenterView::Search | CenterView::DocumentSearch => self.handle_search(key),
            },
        }
    }

    /// Paste into whichever orthogonal state currently owns a text buffer.
    pub fn handle_paste(&mut self, text: &str) {
        if self.overlay.is_some() {
            self.ensure_dialog_state();
            let purpose = self.dialog.as_ref().map(|dialog| dialog.purpose);
            if matches!(
                purpose,
                Some(
                    DialogPurpose::AgentPrompt
                        | DialogPurpose::AskUser
                        | DialogPurpose::NewFile
                        | DialogPurpose::RenameFile
                )
            ) {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                if matches!(purpose, Some(DialogPurpose::AskUser)) {
                    self.select_custom_dialog_option();
                }
                if let Some(dialog) = self.dialog.as_mut() {
                    paste_into(&mut dialog.input, &mut dialog.cursor, &text);
                }
                self.sync_legacy_from_dialog();
            }
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match (self.focus, self.center_view, self.files_context) {
            (Focus::Compose, CenterView::Chat | CenterView::Document, _) => {
                paste_into(&mut self.input, &mut self.input_cursor, &text)
            }
            (Focus::Center, CenterView::Search | CenterView::DocumentSearch, _) => {
                self.search_query.push_str(&text);
                self.recompute_search();
            }
            (Focus::Files, _, FilesContext::Search) => {
                self.file_query.push_str(&text.replace('\n', ""));
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
        self.route_wheel(column, row, delta);
    }

    fn is_text_entry(&self) -> bool {
        self.focus == Focus::Compose
            || (self.focus == Focus::Center
                && matches!(
                    self.center_view,
                    CenterView::Search | CenterView::DocumentSearch
                ))
            || (self.focus == Focus::Files
                && matches!(
                    self.files_context,
                    FilesContext::Search | FilesContext::NewTarget | FilesContext::Rename
                ))
    }

    fn handle_compose(&mut self, key: KeyEvent) -> Option<Command> {
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
                self.send_message();
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

    fn move_input_cursor(&mut self, movement: CursorMove) -> Option<Command> {
        self.input_cursor = move_cursor(&self.input, self.input_cursor, movement);
        None
    }

    fn activate_link(&mut self, target: LinkTarget) -> Option<Command> {
        match target {
            LinkTarget::External(target) => Some(Command::OpenLink(target)),
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
                            self.open_file_document(&path, DocumentReturn::Chat);
                            self.set_status(format!("Created note {}", path.display()));
                        }
                        Err(error) => self.set_status(format!("Wiki note error: {error}")),
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

    fn open_wiki_candidate(&mut self, candidate: &WikiLinkCandidate) {
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
                self.document = Some(Document {
                    kind: DocumentKind::File(candidate.path.clone()),
                    title,
                    source,
                    scroll: 0,
                    target_line: None,
                    return_to: DocumentReturn::Chat,
                });
                self.center_view = CenterView::Document;
                self.focus = Focus::Center;
                self.overlay = None;
                self.dialog = None;
                self.wiki_link_target = None;
                self.wiki_link_candidates.clear();
                self.wiki_link_index = 0;
            }
            Err(error) => self.set_status(format!("Wiki note error: {error}")),
        }
    }

    #[allow(dead_code)]
    fn handle_wiki_link_choice(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = None;
                self.wiki_link_target = None;
                self.wiki_link_candidates.clear();
                self.wiki_link_index = 0;
            }
            KeyCode::Up => {
                self.wiki_link_index = self.wiki_link_index.saturating_sub(1);
            }
            KeyCode::Down => {
                self.wiki_link_index = (self.wiki_link_index + 1)
                    .min(self.wiki_link_candidates.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                if let Some(candidate) =
                    self.wiki_link_candidates.get(self.wiki_link_index).cloned()
                {
                    self.open_wiki_candidate(&candidate);
                }
            }
            _ => {}
        }
        None
    }

    fn handle_chat(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(Command::Quit),
            KeyCode::Tab | KeyCode::Char('i') | KeyCode::Enter => {
                self.focus = Focus::Compose;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_message_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_message_selection(-1);
                None
            }
            KeyCode::Left => {
                self.open_files();
                None
            }
            KeyCode::Right => {
                self.open_todo();
                None
            }
            KeyCode::Char('g') => {
                self.selected = 0;
                self.reveal_selected_message = true;
                None
            }
            KeyCode::Char('G') => {
                self.selected = self.messages.len().saturating_sub(1);
                self.reveal_selected_message = true;
                None
            }
            KeyCode::PageDown => {
                self.reveal_selected_message = false;
                self.scroll = self.scroll.saturating_add(5);
                None
            }
            KeyCode::PageUp => {
                self.reveal_selected_message = false;
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

    fn handle_files(&mut self, key: KeyEvent) -> Option<Command> {
        match self.files_context {
            FilesContext::Browse => self.handle_file_browse(key),
            FilesContext::Search => self.handle_file_search(key),
            FilesContext::MoveTarget => self.handle_move_target(key),
            FilesContext::NewTarget => self.handle_new_target(key),
            FilesContext::Rename => self.handle_rename(key),
        }
    }

    fn handle_file_browse(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q') => {
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
            KeyCode::Right => {
                if let Some(group) = self.selected_file_group() {
                    let expanded = match group {
                        FileGroup::Notes => &mut self.notes_expanded,
                        FileGroup::Archives => &mut self.archives_expanded,
                    };
                    if *expanded {
                        self.focus = Focus::Center;
                    } else {
                        *expanded = true;
                    }
                } else {
                    self.focus = Focus::Center;
                }
                None
            }
            KeyCode::Left => {
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
                    self.open_selected_file(DocumentReturn::Chat);
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

    fn handle_file_search(&mut self, key: KeyEvent) -> Option<Command> {
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
                self.open_selected_file(DocumentReturn::Chat);
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

    fn handle_move_target(&mut self, key: KeyEvent) -> Option<Command> {
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

    fn handle_new_target(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.cancel_file_context();
                None
            }
            KeyCode::Enter => {
                let name = self.new_file_input.clone();
                let Some(id) = self.pending_id.clone() else {
                    self.cancel_file_context();
                    return None;
                };
                match self.storage.create_named_file(&name) {
                    Ok(path) => self.perform_move_to_id(&path, &id),
                    Err(error) => self.set_status(format!("Error: {error}")),
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

    fn handle_rename(&mut self, key: KeyEvent) -> Option<Command> {
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
                        Err(error) => self.set_status(format!("Error: {error}")),
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

    fn retarget_open_document(&mut self, from: &Path, to: &Path) -> bool {
        let Some(document) = self.document.as_mut() else {
            return false;
        };
        if !matches!(&document.kind, DocumentKind::File(path) if path == from) {
            return false;
        }
        document.kind = DocumentKind::File(to.to_path_buf());
        document.title = to
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Document".to_string());
        true
    }

    fn handle_agent_file_moved(&mut self, from: &Path, to: &Path) {
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

    fn resolve_agent_event_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.storage.root.join(path)
        }
    }

    fn handle_todo(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.focus = Focus::Center;
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let visible = self.visible_todo_indices();
                let at_end = visible.is_empty()
                    || visible
                        .last()
                        .is_some_and(|index| *index == self.todo_index);
                if at_end && (!self.agent_prompt.is_empty() || !self.agent_output.is_empty()) {
                    self.focus = Focus::Agent;
                } else {
                    self.move_todo_selection(1);
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_todo_selection(-1);
                None
            }
            KeyCode::Left => {
                self.focus = Focus::Center;
                None
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('x') => {
                self.toggle_todo(self.todo_index);
                None
            }
            _ => None,
        }
    }

    fn handle_agent(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left => {
                self.focus = Focus::Center;
                None
            }
            KeyCode::Up | KeyCode::Char('k') if self.agent_scroll == 0 => {
                self.focus = Focus::Todo;
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
            KeyCode::Enter => {
                self.add_agent_output_to_chat();
                None
            }
            KeyCode::Char('c') if self.ai_running => {
                self.cancel_agent();
                None
            }
            _ => None,
        }
    }

    fn add_agent_output_to_chat(&mut self) {
        if self.ai_running || !self.agent_output_final {
            self.set_status("Agent has no final response to add");
            return;
        }
        let body = self.agent_output.join("\n\n");
        if body.trim().is_empty() {
            self.set_status("Agent has no output to add");
            return;
        }
        match self.storage.append_chat_message(&body) {
            Ok(message) => {
                self.agent_prompt.clear();
                self.agent_output.clear();
                self.agent_output_final = false;
                self.agent_scroll = 0;
                self.reload();
                self.reload_todos();
                if let Some(index) = self
                    .messages
                    .iter()
                    .position(|candidate| candidate.id == message.id)
                {
                    self.selected = index;
                }
                if self.center_view == CenterView::Chat {
                    self.scroll = u16::MAX;
                }
                self.focus = Focus::Center;
                self.set_status("Agent output added to today's daily note");
            }
            Err(error) => self.set_status(format!("Error: {error}")),
        }
    }

    fn handle_search(&mut self, key: KeyEvent) -> Option<Command> {
        let document_search = self.center_view == CenterView::DocumentSearch;
        match key.code {
            KeyCode::Esc => {
                self.center_view = if document_search && self.document.is_some() {
                    CenterView::Document
                } else {
                    CenterView::Chat
                };
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
            KeyCode::Backspace => {
                self.search_query.pop();
                self.recompute_search();
                None
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_query.push(character);
                self.recompute_search();
                None
            }
            _ => None,
        }
    }

    fn handle_document(&mut self, key: KeyEvent) -> Option<Command> {
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
            KeyCode::Left => {
                self.open_files();
                None
            }
            KeyCode::Right => {
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
                Some(DocumentKind::File(path)) => Some(Command::Edit(path.clone())),
                Some(DocumentKind::Message(id)) => self.message_edit_command(id),
                None => None,
            },
            KeyCode::Char('/') => {
                self.open_document_search();
                None
            }
            _ => None,
        }
    }

    fn handle_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        self.ensure_dialog_state();
        self.handle_dialog_key(key)
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) -> Option<Command> {
        let Some(dialog) = self.dialog.clone() else {
            self.overlay = None;
            return None;
        };
        match dialog.purpose {
            DialogPurpose::DeleteMessage => {
                return match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.handle_delete_message_overlay(key)
                    }
                    _ => self.handle_delete_message_overlay(key),
                };
            }
            DialogPurpose::DeleteFile => return self.handle_delete_file_overlay(key),
            DialogPurpose::Help => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                        self.overlay = None;
                        self.dialog = None;
                    }
                    KeyCode::Down | KeyCode::Char('j') => self.adjust_dialog_scroll(1),
                    KeyCode::Up | KeyCode::Char('k') => self.adjust_dialog_scroll(-1),
                    KeyCode::PageDown => self.adjust_dialog_scroll(8),
                    KeyCode::PageUp => self.adjust_dialog_scroll(-8),
                    _ => {}
                }
                return None;
            }
            DialogPurpose::AgentApproval => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        let _ = self.send_approval(ApprovalDecision::Approve);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        let _ = self.send_approval(ApprovalDecision::Deny);
                    }
                    KeyCode::Down | KeyCode::Char('j') => self.adjust_dialog_scroll(1),
                    KeyCode::Up | KeyCode::Char('k') => self.adjust_dialog_scroll(-1),
                    KeyCode::PageDown => self.adjust_dialog_scroll(8),
                    KeyCode::PageUp => self.adjust_dialog_scroll(-8),
                    _ => {}
                }
                return None;
            }
            DialogPurpose::WikiLinkChoice => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = None;
                        self.dialog = None;
                        self.wiki_link_target = None;
                        self.wiki_link_candidates.clear();
                        self.wiki_link_index = 0;
                    }
                    KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
                    KeyCode::Enter => {
                        if let Some(candidate) = self
                            .wiki_link_candidates
                            .get(self.dialog_selected())
                            .cloned()
                        {
                            self.open_wiki_candidate(&candidate);
                        }
                    }
                    _ => {}
                }
                return None;
            }
            DialogPurpose::AskUser => return self.handle_select_or_input_dialog(key),
            DialogPurpose::AgentPrompt | DialogPurpose::NewFile | DialogPurpose::RenameFile => {
                return self.handle_text_dialog(key)
            }
            DialogPurpose::Custom => {}
        }

        match dialog.mode {
            DialogMode::Confirm => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.dialog_result = Some(DialogResult::Confirm(true));
                    self.close_dialog();
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.dialog_result = Some(DialogResult::Confirm(false));
                    self.close_dialog();
                }
                _ => {}
            },
            DialogMode::SingleSelect => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
                KeyCode::Enter => {
                    if let Some(option) =
                        self.dialog.as_ref().and_then(DialogState::selected_option)
                    {
                        self.dialog_result = Some(DialogResult::Selected(option.label.clone()));
                    }
                    self.close_dialog();
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.dialog_result = Some(DialogResult::Cancelled);
                    self.close_dialog();
                }
                _ => {}
            },
            DialogMode::MultiSelect => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
                KeyCode::Char(' ') => self.toggle_dialog_option(),
                KeyCode::Enter => {
                    let selected = self
                        .dialog
                        .as_ref()
                        .map(DialogState::selected_options)
                        .unwrap_or_default();
                    self.dialog_result = Some(DialogResult::SelectedMany(selected));
                    self.close_dialog();
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.dialog_result = Some(DialogResult::Cancelled);
                    self.close_dialog();
                }
                _ => {}
            },
            DialogMode::SelectOrInput => return self.handle_custom_select_or_input(key),
            DialogMode::FreeText => return self.handle_text_dialog(key),
            DialogMode::Approval | DialogMode::Informational => {}
        }
        None
    }

    fn handle_custom_select_or_input(&mut self, key: KeyEvent) -> Option<Command> {
        let option_count = self
            .dialog
            .as_ref()
            .map_or(0, |dialog| dialog.options.len());
        let custom_selected = self.dialog_selected() >= option_count;
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Esc => {
                self.dialog_result = Some(DialogResult::Cancelled);
                self.close_dialog();
            }
            KeyCode::Up if option_count > 0 => self.move_dialog_selection(-1),
            KeyCode::Down if option_count > 0 => {
                let next = (self.dialog_selected() + 1).min(option_count);
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.selected = next;
                }
            }
            KeyCode::Enter
                if custom_selected
                    && modifiers.intersects(
                        KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
                    ) =>
            {
                self.insert_dialog_char('\n');
            }
            KeyCode::Enter => {
                let result = if custom_selected {
                    DialogResult::Text(
                        self.dialog
                            .as_ref()
                            .map(|dialog| dialog.input.trim().to_string())
                            .unwrap_or_default(),
                    )
                } else {
                    DialogResult::Selected(
                        self.dialog
                            .as_ref()
                            .and_then(DialogState::selected_option)
                            .map(|option| option.label.clone())
                            .unwrap_or_default(),
                    )
                };
                self.dialog_result = Some(result);
                self.close_dialog();
            }
            KeyCode::Backspace => {
                self.select_custom_dialog_option();
                self.delete_dialog_backward();
            }
            KeyCode::Delete => {
                self.select_custom_dialog_option();
                self.delete_dialog_forward();
            }
            KeyCode::Left => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::Left);
            }
            KeyCode::Right => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::Right);
            }
            KeyCode::Home => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::LineStart);
            }
            KeyCode::End => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::LineEnd);
            }
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_dialog_option();
                self.insert_dialog_char('\n');
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_dialog_option();
                self.insert_dialog_char(character);
            }
            _ => {}
        }
        None
    }

    fn dialog_selected(&self) -> usize {
        self.dialog.as_ref().map_or(0, |dialog| dialog.selected)
    }

    fn move_dialog_selection(&mut self, delta: i32) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        let max = dialog.options.len().saturating_sub(1);
        dialog.selected = if delta < 0 {
            dialog
                .selected
                .saturating_sub(delta.unsigned_abs() as usize)
        } else {
            dialog.selected.saturating_add(delta as usize).min(max)
        };
        if dialog.purpose == DialogPurpose::AskUser {
            self.ask_user_option = dialog.selected;
        } else if dialog.purpose == DialogPurpose::WikiLinkChoice {
            self.wiki_link_index = dialog.selected;
        }
    }

    fn toggle_dialog_option(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        if let Some(checked) = dialog.checked.get_mut(dialog.selected) {
            *checked = !*checked;
        }
    }

    fn adjust_dialog_scroll(&mut self, delta: i32) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        dialog.scroll = if delta < 0 {
            dialog.scroll.saturating_sub(delta.unsigned_abs() as u16)
        } else {
            dialog.scroll.saturating_add(delta as u16)
        };
        match dialog.purpose {
            DialogPurpose::Help => self.help_scroll = dialog.scroll,
            DialogPurpose::AgentApproval => self.approval_scroll = dialog.scroll,
            _ => {}
        }
    }

    fn handle_select_or_input_dialog(&mut self, key: KeyEvent) -> Option<Command> {
        let option_count = self
            .dialog
            .as_ref()
            .map_or(0, |dialog| dialog.options.len());
        let custom_selected = self.dialog_selected() >= option_count;
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Esc => {
                let _ = self.send_user_response(AskUserResponse::Cancelled);
            }
            KeyCode::Up if option_count > 0 => self.move_dialog_selection(-1),
            KeyCode::Down if option_count > 0 => {
                let next = (self.dialog_selected() + 1).min(option_count);
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.selected = next;
                }
                self.ask_user_option = next;
            }
            KeyCode::Enter
                if custom_selected
                    && modifiers.intersects(
                        KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
                    ) =>
            {
                self.insert_dialog_char('\n');
            }
            KeyCode::Enter => {
                let answer = if custom_selected {
                    self.dialog
                        .as_ref()
                        .map(|dialog| dialog.input.trim().to_string())
                        .unwrap_or_default()
                } else {
                    self.dialog
                        .as_ref()
                        .and_then(DialogState::selected_option)
                        .map(|option| option.label.clone())
                        .unwrap_or_default()
                };
                if answer.is_empty() {
                    self.set_status("Enter an answer before submitting");
                } else {
                    let _ = self.send_user_response(AskUserResponse::Answer(answer));
                }
            }
            KeyCode::Backspace => {
                self.select_custom_dialog_option();
                self.delete_dialog_backward();
            }
            KeyCode::Delete => {
                self.select_custom_dialog_option();
                self.delete_dialog_forward();
            }
            KeyCode::Left => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::Left);
            }
            KeyCode::Right => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::Right);
            }
            KeyCode::Home => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::LineStart);
            }
            KeyCode::End => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::LineEnd);
            }
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_dialog_option();
                self.insert_dialog_char('\n');
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_dialog_option();
                self.insert_dialog_char(character);
            }
            _ => {}
        }
        None
    }

    fn handle_text_dialog(&mut self, key: KeyEvent) -> Option<Command> {
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Enter
                if modifiers.intersects(
                    KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
                ) =>
            {
                self.insert_dialog_char('\n');
            }
            KeyCode::Enter => {
                self.sync_legacy_from_dialog();
                match self.dialog.as_ref().map(|dialog| dialog.purpose) {
                    Some(DialogPurpose::AgentPrompt) => self.submit_agent_prompt(),
                    Some(DialogPurpose::NewFile) => {
                        self.handle_new_target(key);
                        if self.files_context != FilesContext::NewTarget {
                            self.close_dialog();
                        }
                    }
                    Some(DialogPurpose::RenameFile) => {
                        self.handle_rename(key);
                        if self.files_context != FilesContext::Rename {
                            self.close_dialog();
                        }
                    }
                    _ => {
                        let text = self
                            .dialog
                            .as_ref()
                            .map(|dialog| dialog.input.clone())
                            .unwrap_or_default();
                        self.dialog_result = Some(DialogResult::Text(text));
                        self.close_dialog();
                    }
                }
            }
            KeyCode::Esc => {
                match self.dialog.as_ref().map(|dialog| dialog.purpose) {
                    Some(DialogPurpose::AgentPrompt) => self.ai_source_id = None,
                    Some(DialogPurpose::NewFile) => {
                        self.pending_id = None;
                        self.files_context = FilesContext::Browse;
                    }
                    Some(DialogPurpose::RenameFile) => {
                        self.pending_file = None;
                        self.files_context = FilesContext::Browse;
                    }
                    _ => self.dialog_result = Some(DialogResult::Cancelled),
                }
                self.close_dialog();
            }
            KeyCode::Backspace => self.delete_dialog_backward(),
            KeyCode::Delete => self.delete_dialog_forward(),
            KeyCode::Left => self.move_dialog_cursor(CursorMove::Left),
            KeyCode::Right => self.move_dialog_cursor(CursorMove::Right),
            KeyCode::Up => self.move_dialog_cursor(CursorMove::Up),
            KeyCode::Down => self.move_dialog_cursor(CursorMove::Down),
            KeyCode::Home => self.move_dialog_cursor(CursorMove::LineStart),
            KeyCode::End => self.move_dialog_cursor(CursorMove::LineEnd),
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_dialog_char('\n')
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_dialog_char(character)
            }
            _ => {}
        }
        None
    }

    fn insert_dialog_char(&mut self, character: char) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        insert_char(&mut dialog.input, &mut dialog.cursor, character);
        self.sync_legacy_from_dialog();
    }

    fn delete_dialog_backward(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        delete_backward(&mut dialog.input, &mut dialog.cursor);
        self.sync_legacy_from_dialog();
    }

    fn delete_dialog_forward(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        delete_forward(&mut dialog.input, &mut dialog.cursor);
        self.sync_legacy_from_dialog();
    }

    fn move_dialog_cursor(&mut self, movement: CursorMove) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        dialog.cursor = move_cursor(&dialog.input, dialog.cursor, movement);
        self.sync_legacy_from_dialog();
    }

    fn select_custom_dialog_option(&mut self) {
        let count = self
            .dialog
            .as_ref()
            .map_or(0, |dialog| dialog.options.len());
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.selected = count;
        }
        self.ask_user_option = count;
    }

    fn sync_legacy_from_dialog(&mut self) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };
        match dialog.purpose {
            DialogPurpose::AgentPrompt => {
                self.ai_prompt_input = dialog.input.clone();
                self.ai_prompt_cursor = dialog.cursor;
            }
            DialogPurpose::AskUser => {
                self.ask_user_input = dialog.input.clone();
                self.ask_user_cursor = dialog.cursor;
                self.ask_user_option = dialog.selected;
            }
            DialogPurpose::NewFile => {
                self.new_file_input = dialog.input.clone();
                self.new_file_cursor = dialog.cursor;
            }
            DialogPurpose::RenameFile => {
                self.rename_input = dialog.input.clone();
                self.rename_cursor = dialog.cursor;
            }
            DialogPurpose::AgentApproval => self.approval_scroll = dialog.scroll,
            DialogPurpose::Help => self.help_scroll = dialog.scroll,
            DialogPurpose::WikiLinkChoice => self.wiki_link_index = dialog.selected,
            _ => {}
        }
    }

    fn close_dialog(&mut self) {
        self.overlay = None;
        self.dialog = None;
    }

    fn handle_delete_message_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(id) = self.pending_id.take() {
                    let message = self.message_clone(&id);
                    match self.storage.remove_message_by_id(&id) {
                        Ok(true) => {
                            if let Some(message) = message {
                                self.record_undo(UndoOp::Delete(message));
                            }
                            self.set_status("Deleted");
                            self.reload();
                            self.reload_todos();
                        }
                        Ok(false) => self.set_status("Message not found"),
                        Err(error) => self.set_status(format!("Error: {error}")),
                    }
                }
                self.overlay = None;
                self.dialog = None;
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_id = None;
                self.overlay = None;
                self.dialog = None;
                None
            }
            _ => None,
        }
    }

    fn handle_delete_file_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(path) = self.pending_file.take() {
                    let archived = self
                        .note_files
                        .iter()
                        .find(|file| file.path == path)
                        .is_some_and(|file| file.archived);
                    let result = if archived {
                        self.storage.delete_archived_file(&path)
                    } else {
                        self.storage.delete_file(&path)
                    };
                    match result {
                        Ok(()) => {
                            self.set_status(format!(
                                "Deleted {}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            ));
                            if self
                                .document
                                .as_ref()
                                .is_some_and(|document| document.kind == DocumentKind::File(path))
                            {
                                self.document = None;
                                self.center_view = CenterView::Chat;
                            }
                            self.reload_files();
                        }
                        Err(error) => self.set_status(format!("Error: {error}")),
                    }
                }
                self.overlay = None;
                self.dialog = None;
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_file = None;
                self.overlay = None;
                self.dialog = None;
                None
            }
            _ => None,
        }
    }

    #[allow(dead_code)]
    fn handle_help_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                self.overlay = None;
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

    #[allow(dead_code)]
    fn handle_ai_prompt_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Enter
                if modifiers.intersects(
                    KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
                ) =>
            {
                insert_char(&mut self.ai_prompt_input, &mut self.ai_prompt_cursor, '\n');
                None
            }
            KeyCode::Enter => {
                self.submit_agent_prompt();
                None
            }
            KeyCode::Esc => {
                self.overlay = None;
                self.ai_source_id = None;
                None
            }
            KeyCode::Backspace => {
                delete_backward(&mut self.ai_prompt_input, &mut self.ai_prompt_cursor);
                None
            }
            KeyCode::Delete => {
                delete_forward(&mut self.ai_prompt_input, &mut self.ai_prompt_cursor);
                None
            }
            KeyCode::Left => {
                self.ai_prompt_cursor = move_cursor(
                    &self.ai_prompt_input,
                    self.ai_prompt_cursor,
                    CursorMove::Left,
                );
                None
            }
            KeyCode::Right => {
                self.ai_prompt_cursor = move_cursor(
                    &self.ai_prompt_input,
                    self.ai_prompt_cursor,
                    CursorMove::Right,
                );
                None
            }
            KeyCode::Up => {
                self.ai_prompt_cursor =
                    move_cursor(&self.ai_prompt_input, self.ai_prompt_cursor, CursorMove::Up);
                None
            }
            KeyCode::Down => {
                self.ai_prompt_cursor = move_cursor(
                    &self.ai_prompt_input,
                    self.ai_prompt_cursor,
                    CursorMove::Down,
                );
                None
            }
            KeyCode::Home => {
                self.ai_prompt_cursor = move_cursor(
                    &self.ai_prompt_input,
                    self.ai_prompt_cursor,
                    CursorMove::LineStart,
                );
                None
            }
            KeyCode::End => {
                self.ai_prompt_cursor = move_cursor(
                    &self.ai_prompt_input,
                    self.ai_prompt_cursor,
                    CursorMove::LineEnd,
                );
                None
            }
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                insert_char(&mut self.ai_prompt_input, &mut self.ai_prompt_cursor, '\n');
                None
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                insert_char(
                    &mut self.ai_prompt_input,
                    &mut self.ai_prompt_cursor,
                    character,
                );
                None
            }
            _ => None,
        }
    }

    #[allow(dead_code)]
    fn handle_approval_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let _ = self.send_approval(ApprovalDecision::Approve);
                None
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                let _ = self.send_approval(ApprovalDecision::Deny);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.approval_scroll = self.approval_scroll.saturating_add(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.approval_scroll = self.approval_scroll.saturating_sub(1);
                None
            }
            KeyCode::PageDown => {
                self.approval_scroll = self.approval_scroll.saturating_add(8);
                None
            }
            KeyCode::PageUp => {
                self.approval_scroll = self.approval_scroll.saturating_sub(8);
                None
            }
            _ => None,
        }
    }

    #[allow(dead_code)]
    fn handle_ask_user_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        let modifiers = key.modifiers;
        let option_count = self
            .ask_user_request
            .as_ref()
            .map_or(0, |request| request.options.len());
        match key.code {
            KeyCode::Esc => {
                let _ = self.send_user_response(AskUserResponse::Cancelled);
                None
            }
            KeyCode::Up if option_count > 0 => {
                self.ask_user_option = self.ask_user_option.saturating_sub(1);
                None
            }
            KeyCode::Down if option_count > 0 => {
                self.ask_user_option = (self.ask_user_option + 1).min(option_count);
                None
            }
            KeyCode::Enter
                if modifiers.intersects(
                    KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
                ) && self.ask_user_option == option_count =>
            {
                insert_char(&mut self.ask_user_input, &mut self.ask_user_cursor, '\n');
                None
            }
            KeyCode::Enter => {
                let answer = self
                    .ask_user_request
                    .as_ref()
                    .and_then(|request| request.options.get(self.ask_user_option))
                    .cloned()
                    .unwrap_or_else(|| self.ask_user_input.trim().to_string());
                if answer.is_empty() {
                    self.set_status("Enter an answer before submitting");
                } else {
                    let _ = self.send_user_response(AskUserResponse::Answer(answer));
                }
                None
            }
            KeyCode::Backspace => {
                self.select_custom_answer();
                delete_backward(&mut self.ask_user_input, &mut self.ask_user_cursor);
                None
            }
            KeyCode::Delete => {
                self.select_custom_answer();
                delete_forward(&mut self.ask_user_input, &mut self.ask_user_cursor);
                None
            }
            KeyCode::Left => {
                self.select_custom_answer();
                self.ask_user_cursor =
                    move_cursor(&self.ask_user_input, self.ask_user_cursor, CursorMove::Left);
                None
            }
            KeyCode::Right => {
                self.select_custom_answer();
                self.ask_user_cursor = move_cursor(
                    &self.ask_user_input,
                    self.ask_user_cursor,
                    CursorMove::Right,
                );
                None
            }
            KeyCode::Home => {
                self.select_custom_answer();
                self.ask_user_cursor = move_cursor(
                    &self.ask_user_input,
                    self.ask_user_cursor,
                    CursorMove::LineStart,
                );
                None
            }
            KeyCode::End => {
                self.select_custom_answer();
                self.ask_user_cursor = move_cursor(
                    &self.ask_user_input,
                    self.ask_user_cursor,
                    CursorMove::LineEnd,
                );
                None
            }
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_answer();
                insert_char(&mut self.ask_user_input, &mut self.ask_user_cursor, '\n');
                None
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_answer();
                insert_char(
                    &mut self.ask_user_input,
                    &mut self.ask_user_cursor,
                    character,
                );
                None
            }
            _ => None,
        }
    }

    #[allow(dead_code)]
    fn select_custom_answer(&mut self) {
        self.ask_user_option = self
            .ask_user_request
            .as_ref()
            .map_or(0, |request| request.options.len());
    }

    fn send_user_response(&mut self, response: AskUserResponse) -> anyhow::Result<()> {
        let sender = self
            .ai_user_sender
            .as_ref()
            .context("Agent user-response channel is unavailable")?;
        sender
            .send(response.clone())
            .context("sending response to Agent")?;
        self.set_status(match response {
            AskUserResponse::Answer(_) => "Answer sent to Agent",
            AskUserResponse::Cancelled => "Agent question cancelled",
        });
        self.clear_ask_user();
        Ok(())
    }

    fn clear_ask_user(&mut self) {
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

    fn send_approval(&mut self, decision: ApprovalDecision) -> anyhow::Result<()> {
        let sender = self
            .ai_approval_sender
            .as_ref()
            .context("Agent approval channel is unavailable")?;
        sender
            .send(decision)
            .context("sending Agent approval decision")?;
        self.set_status(match decision {
            ApprovalDecision::Approve => "Change approved",
            ApprovalDecision::Deny => "Change denied",
        });
        self.approval_request = None;
        if self.overlay == Some(Overlay::Approval) {
            self.overlay = None;
        }
        if self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.purpose == DialogPurpose::AgentApproval)
        {
            self.dialog = None;
        }
        Ok(())
    }

    fn toggle_permission_mode(&mut self) {
        self.permission_mode = self.permission_mode.toggled();
        self.permission_bypass.store(
            self.permission_mode == PermissionMode::Bypass,
            Ordering::Relaxed,
        );
        if self.permission_mode == PermissionMode::Bypass && self.overlay == Some(Overlay::Approval)
        {
            let _ = self.send_approval(ApprovalDecision::Approve);
        }
        self.set_status(format!("Permission mode: {}", self.permission_mode.label()));
    }

    fn route_wheel(&mut self, column: u16, row: u16, delta: i32) {
        if self.overlay.is_some() {
            if self.layout.overlay.is_none() || in_area(column, row, self.layout.overlay) {
                self.ensure_dialog_state();
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
        } else if in_area(column, row, self.layout.todo) {
            self.move_todo_selection(delta);
        } else if in_area(column, row, self.layout.agent) {
            self.agent_scroll = if delta > 0 {
                self.agent_scroll.saturating_add(delta as u16)
            } else {
                self.agent_scroll
                    .saturating_sub(delta.unsigned_abs() as u16)
            };
        } else if in_area(column, row, self.layout.center) {
            match self.center_view {
                CenterView::Chat => {
                    self.reveal_selected_message = false;
                    self.scroll = if delta > 0 {
                        self.scroll.saturating_add(delta as u16)
                    } else {
                        self.scroll.saturating_sub(delta.unsigned_abs() as u16)
                    };
                }
                CenterView::Document => self.scroll_document(delta),
                CenterView::Search | CenterView::DocumentSearch => {
                    self.move_search_selection(delta)
                }
            }
        }
    }

    fn handle_left_click(&mut self, column: u16, row: u16) -> Option<Command> {
        if self.overlay.is_some() {
            self.ensure_dialog_state();
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
                self.sync_legacy_from_dialog();
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
            self.focus = Focus::Todo;
            self.todo_index = index;
            self.toggle_todo(index);
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
                        self.open_selected_file(DocumentReturn::Chat)
                    }
                    FilesContext::MoveTarget => self.perform_move_to(&path),
                    FilesContext::NewTarget | FilesContext::Rename => {}
                }
            }
            return None;
        }

        if self.center_view == CenterView::Chat {
            if let Some((id, action)) = self
                .hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| (hitbox.message_id.clone(), hitbox.action))
            {
                return self.dispatch_action(&id, action);
            }
        }

        if in_area(column, row, self.layout.compose)
            && matches!(self.center_view, CenterView::Chat | CenterView::Document)
        {
            self.focus = Focus::Compose;
        } else if in_area(column, row, self.layout.files) {
            self.focus = Focus::Files;
        } else if in_area(column, row, self.layout.todo) {
            self.focus = Focus::Todo;
        } else if in_area(column, row, self.layout.agent) {
            self.focus = Focus::Agent;
        } else if in_area(column, row, self.layout.center) {
            self.focus = Focus::Center;
        }
        None
    }

    fn move_message_selection(&mut self, delta: i32) {
        if !self.messages.is_empty() {
            let selected = (self.selected as i32 + delta)
                .clamp(0, self.messages.len().saturating_sub(1) as i32)
                as usize;
            if selected != self.selected {
                self.selected = selected;
                self.reveal_selected_message = true;
            }
        }
    }

    fn move_file_selection(&mut self, delta: i32) {
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

    fn ensure_visible_file_selection(&mut self) {
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

    fn sync_selected_file(&mut self) {
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

    fn select_file_row(&mut self, row: usize) {
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

    fn selected_file_group(&self) -> Option<FileGroup> {
        self.visible_file_rows()
            .get(self.file_row)
            .and_then(|row| match row {
                FileListRow::Group(group) => Some(*group),
                FileListRow::File(_) => None,
            })
    }

    fn toggle_file_group(&mut self, group: FileGroup) {
        match group {
            FileGroup::Notes => self.notes_expanded = !self.notes_expanded,
            FileGroup::Archives => self.archives_expanded = !self.archives_expanded,
        }
        self.ensure_visible_file_selection();
    }

    fn move_todo_selection(&mut self, delta: i32) {
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

    fn move_search_selection(&mut self, delta: i32) {
        if !self.search_results.is_empty() {
            self.search_index = (self.search_index as i32 + delta)
                .clamp(0, self.search_results.len().saturating_sub(1) as i32)
                as usize;
        }
    }

    fn scroll_document(&mut self, delta: i32) {
        if let Some(document) = self.document.as_mut() {
            document.scroll = if delta > 0 {
                document.scroll.saturating_add(delta as u16)
            } else {
                document.scroll.saturating_sub(delta.unsigned_abs() as u16)
            };
        }
    }

    fn toggle_todo(&mut self, index: usize) {
        match self.storage.toggle_todo_task(index) {
            Ok(true) => {
                self.reload_todos();
                self.reload_files();
            }
            Ok(false) => self.set_status("No such task"),
            Err(error) => self.set_status(format!("Error: {error}")),
        }
    }

    fn recompute_search(&mut self) {
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
            for message in &self.messages {
                if message.body.to_lowercase().contains(&query) {
                    results.push(SearchHit::Message {
                        id: message.id.clone(),
                        text: best_line(&message.body, &query),
                    });
                }
            }
            results.extend(self.storage.search_file_lines(&query));
        }
        self.search_results = results;
        self.search_index = self
            .search_index
            .min(self.search_results.len().saturating_sub(1));
    }

    fn jump_to_search_result(&mut self, index: usize) {
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
            SearchHit::Message { id, .. } => {
                self.open_message_document(&id, DocumentReturn::Search)
            }
            SearchHit::FileLine { path, line_no, .. } => {
                self.open_file_document(&path, DocumentReturn::Search);
                if let Some(document) = self.document.as_mut() {
                    document.target_line = Some(line_no);
                }
            }
            SearchHit::DocumentLine { .. } => {}
        }
    }

    fn open_selected_file(&mut self, return_to: DocumentReturn) {
        if let Some(path) = self.selected_file.clone() {
            self.open_file_document(&path, return_to);
        }
    }

    fn archive_selected_note(&mut self) {
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
            Err(error) => self.set_status(format!("Error: {error}")),
        }
    }

    fn restore_selected_note(&mut self) {
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
                    if self
                        .document
                        .as_ref()
                        .is_some_and(|document| document.kind == DocumentKind::File(path.clone()))
                    {
                        self.document = None;
                        self.center_view = CenterView::Chat;
                        self.focus = Focus::Center;
                    }
                    self.reload_workspace();
                    if let Some(index) = self.messages.iter().position(|message| message.id == date)
                    {
                        self.selected = index;
                        self.reveal_selected_message = true;
                    }
                    self.set_status("Daily note restored");
                }
                Err(error) => self.set_status(format!("Error: {error}")),
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
            Err(error) => self.set_status(format!("Error: {error}")),
        }
    }

    fn open_file_document(&mut self, path: &Path, return_to: DocumentReturn) {
        match self.storage.read_document_file(path) {
            Ok(source) => {
                let title = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Document".to_string());
                self.document = Some(Document {
                    kind: DocumentKind::File(path.to_path_buf()),
                    title,
                    source,
                    scroll: 0,
                    target_line: None,
                    return_to,
                });
                self.center_view = CenterView::Document;
                self.focus = Focus::Center;
            }
            Err(error) => self.set_status(format!("Error: {error}")),
        }
    }

    fn open_message_document(&mut self, id: &str, return_to: DocumentReturn) {
        let Some(message) = self.message_clone(id) else {
            return;
        };
        self.document = Some(Document {
            kind: DocumentKind::Message(message.id),
            title: format!("Daily {}", message.created_at.format("%Y-%m-%d")),
            source: message.body,
            scroll: 0,
            target_line: None,
            return_to,
        });
        self.center_view = CenterView::Document;
        self.focus = Focus::Center;
    }

    fn close_document(&mut self) {
        let Some(document) = self.document.take() else {
            self.center_view = CenterView::Chat;
            return;
        };
        match document.return_to {
            DocumentReturn::Search => {
                self.center_view = CenterView::Search;
                self.focus = Focus::Center;
            }
            DocumentReturn::Chat => {
                self.center_view = CenterView::Chat;
                self.focus = if matches!(document.kind, DocumentKind::File(_)) {
                    Focus::Files
                } else {
                    Focus::Center
                };
            }
        }
    }

    fn act(&mut self, action: Action) -> Option<Command> {
        let id = self.selected_id()?.to_string();
        self.dispatch_action(&id, action)
    }

    fn dispatch_action(&mut self, id: &str, action: Action) -> Option<Command> {
        match action {
            Action::Ai => {
                self.open_agent_prompt(id);
                None
            }
            Action::Move => {
                self.pending_id = Some(id.to_string());
                self.file_query.clear();
                self.reload_files();
                self.files_context = FilesContext::MoveTarget;
                self.ensure_visible_file_selection();
                self.focus = Focus::Files;
                None
            }
            Action::Archive => {
                if let Some(message) = self.message_clone(id) {
                    match self.storage.archive_daily(&message.id) {
                        Ok(_) => {
                            self.record_undo(UndoOp::Archive(message));
                            self.set_status("Daily note archived");
                            self.reload_workspace();
                        }
                        Err(error) => self.set_status(format!("Error: {error}")),
                    }
                }
                None
            }
            Action::New => {
                self.pending_id = Some(id.to_string());
                self.new_file_input.clear();
                self.new_file_cursor = 0;
                self.files_context = FilesContext::NewTarget;
                self.focus = Focus::Files;
                self.open_file_name_dialog(DialogPurpose::NewFile);
                None
            }
            Action::View => {
                self.open_message_document(id, DocumentReturn::Chat);
                None
            }
            Action::Edit => self.message_edit_command(id),
            Action::Delete => {
                self.pending_id = Some(id.to_string());
                self.set_overlay(Overlay::ConfirmDeleteMessage);
                None
            }
        }
    }

    fn open_agent_prompt(&mut self, id: &str) {
        if self.ai_running {
            self.set_status("AI is already working");
            return;
        }
        if self.message_clone(id).is_none() {
            self.set_status("Message not found");
            return;
        }
        self.ai_source_id = Some(id.to_string());
        self.ai_prompt_input.clear();
        self.ai_prompt_cursor = 0;
        self.set_overlay(Overlay::AiPrompt);
    }

    fn message_edit_command(&self, id: &str) -> Option<Command> {
        self.message_clone(id).map(|message| Command::EditMessage {
            id: message.id,
            body: message.body,
        })
    }

    pub fn apply_external_message_edit(&mut self, id: &str, body: String) {
        let Some(message) = self.message_clone(id) else {
            self.set_status("Message not found");
            return;
        };
        if message.body == body {
            self.set_status("Message unchanged");
            return;
        }
        let old = message.clone();
        let mut updated = message;
        updated.body = body;
        match self.storage.replace_message(&updated) {
            Ok(true) => {
                self.record_undo(UndoOp::Edit(old));
                self.reload();
                self.reload_todos();
                if let Some(index) = self.messages.iter().position(|message| message.id == id) {
                    self.selected = index;
                }
                if let Some(document) = self.document.as_mut().filter(|document| {
                    matches!(&document.kind, DocumentKind::Message(message_id) if message_id == id)
                }) {
                    document.source = updated.body;
                }
                self.set_status("Message updated in editor");
            }
            Ok(false) => self.set_status("Message not found"),
            Err(error) => self.set_status(format!("Error: {error}")),
        }
    }

    fn submit_agent_prompt(&mut self) {
        let Some(id) = self.ai_source_id.take() else {
            self.overlay = None;
            return;
        };
        let Some(message) = self.message_clone(&id) else {
            self.overlay = None;
            self.set_status("Message not found");
            return;
        };
        let requested = self.ai_prompt_input.trim();
        let content = if requested.is_empty() {
            message.body
        } else {
            requested.to_string()
        };
        let prompt = format!("Source Nole daily date: {id}\n\n{content}");
        self.overlay = None;
        self.dialog = None;
        self.start_agent(prompt, content);
    }

    fn submit_compose_to_agent(&mut self) {
        let Some(prompt) = self.compose_agent_prompt() else {
            self.set_status("Enter a prompt for Agent");
            return;
        };
        let display_prompt = self.input.trim().to_string();
        if self.start_agent(prompt, display_prompt) {
            self.input.clear();
            self.input_cursor = 0;
        }
    }

    fn compose_agent_prompt(&self) -> Option<String> {
        let content = self.input.trim();
        if content.is_empty() {
            return None;
        }
        let note = self
            .document
            .as_ref()
            .filter(|_| self.center_view == CenterView::Document)
            .and_then(|document| match &document.kind {
                DocumentKind::File(path) => Some(path),
                DocumentKind::Message(_) => None,
            });
        Some(if let Some(path) = note {
            let display = path
                .strip_prefix(&self.storage.root)
                .unwrap_or(path)
                .to_string_lossy();
            format!("The user is currently viewing note: {display}\n\n{content}")
        } else {
            content.to_string()
        })
    }

    fn start_agent(&mut self, prompt: String, display_prompt: String) -> bool {
        if self.ai_running {
            self.set_status("AI is already working");
            return false;
        }
        let config_path = self.storage.ai_config_path.clone();
        let root = self.storage.root.clone();
        let (event_sender, event_receiver) = mpsc::channel();
        let (approval_sender, approval_receiver) = mpsc::channel();
        let (user_sender, user_receiver) = mpsc::channel();
        self.ai_events = Some(event_receiver);
        self.ai_approval_sender = Some(approval_sender);
        self.ai_user_sender = Some(user_sender);
        self.ai_running = true;
        self.agent_prompt = display_prompt;
        self.agent_output.clear();
        self.agent_output_final = false;
        self.agent_scroll = 0;
        self.set_status("AI is working...");
        let bypass = self.permission_bypass.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.ai_cancel = Some(cancelled.clone());
        thread::spawn(move || {
            let result = Agent::from_config(
                &config_path,
                &root,
                event_sender.clone(),
                approval_receiver,
                user_receiver,
                bypass,
                cancelled,
            )
            .and_then(|agent| agent.run(&prompt))
            .map_err(|error| error.to_string());
            let _ = event_sender.send(AgentEvent::Finished(result));
        });
        true
    }

    fn cancel_agent(&mut self) {
        if !self.ai_running {
            self.set_status("Agent is not running");
            return;
        }
        if let Some(cancelled) = self.ai_cancel.take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.ai_running = false;
        self.ai_events = None;
        self.ai_approval_sender = None;
        self.ai_user_sender = None;
        self.approval_request = None;
        self.clear_ask_user();
        if self.overlay == Some(Overlay::Approval) {
            self.overlay = None;
        }
        self.agent_output.push("Cancelled".to_string());
        self.agent_output_final = false;
        self.agent_scroll = u16::MAX;
        self.set_status("Agent task cancelled");
    }

    fn cancel_file_context(&mut self) {
        self.pending_id = None;
        self.pending_file = None;
        self.files_context = FilesContext::Browse;
        self.focus = Focus::Center;
    }

    fn perform_move_to(&mut self, path: &Path) {
        let Some(id) = self.pending_id.clone() else {
            self.cancel_file_context();
            return;
        };
        self.perform_move_to_id(path, &id);
    }

    fn perform_move_to_id(&mut self, path: &Path, id: &str) {
        let Some(message) = self.message_clone(id) else {
            self.set_status("Message not found");
            return;
        };
        match self.storage.move_to_markdown(path, &message) {
            Ok(appended) => {
                self.record_undo(UndoOp::Move {
                    message,
                    target: path.to_path_buf(),
                    appended,
                });
                self.set_status(format!(
                    "Moved to {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                self.pending_id = None;
                self.files_context = FilesContext::Browse;
                self.focus = Focus::Center;
                self.center_view = CenterView::Chat;
                self.reload_workspace();
            }
            Err(error) => self.set_status(format!("Error: {error}")),
        }
    }

    fn send_message(&mut self) {
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
            Some(DocumentKind::File(path)) => self.append_to_open_note(&path, &body),
            Some(DocumentKind::Message(date)) => self.append_to_open_daily(&date, &body),
            None => self.append_to_today(&body),
        };
        if let Err(error) = result {
            self.set_status(format!("Error: {error}"));
        }
    }

    fn append_to_open_note(&mut self, path: &Path, body: &str) -> anyhow::Result<()> {
        self.storage.append_document(path, body)?;
        let source = self.storage.read_document_file(path)?;
        if let Some(document) = self.document.as_mut() {
            document.source = source;
        }
        self.input.clear();
        self.input_cursor = 0;
        self.reload_files();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.notifications.notify(format!("Appended to {name}"));
        self.set_status("Appended without leaving the document");
        Ok(())
    }

    fn append_to_open_daily(&mut self, date: &str, body: &str) -> anyhow::Result<()> {
        let message = self.storage.append_daily(date, body)?;
        if let Some(document) = self.document.as_mut() {
            document.source = message.body;
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

    fn append_to_today(&mut self, body: &str) -> anyhow::Result<()> {
        self.storage.append_chat_message(body)?;
        self.input.clear();
        self.input_cursor = 0;
        self.reload();
        self.reload_todos();
        self.selected = self.messages.len().saturating_sub(1);
        self.scroll = u16::MAX;
        self.reveal_selected_message = true;
        self.set_status("Saved");
        Ok(())
    }

    fn message_clone(&self, id: &str) -> Option<Message> {
        self.messages
            .iter()
            .find(|message| message.id == id)
            .cloned()
    }

    fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    fn record_undo(&mut self, operation: UndoOp) {
        const CAPACITY: usize = 50;
        if self.undo_stack.len() == CAPACITY {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(operation);
    }

    fn undo(&mut self) {
        let Some(operation) = self.undo_stack.pop() else {
            self.set_status("Nothing to undo");
            return;
        };
        let status = match operation {
            UndoOp::Delete(message) => match self.storage.restore_message_to_chat(&message) {
                Ok(()) => "Undid delete".to_string(),
                Err(error) => format!("Undo error: {error}"),
            },
            UndoOp::Move {
                message,
                target,
                appended,
            } => match self.storage.restore_message_to_chat(&message) {
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
            UndoOp::Archive(message) => match self.storage.restore_archived_daily(&message.id) {
                Ok(()) => "Undid archive".to_string(),
                Err(error) => format!("Undo error: {error}"),
            },
            UndoOp::Edit(message) => match self.storage.replace_message(&message) {
                Ok(true) => "Undid edit".to_string(),
                Ok(false) => "Undid edit (message gone)".to_string(),
                Err(error) => format!("Undo error: {error}"),
            },
        };
        self.set_status(status);
        self.reload_workspace();
        self.selected = self.messages.len().saturating_sub(1);
        self.scroll = u16::MAX;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_app() -> (App, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        (App::new(storage).unwrap(), directory)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn add_message(app: &mut App, body: &str) {
        app.storage.append_chat_message(body).unwrap();
        app.reload();
        app.selected = app.messages.len() - 1;
        app.focus = Focus::Center;
    }

    #[test]
    fn starts_with_compose_focused_and_chat_in_center() {
        let (app, _directory) = make_app();
        assert_eq!(app.focus, Focus::Compose);
        assert_eq!(app.center_view, CenterView::Chat);
        assert_eq!(app.files_context, FilesContext::Browse);
        assert_eq!(app.overlay, None);
        assert_eq!(app.permission_mode, PermissionMode::Approve);
    }

    #[test]
    fn command_dialog_supports_single_multi_and_free_text_modes() {
        let (mut app, _directory) = make_app();
        app.open_dialog(DialogState::new(
            "Format",
            "Choose a format",
            DialogMode::SingleSelect,
            DialogPurpose::Custom,
            vec![DialogOption::new("Markdown"), DialogOption::new("MBDown")],
        ));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.take_dialog_result(),
            Some(DialogResult::Selected("MBDown".to_string()))
        );

        app.open_dialog(DialogState::new(
            "Targets",
            "Select targets",
            DialogMode::MultiSelect,
            DialogPurpose::Custom,
            vec![DialogOption::new("daily"), DialogOption::new("archives")],
        ));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char(' ')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.take_dialog_result(),
            Some(DialogResult::SelectedMany(vec![
                "daily".to_string(),
                "archives".to_string()
            ]))
        );

        app.open_dialog(DialogState::new(
            "Name",
            "Choose or type a name",
            DialogMode::SelectOrInput,
            DialogPurpose::Custom,
            vec![DialogOption::new("Existing")],
        ));
        app.handle_key(key(KeyCode::Char('n')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.take_dialog_result(),
            Some(DialogResult::Text("n".to_string()))
        );
    }

    #[test]
    fn animation_phase_advances_only_while_agent_runs() {
        let (mut app, _directory) = make_app();
        app.advance_animation();
        assert_eq!(app.animation_tick, 0);
        app.ai_running = true;
        app.advance_animation();
        app.advance_animation();
        assert_eq!(app.animation_tick, 2);
        app.ai_running = false;
        app.advance_animation();
        assert_eq!(app.animation_tick, 2);
    }

    #[test]
    fn tab_switches_permission_mode_without_changing_focus() {
        let (mut app, _directory) = make_app();
        assert_eq!(app.focus, Focus::Compose);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.permission_mode, PermissionMode::Bypass);
        assert_eq!(app.focus, Focus::Compose);
        assert!(app.permission_bypass.load(Ordering::Relaxed));
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.permission_mode, PermissionMode::Approve);
        assert!(!app.permission_bypass.load(Ordering::Relaxed));
    }

    #[test]
    fn ai_action_opens_an_optional_prompt_overlay() {
        let (mut app, _directory) = make_app();
        add_message(&mut app, "card body");
        let id = app.selected_id().unwrap().to_string();
        app.dispatch_action(&id, Action::Ai);
        assert_eq!(app.overlay, Some(Overlay::AiPrompt));
        assert_eq!(app.ai_source_id.as_deref(), Some(id.as_str()));
        app.handle_paste("custom prompt");
        assert_eq!(app.ai_prompt_input, "custom prompt");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn approval_overlay_sends_the_user_decision() {
        let (mut app, _directory) = make_app();
        let (sender, receiver) = mpsc::channel();
        app.ai_approval_sender = Some(sender);
        app.approval_request = Some(ApprovalRequest {
            title: "Update note".to_string(),
            diff: "--- old\n+++ new\n-old\n+new\n".to_string(),
        });
        app.overlay = Some(Overlay::Approval);
        app.handle_key(key(KeyCode::Char('y')));
        assert_eq!(receiver.try_recv().unwrap(), ApprovalDecision::Approve);
        assert_eq!(app.overlay, None);
        assert!(app.approval_request.is_none());
    }

    #[test]
    fn ask_user_overlay_accepts_options_and_custom_text() {
        let (mut app, _directory) = make_app();
        let (event_sender, event_receiver) = mpsc::channel();
        let (answer_sender, answer_receiver) = mpsc::channel();
        app.ai_events = Some(event_receiver);
        app.ai_user_sender = Some(answer_sender);
        event_sender
            .send(AgentEvent::AskUser(AskUserRequest {
                question: "Choose a format".to_string(),
                options: vec!["Markdown".to_string(), "MBDown".to_string()],
            }))
            .unwrap();
        app.poll_agent();
        assert_eq!(app.overlay, Some(Overlay::AskUser));

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            answer_receiver.try_recv().unwrap(),
            AskUserResponse::Answer("MBDown".to_string())
        );
        assert_eq!(app.overlay, None);

        event_sender
            .send(AgentEvent::AskUser(AskUserRequest {
                question: "Anything else?".to_string(),
                options: vec!["No".to_string()],
            }))
            .unwrap();
        app.poll_agent();
        app.handle_key(key(KeyCode::Char('Y')));
        app.handle_paste("es, use colors");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            answer_receiver.try_recv().unwrap(),
            AskUserResponse::Answer("Yes, use colors".to_string())
        );
    }

    #[test]
    fn agent_panel_keeps_only_the_final_reply() {
        let (mut app, _directory) = make_app();
        let (sender, receiver) = mpsc::channel();
        app.ai_events = Some(receiver);
        app.ai_running = true;
        sender
            .send(AgentEvent::Progress("Using read_file".to_string()))
            .unwrap();
        app.poll_agent();
        assert_eq!(app.status, "Using read_file");
        assert_eq!(app.agent_output, ["Using read_file"]);
        assert!(!app.agent_output_final);

        sender
            .send(AgentEvent::Finished(Ok("final reply".to_string())))
            .unwrap();
        app.poll_agent();
        assert_eq!(app.agent_output, ["final reply"]);
        assert!(app.agent_output_final);
        assert_eq!(app.status, "Agent finished");
    }

    #[test]
    fn c_cancels_a_running_agent_only_from_the_agent_panel() {
        let (mut app, _directory) = make_app();
        let cancelled = Arc::new(AtomicBool::new(false));
        app.ai_cancel = Some(cancelled.clone());
        app.ai_running = true;
        app.agent_output = vec!["Using web_fetch".to_string()];
        app.focus = Focus::Center;

        app.handle_key(key(KeyCode::Char('c')));
        assert!(app.ai_running);
        assert!(!cancelled.load(Ordering::Relaxed));

        app.focus = Focus::Agent;
        app.handle_key(key(KeyCode::Char('c')));
        assert!(!app.ai_running);
        assert!(cancelled.load(Ordering::Relaxed));
        assert_eq!(
            app.agent_output.last().map(String::as_str),
            Some("Cancelled")
        );
        assert!(!app.agent_output_final);
        assert!(app.ai_events.is_none());
        assert!(app.ai_approval_sender.is_none());
        assert!(app.ai_user_sender.is_none());
        assert_eq!(app.status, "Agent task cancelled");
    }

    #[test]
    fn recording_from_a_document_appends_to_it_and_keeps_it_open() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Article.md");
        fs::write(&path, "# Article\n\nInspiration\n").unwrap();
        app.open_file_document(&path, DocumentReturn::Chat);
        app.document.as_mut().unwrap().scroll = 1;
        app.handle_key(key(KeyCode::Char('i')));
        assert_eq!(app.focus, Focus::Compose);
        app.handle_paste("new idea");
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(path.clone()))
        );
        assert_eq!(app.document.as_ref().unwrap().scroll, 1);
        assert!(app.messages.is_empty());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Article\n\nInspiration\n\nnew idea\n"
        );
        assert_eq!(
            app.document.as_ref().unwrap().source,
            "# Article\n\nInspiration\n\nnew idea\n"
        );
        assert_eq!(
            app.notifications.visible().as_deref(),
            Some("Appended to Article.md")
        );
    }

    #[test]
    fn recording_from_a_daily_preview_appends_to_that_date() {
        let (mut app, _directory) = make_app();
        app.storage.append_daily("2026-07-26", "first").unwrap();
        app.reload();
        app.open_message_document("2026-07-26", DocumentReturn::Chat);
        app.handle_key(key(KeyCode::Char('i')));
        app.handle_paste("second");
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.storage.read_daily_by_date("2026-07-26").unwrap().body,
            "first\n\nsecond"
        );
        assert_eq!(app.document.as_ref().unwrap().source, "first\n\nsecond");
        assert_eq!(
            app.notifications.visible().as_deref(),
            Some("Appended to Daily 2026-07-26")
        );
    }

    #[test]
    fn reload_keeps_the_selected_message_by_id() {
        let (mut app, _directory) = make_app();
        let first = app.storage.append_daily("2026-07-26", "first").unwrap();
        let second = app.storage.append_daily("2026-07-27", "second").unwrap();
        app.reload();
        app.selected = app
            .messages
            .iter()
            .position(|message| message.id == second.id)
            .unwrap();

        app.storage.remove_message_by_id(&first.id).unwrap();
        app.reload();

        assert_eq!(app.selected_id(), Some(second.id.as_str()));
    }

    #[test]
    fn compose_paste_normalizes_newlines_at_character_cursor() {
        let (mut app, _directory) = make_app();
        app.input = "ab".to_string();
        app.input_cursor = 1;
        app.handle_paste("X\r\nY\rZ");
        assert_eq!(app.input, "aX\nY\nZb");
        assert_eq!(app.input_cursor, 6);
    }

    #[test]
    fn compose_agent_prompt_includes_the_current_note_path() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Reference.md");
        fs::write(&path, "# Reference\n").unwrap();
        app.open_file_document(&path, DocumentReturn::Chat);
        app.input = "Summarize the key point".to_string();

        let prompt = app.compose_agent_prompt().unwrap();
        assert!(prompt.contains("currently viewing note: data/Reference.md"));
        assert!(prompt.ends_with("Summarize the key point"));

        app.document = Some(Document {
            kind: DocumentKind::Message("msg-1".to_string()),
            title: "Message".to_string(),
            source: String::new(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Chat,
        });
        assert_eq!(
            app.compose_agent_prompt().as_deref(),
            Some("Summarize the key point")
        );
    }

    #[test]
    fn ctrl_enter_sends_compose_to_agent_without_creating_a_chat_card() {
        let (mut app, _directory) = make_app();
        let message_count = app.messages.len();
        app.input = "Direct Agent prompt".to_string();
        app.input_cursor = app.input.chars().count();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

        assert!(app.ai_running);
        assert_eq!(app.agent_prompt, "Direct Agent prompt");
        assert!(app.input.is_empty());
        assert_eq!(app.input_cursor, 0);
        assert_eq!(app.messages.len(), message_count);
    }

    #[test]
    fn ctrl_enter_preserves_compose_while_agent_is_busy() {
        let (mut app, _directory) = make_app();
        app.ai_running = true;
        app.input = "Keep this prompt".to_string();
        app.input_cursor = app.input.chars().count();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

        assert_eq!(app.input, "Keep this prompt");
        assert_eq!(app.status, "AI is already working");
    }

    #[test]
    fn f_and_t_change_only_focus() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.handle_key(key(KeyCode::Char('f')));
        assert_eq!(app.focus, Focus::Files);
        assert_eq!(app.center_view, CenterView::Chat);
        app.handle_key(key(KeyCode::Char('T')));
        assert_eq!(app.focus, Focus::Todo);
        assert_eq!(app.center_view, CenterView::Chat);
    }

    #[test]
    fn arrows_move_focus_across_the_workspace() {
        let (mut app, _directory) = make_app();
        add_message(&mut app, "selected card");
        app.focus = Focus::Center;

        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.focus, Focus::Files);
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.focus, Focus::Center);
        assert_eq!(app.center_view, CenterView::Chat);

        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.focus, Focus::Todo);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.focus, Focus::Center);

        app.todo_items.clear();
        app.agent_output = vec!["final reply".to_string()];
        app.focus = Focus::Todo;
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.focus, Focus::Agent);
    }

    #[test]
    fn file_tree_keeps_both_groups_and_expands_archives_on_demand() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Note.md"), "note").unwrap();
        fs::write(app.storage.archives_dir.join("Old.md"), "old").unwrap();
        app.reload_files();

        let rows = app.visible_file_rows();
        assert!(rows.contains(&FileListRow::Group(FileGroup::Notes)));
        assert!(rows.contains(&FileListRow::Group(FileGroup::Archives)));
        assert!(rows.iter().any(|row| matches!(
            row,
            FileListRow::File(index) if !app.note_files[*index].archived
        )));
        assert!(!rows.iter().any(|row| matches!(
            row,
            FileListRow::File(index) if app.note_files[*index].archived
        )));

        app.archives_expanded = true;
        assert!(app.visible_file_rows().iter().any(|row| matches!(
            row,
            FileListRow::File(index) if app.note_files[*index].archived
        )));
    }

    #[test]
    fn file_search_includes_archives_but_move_targets_do_not() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Active.md"), "active").unwrap();
        fs::write(app.storage.archives_dir.join("Archived.md"), "old").unwrap();
        app.reload_files();
        app.files_context = FilesContext::Search;
        app.file_query = "arch".to_string();
        assert!(app.visible_file_rows().iter().any(|row| matches!(
            row,
            FileListRow::File(index) if app.note_files[*index].archived
        )));

        app.files_context = FilesContext::MoveTarget;
        app.file_query.clear();
        assert!(app.visible_file_rows().iter().all(|row| matches!(
            row,
            FileListRow::File(index) if !app.note_files[*index].archived
        )));
    }

    #[test]
    fn todo_and_agent_form_a_navigable_right_sidebar() {
        let (mut app, _directory) = make_app();
        app.todo_items = vec![TodoItem {
            checked: false,
            text: "only task".to_string(),
        }];
        app.todo_index = 0;
        app.agent_output = vec!["final reply".to_string()];
        app.focus = Focus::Todo;

        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.focus, Focus::Agent);
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.focus, Focus::Todo);

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.focus, Focus::Center);
    }

    #[test]
    fn enter_on_agent_output_adds_exactly_one_chat_card() {
        let (mut app, _directory) = make_app();
        let original_count = app.messages.len();
        app.agent_prompt = "User prompt".to_string();
        app.agent_output = vec!["Agent final reply".to_string()];
        app.agent_output_final = true;
        app.focus = Focus::Agent;

        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.messages.len(), original_count + 1);
        assert_eq!(app.messages.last().unwrap().body, "Agent final reply");
        assert!(app.agent_prompt.is_empty());
        assert!(app.agent_output.is_empty());
        assert_eq!(app.focus, Focus::Center);

        app.add_agent_output_to_chat();
        assert_eq!(app.messages.len(), original_count + 1);
        assert_eq!(app.status, "Agent has no final response to add");
    }

    #[test]
    fn files_search_uses_note_files_without_duplicate_lists() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
        fs::write(app.storage.data_dir.join("Personal.md"), "personal").unwrap();
        app.open_files();
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.files_context, FilesContext::Search);
        app.handle_key(key(KeyCode::Char('w')));
        app.handle_key(key(KeyCode::Char('k')));
        let visible = app
            .visible_file_rows()
            .into_iter()
            .filter_map(|row| match row {
                FileListRow::File(index) => Some(index),
                FileListRow::Group(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(visible.len(), 1);
        assert_eq!(
            app.note_files[visible[0]]
                .path
                .file_stem()
                .and_then(|stem| stem.to_str()),
            Some("Work")
        );
    }

    #[test]
    fn file_enter_opens_center_document_and_escape_returns_to_files() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "# Project\n").unwrap();
        app.open_files();
        app.selected_file = Some(path.clone());
        app.file_index = app
            .note_files
            .iter()
            .position(|file| file.path == path)
            .unwrap();
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Center);
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(path))
        );
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.center_view, CenterView::Chat);
        assert_eq!(app.focus, Focus::Files);
    }

    #[test]
    fn file_edit_returns_terminal_command() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "# Project\n").unwrap();
        app.open_files();
        app.file_index = app
            .note_files
            .iter()
            .position(|file| file.path == path)
            .unwrap();
        app.sync_selected_file();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('e'))),
            Some(Command::Edit(path))
        );
    }

    #[test]
    fn document_edit_returns_terminal_command() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "# Project\n").unwrap();
        app.open_file_document(&path, DocumentReturn::Chat);

        assert_eq!(
            app.handle_key(key(KeyCode::Char('e'))),
            Some(Command::Edit(path))
        );
        assert_eq!(app.center_view, CenterView::Document);
    }

    #[test]
    fn document_search_reuses_search_view_and_jumps_to_source_line() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "# Project\nfirst needle\nother\nsecond NEEDLE\n").unwrap();
        fs::write(
            app.storage.data_dir.join("Other.md"),
            "needle outside document\n",
        )
        .unwrap();
        app.open_file_document(&path, DocumentReturn::Chat);

        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.center_view, CenterView::DocumentSearch);
        app.handle_paste("needle");
        assert_eq!(app.search_results.len(), 2);
        assert!(matches!(
            app.search_results[0],
            SearchHit::DocumentLine { line_no: 2, .. }
        ));

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(app.document.as_ref().unwrap().target_line, Some(4));
    }

    #[test]
    fn escape_from_document_search_returns_to_document() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "# Project\n").unwrap();
        app.open_file_document(&path, DocumentReturn::Chat);
        app.handle_key(key(KeyCode::Char('/')));

        app.handle_key(key(KeyCode::Esc));

        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(path))
        );
    }

    #[test]
    fn move_and_new_are_file_contexts() {
        let (mut app, _directory) = make_app();
        add_message(&mut app, "file this");
        app.handle_key(key(KeyCode::Char('m')));
        assert_eq!(app.focus, Focus::Files);
        assert_eq!(app.files_context, FilesContext::MoveTarget);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.files_context, FilesContext::Browse);
        assert_eq!(app.focus, Focus::Center);

        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.focus, Focus::Files);
        assert_eq!(app.files_context, FilesContext::NewTarget);
    }

    #[test]
    fn file_rename_is_a_context_and_delete_is_an_overlay() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Old.md"), "old").unwrap();
        app.open_files();
        app.file_index = app
            .note_files
            .iter()
            .position(|file| file.path.ends_with("Old.md"))
            .unwrap();
        app.sync_selected_file();
        app.handle_key(key(KeyCode::Char('r')));
        assert_eq!(app.files_context, FilesContext::Rename);
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteFile));
        assert_eq!(app.focus, Focus::Files);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.overlay, None);
        assert_eq!(app.focus, Focus::Files);
    }

    #[test]
    fn renaming_the_open_document_retargets_it_before_workspace_reload() {
        let (mut app, _directory) = make_app();
        let from = app.storage.data_dir.join("Old.md");
        fs::write(&from, "# Old\n\nBody\n").unwrap();
        app.reload_files();
        app.file_index = app
            .note_files
            .iter()
            .position(|file| file.path == from)
            .unwrap();
        app.sync_selected_file();
        app.open_file_document(&from, DocumentReturn::Chat);
        app.focus = Focus::Files;

        app.handle_key(key(KeyCode::Char('r')));
        app.rename_input = "Renamed".to_string();
        app.rename_cursor = app.rename_input.chars().count();
        app.handle_key(key(KeyCode::Enter));

        let to = app.storage.data_dir.join("Renamed.md");
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(app.selected_file.as_ref(), Some(&to));
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(to.clone()))
        );
        assert_eq!(
            app.document
                .as_ref()
                .map(|document| document.title.as_str()),
            Some("Renamed.md")
        );

        app.reload_workspace();
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document
                .as_ref()
                .map(|document| document.source.as_str()),
            Some("# Old\n\nBody\n")
        );
        assert!(!app.status.starts_with("Reload error:"));
    }

    #[test]
    fn agent_move_event_keeps_the_open_document_across_watcher_reload() {
        let (mut app, _directory) = make_app();
        let from = app.storage.data_dir.join("Old.md");
        fs::write(&from, "# Old\n\nBody\n").unwrap();
        app.open_file_document(&from, DocumentReturn::Chat);
        let destination_dir = app.storage.data_dir.join("moved");
        fs::create_dir(&destination_dir).unwrap();
        let to = destination_dir.join("Renamed.md");
        fs::rename(&from, &to).unwrap();

        let (sender, receiver) = mpsc::channel();
        app.ai_events = Some(receiver);
        app.ai_running = true;

        // The filesystem watcher can run before the Agent event is polled.
        app.reload_workspace();
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(from))
        );

        sender
            .send(AgentEvent::FileMoved {
                from: PathBuf::from("data/Old.md"),
                to: PathBuf::from("data/moved/Renamed.md"),
            })
            .unwrap();
        sender
            .send(AgentEvent::Finished(Ok("Moved the file".to_string())))
            .unwrap();
        app.poll_agent();

        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(to.clone()))
        );
        assert_eq!(
            app.document
                .as_ref()
                .map(|document| document.source.as_str()),
            Some("# Old\n\nBody\n")
        );
        assert_eq!(
            app.document
                .as_ref()
                .map(|document| document.title.as_str()),
            Some("Renamed.md")
        );
        assert!(!app.status.starts_with("Reload error:"));
    }

    #[test]
    fn search_result_message_edit_returns_external_editor_command() {
        let (mut app, _directory) = make_app();
        add_message(&mut app, "needle");
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.center_view, CenterView::Search);
        app.handle_paste("needle");
        assert_eq!(app.search_results.len(), 1);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| document.return_to),
            Some(DocumentReturn::Search)
        );
        let id = app.messages[0].id.clone();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('e'))),
            Some(Command::EditMessage {
                id,
                body: "needle".to_string(),
            })
        );
        assert_eq!(app.center_view, CenterView::Document);
    }

    #[test]
    fn file_search_result_keeps_its_source_line_as_a_document_anchor() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "# Project\n\nintro\n\nunique needle\n").unwrap();
        app.reload_files();
        app.open_search();
        app.handle_paste("unique needle");
        assert_eq!(app.search_results.len(), 1);
        app.handle_key(key(KeyCode::Enter));
        let document = app.document.as_ref().expect("opened document");
        assert_eq!(document.kind, DocumentKind::File(path));
        assert_eq!(document.target_line, Some(5));
        assert_eq!(document.return_to, DocumentReturn::Search);
    }

    #[test]
    fn external_message_edit_writes_by_id_and_remains_undoable() {
        let (mut app, _directory) = make_app();
        add_message(&mut app, "before");
        let id = app.messages[0].id.clone();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('e'))),
            Some(Command::EditMessage {
                id: id.clone(),
                body: "before".to_string(),
            })
        );
        app.apply_external_message_edit(&id, "after".to_string());
        assert_eq!(app.messages[0].body, "after");
        app.handle_key(key(KeyCode::Char('u')));
        assert_eq!(app.messages[0].body, "before");
    }

    #[test]
    fn help_overlay_restores_underlying_state() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Todo;
        app.open_help();
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.help_scroll, 1);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.overlay, None);
        assert_eq!(app.focus, Focus::Todo);
        assert_eq!(app.center_view, CenterView::Chat);
    }

    #[test]
    fn wheel_routes_by_layout_coordinates_not_focus() {
        let (mut app, _directory) = make_app();
        app.todo_items = vec![
            TodoItem {
                checked: false,
                text: "one".to_string(),
            },
            TodoItem {
                checked: false,
                text: "two".to_string(),
            },
        ];
        app.layout.todo = Some(Rect::new(80, 0, 20, 20));
        app.layout.center = Some(Rect::new(20, 0, 60, 20));
        app.focus = Focus::Center;
        app.scroll = 4;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 90,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.todo_index, 1);
        assert_eq!(app.scroll, 4);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 30,
            row: 4,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.scroll, 3);
    }

    #[test]
    fn todo_navigation_follows_grouped_display_order() {
        let (mut app, _directory) = make_app();
        app.todo_items = vec![
            TodoItem {
                checked: true,
                text: "done first in file".to_string(),
            },
            TodoItem {
                checked: false,
                text: "open second in file".to_string(),
            },
            TodoItem {
                checked: false,
                text: "open third in file".to_string(),
            },
        ];
        assert_eq!(app.visible_todo_indices(), vec![1, 2, 0]);
        app.todo_index = 1;
        app.move_todo_selection(1);
        assert_eq!(app.todo_index, 2);
        app.move_todo_selection(1);
        assert_eq!(app.todo_index, 0);
    }

    #[test]
    fn non_left_mouse_buttons_are_ignored() {
        let (mut app, _directory) = make_app();
        app.layout.files = Some(Rect::new(0, 0, 20, 20));
        app.focus = Focus::Center;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.focus, Focus::Center);
    }

    #[test]
    fn link_clicks_open_external_targets_or_internal_wiki_notes() {
        let (mut app, _directory) = make_app();
        app.link_hitboxes.push(LinkHitbox {
            target: LinkTarget::External("https://example.test".to_string()),
            area: Rect::new(4, 3, 7, 1),
        });
        let command = app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            command,
            Some(Command::OpenLink("https://example.test".to_string()))
        );

        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "linked note").unwrap();
        app.reload_files();
        app.link_hitboxes.clear();
        app.link_hitboxes.push(LinkHitbox {
            target: LinkTarget::WikiLink("Project".to_string()),
            area: Rect::new(4, 3, 7, 1),
        });
        assert!(app
            .handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            })
            .is_none());
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(path))
        );
    }

    #[test]
    fn wikilink_chooses_between_data_and_archived_matches_and_creates_missing_notes() {
        let (mut app, _directory) = make_app();
        let data = app.storage.data_dir.join("Project.md");
        let archived = app.storage.archives_dir.join("Project.md");
        fs::write(&data, "data version").unwrap();
        fs::write(&archived, "archived version").unwrap();
        app.link_hitboxes.push(LinkHitbox {
            target: LinkTarget::WikiLink("Project".to_string()),
            area: Rect::new(1, 1, 8, 1),
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.overlay, Some(Overlay::WikiLinkChoice));
        assert_eq!(app.wiki_link_candidates.len(), 2);
        assert!(!app.wiki_link_candidates[0].archived);
        assert!(app.wiki_link_candidates[1].archived);
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.overlay, None);
        assert_eq!(app.document.as_ref().unwrap().source, "archived version");

        app.document = None;
        app.center_view = CenterView::Chat;
        app.link_hitboxes.clear();
        app.link_hitboxes.push(LinkHitbox {
            target: LinkTarget::WikiLink("New Note".to_string()),
            area: Rect::new(1, 1, 8, 1),
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        let created = app.storage.data_dir.join("New Note.md");
        assert!(created.is_file());
        assert_eq!(
            app.document.as_ref().unwrap().kind,
            DocumentKind::File(created)
        );
    }

    #[test]
    fn base_escape_and_q_both_quit() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Command::Quit));
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Some(Command::Quit));
    }

    #[test]
    fn clicking_a_file_opens_it_in_center() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Clicked.md");
        fs::write(&path, "# Clicked\n").unwrap();
        app.open_files();
        app.file_hitboxes.push(FileHitbox {
            path: path.clone(),
            area: Rect::new(1, 1, 10, 2),
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(app.focus, Focus::Center);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(path))
        );
    }

    #[test]
    fn move_targets_list_managed_data_notes() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Work.md"), "# Work\n").unwrap();
        add_message(&mut app, "file this");
        app.handle_key(key(KeyCode::Char('m')));
        let names: Vec<String> = app
            .visible_file_rows()
            .into_iter()
            .filter_map(|row| match row {
                FileListRow::File(index) => app.note_files[index]
                    .path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .map(str::to_string),
                FileListRow::Group(_) => None,
            })
            .collect();
        assert_eq!(names, vec!["Work"]);
    }

    #[test]
    fn rename_error_keeps_modal_context_for_retry() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Old.md"), "old").unwrap();
        fs::write(app.storage.data_dir.join("Taken.md"), "taken").unwrap();
        app.open_files();
        app.file_index = app
            .note_files
            .iter()
            .position(|file| file.path.ends_with("Old.md"))
            .unwrap();
        app.sync_selected_file();
        app.handle_key(key(KeyCode::Char('r')));
        app.rename_input = "Taken".to_string();
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.files_context, FilesContext::Rename);
        assert!(app.pending_file.is_some());
        assert!(app.status.starts_with("Error:"));
    }

    #[test]
    fn file_name_modal_inputs_edit_at_the_character_cursor() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Files;
        app.files_context = FilesContext::NewTarget;
        app.new_file_input = "文件".to_string();
        app.new_file_cursor = 2;
        app.handle_key(key(KeyCode::Left));
        app.handle_key(key(KeyCode::Char('新')));
        assert_eq!(app.new_file_input, "文新件");
        assert_eq!(app.new_file_cursor, 2);
        app.handle_key(key(KeyCode::Backspace));
        app.handle_key(key(KeyCode::Delete));
        assert_eq!(app.new_file_input, "文");
        assert_eq!(app.new_file_cursor, 1);

        app.files_context = FilesContext::Rename;
        app.rename_input = "Report".to_string();
        app.rename_cursor = app.rename_input.chars().count();
        app.handle_key(key(KeyCode::Home));
        app.handle_paste("New-");
        app.handle_key(key(KeyCode::End));
        app.handle_key(key(KeyCode::Char('2')));
        assert_eq!(app.rename_input, "New-Report2");
        assert_eq!(app.rename_cursor, app.rename_input.chars().count());
    }

    #[test]
    fn delete_overlay_and_undo_keep_business_behavior() {
        let (mut app, _directory) = make_app();
        add_message(&mut app, "remove me");
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteMessage));
        app.handle_key(key(KeyCode::Char('y')));
        assert!(app.messages.is_empty());
        app.handle_key(key(KeyCode::Char('u')));
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].body, "remove me");
    }
}
