//! Application state and event handling.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::agent::{
    AgentEvent, AgentRuntime, AgentStopReason, AgentWorker, ApprovalDecision, ApprovalRequest,
    AskUserKind, AskUserRequest, AskUserResponse, PermissionMode, AGENT_STREAM_BUFFER,
};
use crate::agent_session::{AgentConversation, AgentPanelEntry, AgentSession, TokenUsage};
use crate::embedded_terminal::{is_terminal_toggle, EmbeddedTerminal, TerminalSnapshot};
use crate::model::{
    Action, ButtonHitbox, DailyNote, DialogOptionHitbox, FileGroup, FileGroupHitbox, FileHitbox,
    FileListRow, LinkHitbox, LinkTarget, NoteFile, SearchHit, SearchHitbox, TagHitbox, TodoHitbox,
    TodoItem, WikiLinkCandidate, WikiLinkHitbox,
};
use crate::notification::NotificationService;
use crate::observable::Observable;
use crate::storage::{LoadedTheme, Storage};
use crate::workspace_index::{TagRenamePlan, WorkspaceIndex, WorkspaceIndexHandle};

pub(in crate::app) const FORMAT_DAILY_NOTE_PROMPT: &str = "Read this daily note, then edit it in place to improve its Markdown formatting and readability. Preserve every fact, idea, task, link, and the author's meaning. Only improve structure and presentation, such as headings, paragraphs, lists, spacing, and emphasis. Do not add new factual content, and do not merely describe the changes.";

mod agent;
mod dialog;
mod dialogs;
mod document;
mod documents;
mod model;
#[cfg(test)]
mod tests;
mod terminal;
mod vlist;

pub use self::{dialog::*, document::*, model::*};
pub(crate) use self::vlist::*;

pub(in crate::app) fn point_in_rect(col: u16, row: u16, area: Rect) -> bool {
    col >= area.x
        && col < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

pub(in crate::app) fn agent_debug_logging_enabled() -> bool {
    std::env::var("NOLE_DEBUG").is_ok_and(|value| value == "1")
}

pub(in crate::app) fn in_area(col: u16, row: u16, area: Option<Rect>) -> bool {
    area.is_some_and(|area| point_in_rect(col, row, area))
}

pub(in crate::app) fn wiki_name_matches(path: &Path, requested: &str) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(requested))
        || path
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(requested))
}

/// Case-insensitive subsequence matching. An empty query matches every file.
pub(in crate::app) fn fuzzy_match(haystack: &str, needle: &str) -> bool {
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

pub(in crate::app) fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

pub(in crate::app) fn insert_char(buffer: &mut String, cursor: &mut usize, character: char) {
    buffer.insert(char_to_byte(buffer, *cursor), character);
    *cursor += 1;
}

pub(in crate::app) fn delete_backward(buffer: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = char_to_byte(buffer, *cursor - 1);
    let end = char_to_byte(buffer, *cursor);
    buffer.replace_range(start..end, "");
    *cursor -= 1;
}

pub(in crate::app) fn delete_forward(buffer: &mut String, cursor: &mut usize) {
    if *cursor >= buffer.chars().count() {
        return;
    }
    let start = char_to_byte(buffer, *cursor);
    let end = char_to_byte(buffer, *cursor + 1);
    buffer.replace_range(start..end, "");
}

pub(in crate::app) fn paste_into(buffer: &mut String, cursor: &mut usize, text: &str) {
    buffer.insert_str(char_to_byte(buffer, *cursor), text);
    *cursor += text.chars().count();
}

pub(in crate::app) fn move_cursor(buffer: &str, cursor: usize, movement: CursorMove) -> usize {
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

pub struct App {
    pub storage: Storage,
    pub theme: crate::theme::Theme,
    pub theme_selection: String,
    pub active_theme: String,
    pub theme_source: Option<PathBuf>,
    pub(crate) images: crate::media::ImageService,

    pub focus: Focus,
    pub center_view: CenterView,
    pub files_context: FilesContext,
    pub overlay: Option<Overlay>,
    pub document: Option<Document>,
    document_render_lru: DocumentRenderLru,

    pub daily_notes: Vec<DailyNote>,
    pub(crate) daily_vlist: DailyVirtualList,
    pub selected: usize,
    pub scroll: u16,
    /// Set only when navigation should bring the selected card back on screen.
    pub reveal_selected_daily: bool,

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

    /// Daily note being moved, filed, or deleted by a contextual interaction.
    pub pending_daily_date: Option<NaiveDate>,
    new_note_from_template: bool,
    /// File awaiting rename or delete confirmation.
    pub pending_file: Option<PathBuf>,

    pub todo_items: Vec<TodoItem>,
    pub todo_index: usize,

    pub search_query: String,
    pub search_results: Vec<SearchHit>,
    pub search_index: usize,
    workspace_index: WorkspaceIndexHandle,
    pending_tag_rename: Option<String>,

    pub help_scroll: u16,
    pub status: String,
    pub animation_tick: u64,
    pub mouse_captured: bool,
    pub layout: LayoutSnapshot,

    /// Rebuilt every frame by the renderer.
    pub hitboxes: Vec<ButtonHitbox>,
    pub link_hitboxes: Vec<LinkHitbox>,
    pub tag_hitboxes: Vec<TagHitbox>,
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
    command_matches: Vec<AppCommand>,
    terminal: Option<EmbeddedTerminal>,
    terminal_return_overlay: Option<Overlay>,
    terminal_return_dialog: Option<DialogState>,

    active_agent: Option<Observable<crate::agent::AgentRunOutput, AgentEvent>>,
    ai_approval_sender: Option<tokio::sync::mpsc::UnboundedSender<ApprovalDecision>>,
    ai_user_sender: Option<tokio::sync::mpsc::UnboundedSender<AskUserResponse>>,
    agent_worker: AgentWorker,
    agent_input_buffer: Arc<Mutex<Vec<String>>>,
    pub ai_running: bool,
    pub permission_mode: PermissionMode,
    permission_bypass: Arc<AtomicBool>,
    pub agent_panel: Vec<AgentPanelEntry>,
    pub(crate) agent_vlist: AgentVirtualList,
    pub agent_scroll: u16,
    pub agent_usage: TokenUsage,
    pub agent_timed_output_tokens: u64,
    pub agent_response_duration: Duration,
    pub agent_retry_count: u64,
    pub agent_round: u32,
    pub agent_round_limit: u32,
    agent_conversation: AgentConversation,
    pub ai_prompt_input: String,
    pub ai_prompt_cursor: usize,
    ai_source_date: Option<NaiveDate>,
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
    ai_cancelling: bool,

    undo_stack: Vec<UndoOp>,
}

impl App {
    pub fn new(storage: Storage) -> anyhow::Result<Self> {
        let loaded_theme = storage.load_theme(None)?;
        let (
            agent_conversation,
            agent_panel,
            agent_usage,
            agent_timed_output_tokens,
            agent_response_duration,
        ) = storage
            .load_agent_session()?
            .unwrap_or_default()
            .into_parts();
        let agent_scroll = if agent_panel.is_empty() { 0 } else { u16::MAX };
        let daily_notes = storage.load_daily_notes()?;
        let selected = daily_notes.len().saturating_sub(1);
        let mut note_files = storage.list_note_files()?;
        let first_note = note_files.first().map(|file| file.path.clone());
        note_files.extend(storage.list_archived_note_files()?);
        let file_row = usize::from(first_note.is_some());
        let todo_items = storage.load_todo_tasks();
        let images = crate::media::ImageService::new(&storage.root);
        let workspace_index = WorkspaceIndexHandle::default();
        let agent_input_buffer = Arc::new(Mutex::new(Vec::new()));
        let permission_bypass = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (event_sender, _) = tokio::sync::broadcast::channel(AGENT_STREAM_BUFFER);
        let (approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
        let agent_worker = AgentWorker::spawn(
            storage.ai_config_path.clone(),
            storage.root.clone(),
            AgentRuntime::new(
                event_sender,
                approval_receiver,
                user_receiver,
                agent_input_buffer.clone(),
                permission_bypass.clone(),
                cancelled.clone(),
            )
            .with_workspace_index(workspace_index.clone()),
        );
        Ok(Self {
            storage,
            theme: loaded_theme.theme,
            theme_selection: loaded_theme.requested,
            active_theme: loaded_theme.active,
            theme_source: loaded_theme.source,
            images,
            focus: Focus::Center,
            center_view: CenterView::Daily,
            files_context: FilesContext::Browse,
            overlay: None,
            document: None,
            document_render_lru: DocumentRenderLru::default(),
            daily_notes,
            daily_vlist: DailyVirtualList::default(),
            selected,
            scroll: u16::MAX,
            reveal_selected_daily: true,
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
            pending_daily_date: None,
            new_note_from_template: false,
            pending_file: None,
            todo_items,
            todo_index: 0,
            search_query: String::new(),
            search_results: Vec::new(),
            search_index: 0,
            workspace_index,
            pending_tag_rename: None,
            help_scroll: 0,
            status: String::new(),
            animation_tick: 0,
            mouse_captured: true,
            layout: LayoutSnapshot::default(),
            hitboxes: Vec::new(),
            link_hitboxes: Vec::new(),
            tag_hitboxes: Vec::new(),
            file_hitboxes: Vec::new(),
            file_group_hitboxes: Vec::new(),
            todo_hitboxes: Vec::new(),
            search_hitboxes: Vec::new(),
            wiki_link_hitboxes: Vec::new(),
            dialog_hitboxes: Vec::new(),
            dialog: None,
            dialog_result: None,
            command_matches: Vec::new(),
            terminal: None,
            terminal_return_overlay: None,
            terminal_return_dialog: None,
            active_agent: None,
            ai_approval_sender: Some(approval_sender),
            ai_user_sender: Some(user_sender),
            agent_worker,
            agent_input_buffer,
            ai_running: false,
            permission_mode: PermissionMode::Approve,
            permission_bypass,
            agent_panel,
            agent_vlist: AgentVirtualList::default(),
            agent_scroll,
            agent_usage,
            agent_timed_output_tokens,
            agent_response_duration,
            agent_retry_count: 0,
            agent_round: 0,
            agent_round_limit: 0,
            agent_conversation,
            ai_prompt_input: String::new(),
            ai_prompt_cursor: 0,
            ai_source_date: None,
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
            ai_cancelling: false,
            undo_stack: Vec::new(),
        })
    }

    pub fn reload(&mut self) {
        let selected_date = self.selected_date();
        match self.storage.load_daily_notes() {
            Ok(daily_notes) => {
                self.daily_notes = daily_notes;
                self.selected = selected_date
                    .and_then(|date| self.daily_notes.iter().position(|note| note.date == date))
                    .unwrap_or_else(|| self.selected.min(self.daily_notes.len().saturating_sub(1)));
            }
            Err(error) => self.set_error(format!("Reload error: {error}")),
        }
    }

    pub fn advance_animation(&mut self) {
        if self.center_view == CenterView::Daily
            || self.ai_running
            || self.focus == Focus::Compose
            || self.permission_mode == PermissionMode::Bypass
        {
            self.animation_tick = self.animation_tick.wrapping_add(1);
        }
    }

    pub fn reload_files(&mut self) {
        let selected = self.selected_file.clone();
        match self.combined_note_files() {
            Ok(files) => self.note_files = files,
            Err(error) => {
                self.set_error(format!("Reload error: {error}"));
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

    fn apply_loaded_theme(&mut self, loaded: LoadedTheme) {
        let colors_changed = loaded.theme != self.theme;
        self.theme = loaded.theme;
        self.theme_selection = loaded.requested;
        self.active_theme = loaded.active;
        self.theme_source = loaded.source;
        if colors_changed {
            self.document_render_lru = DocumentRenderLru::default();
            if let Some(document) = self.document.as_mut() {
                document.render_cache = None;
            }
            self.daily_vlist = DailyVirtualList::default();
            self.agent_vlist = AgentVirtualList::default();
        }
    }

    /// Reload everything that may have changed while the external editor was running.
    pub fn reload_workspace(&mut self) {
        let previous_random_source = (self.theme_selection == "random")
            .then_some(self.theme_source.as_deref())
            .flatten();
        match self.storage.load_theme(previous_random_source) {
            Ok(loaded) => self.apply_loaded_theme(loaded),
            Err(error) => self.set_error(format!("Theme reload error: {error}")),
        }
        self.reload();
        self.reload_files();
        self.reload_todos();
        if matches!(
            self.center_view,
            CenterView::Search | CenterView::DocumentSearch
        ) {
            self.recompute_search();
        }
        let document_kind = self.document.as_ref().map(|document| document.kind.clone());
        match document_kind {
            Some(DocumentKind::File(path)) => match self.storage.read_document_file(&path) {
                Ok(updated) => {
                    if let Some(document) = self.document.as_mut() {
                        document.replace_source(updated);
                    }
                }
                Err(_) if self.ai_running && !path.exists() => {
                    // The watcher can observe a move before the Agent event
                    // channel reports its destination. Keep the page open
                    // until that mapping arrives or the task finishes.
                }
                Err(error) => {
                    self.document_render_lru
                        .remove(&DocumentKind::File(path.clone()));
                    self.document = None;
                    self.center_view = CenterView::Daily;
                    self.focus = Focus::Center;
                    self.set_error(format!("Reload error: {error}"));
                }
            },
            Some(DocumentKind::Daily(date)) => {
                match self.storage.read_daily_by_date(&date.to_string()) {
                    Ok(updated) => {
                        if let Some(document) = self.document.as_mut() {
                            document.replace_source(updated.body);
                        }
                    }
                    Err(error) => {
                        self.document_render_lru.remove(&DocumentKind::Daily(date));
                        self.document = None;
                        self.center_view = CenterView::Daily;
                        self.focus = Focus::Center;
                        self.set_error(format!("Reload error: {error}"));
                    }
                }
            }
            None => {}
        }
    }

    /// Collect background Agent events without blocking the TUI.
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
                _ => {}
            }
        }

        match self.focus {
            Focus::Compose => self.handle_compose(key),
            Focus::Files => self.handle_files(key),
            Focus::Todo => self.handle_todo(key),
            Focus::Agent => self.handle_agent(key),
            Focus::Center => match self.center_view {
                CenterView::Daily => self.handle_daily(key),
                CenterView::Document => self.handle_document(key),
                CenterView::Search | CenterView::DocumentSearch => self.handle_search(key),
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
                    let text = if purpose == Some(DialogPurpose::CommandPalette) {
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
            (Focus::Compose, CenterView::Daily | CenterView::Document, _) => {
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

    fn move_input_cursor(&mut self, movement: CursorMove) -> Option<Command> {
        self.input_cursor = move_cursor(&self.input, self.input_cursor, movement);
        None
    }

    fn activate_link(&mut self, target: LinkTarget) -> Option<Command> {
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

    fn handle_daily(&mut self, key: KeyEvent) -> Option<Command> {
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
                self.open_todo();
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
                if at_end && !self.agent_panel.is_empty() {
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
            KeyCode::Left | KeyCode::Char('h') => {
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
            KeyCode::Char('C') => self.execute_app_command(AppCommand::ClearAgentSession),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Char('h') => {
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
            KeyCode::Char('c') if self.ai_running => {
                self.execute_app_command(AppCommand::InterruptAgent)
            }
            _ => None,
        }
    }

    fn handle_search(&mut self, key: KeyEvent) -> Option<Command> {
        let document_search = self.center_view == CenterView::DocumentSearch;
        match key.code {
            KeyCode::Esc => {
                self.center_view = if document_search && self.document.is_some() {
                    CenterView::Document
                } else {
                    CenterView::Daily
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
                Some(DocumentKind::File(path)) => Some(Command::Edit(path.clone())),
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
                if self
                    .document
                    .as_ref()
                    .is_some_and(|document| matches!(document.kind, DocumentKind::File(_))) =>
            {
                self.delete_current_note();
                None
            }
            KeyCode::Char('r')
                if self
                    .document
                    .as_ref()
                    .is_some_and(|document| matches!(document.kind, DocumentKind::File(_))) =>
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

    fn route_wheel(&mut self, column: u16, row: u16, delta: i32) {
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
                CenterView::Daily => {
                    self.reveal_selected_daily = false;
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
            && matches!(self.center_view, CenterView::Daily | CenterView::Document)
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

    fn send_message(&mut self) {
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
            None => self.append_to_today(&body, &original_input),
        };
        if let Err(error) = result {
            self.set_error(format!("Error: {error}"));
        }
    }

    fn append_to_open_note(
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

    fn append_to_open_daily(
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

    fn append_to_today(&mut self, body: &str, original_input: &str) -> anyhow::Result<()> {
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

    fn daily_note_clone(&self, date: NaiveDate) -> Option<DailyNote> {
        self.daily_notes
            .iter()
            .find(|note| note.date == date)
            .cloned()
    }

    pub(crate) fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub(crate) fn set_error(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.notifications.notify(error.clone());
        self.status = error;
    }

    fn record_undo(&mut self, operation: UndoOp) {
        const CAPACITY: usize = 50;
        if self.undo_stack.len() == CAPACITY {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(operation);
    }

    fn recall_last_append(&mut self) {
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

    fn restore_recalled_input(&mut self, recalled: String) {
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

    fn undo(&mut self) {
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
