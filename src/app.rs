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
mod document;
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

    fn open_command_palette(&mut self) {
        let dialog = DialogState::new(
            "Command Palette · Ctrl+P",
            String::new(),
            DialogMode::CommandPalette,
            DialogPurpose::CommandPalette,
            Vec::new(),
        );
        self.open_dialog(dialog);
        self.refresh_command_palette();
    }

    fn open_theme_picker(&mut self) {
        let names = match self.storage.list_theme_names() {
            Ok(names) => names,
            Err(error) => {
                self.set_error(format!("Theme list error: {error}"));
                return;
            }
        };
        let mut options = vec![
            DialogOption::with_hint("default", "themes/default.toml"),
            DialogOption::with_hint("random", "Choose a custom theme at random"),
        ];
        options.extend(names.into_iter().map(|name| {
            let hint = if name == self.active_theme {
                "Custom theme · active"
            } else {
                "Custom theme"
            };
            DialogOption::with_hint(name, hint)
        }));
        let selected = options
            .iter()
            .position(|option| option.label == self.theme_selection)
            .unwrap_or(0);
        let mut dialog = DialogState::new(
            "Theme · Enter apply",
            format!("Active: {}", self.active_theme),
            DialogMode::SingleSelect,
            DialogPurpose::ThemePicker,
            options,
        );
        dialog.selected = selected;
        self.open_dialog(dialog);
    }

    fn open_tag_picker(&mut self) {
        let Some(tags) = self.workspace_index.with_index(WorkspaceIndex::tags) else {
            self.set_status("Tag index is still building");
            return;
        };
        let options = tags
            .into_iter()
            .map(|tag| {
                DialogOption::with_hint(
                    format!("#{}", tag.name),
                    format!("{} documents · {} mentions", tag.documents, tag.mentions),
                )
            })
            .collect::<Vec<_>>();
        if options.is_empty() {
            self.set_status("No tags found");
            return;
        }
        self.open_dialog(DialogState::new(
            "Tags · Enter search",
            String::new(),
            DialogMode::SingleSelect,
            DialogPurpose::TagPicker,
            options,
        ));
    }

    fn open_tag_rename_picker(&mut self) {
        let Some(tags) = self.workspace_index.with_index(WorkspaceIndex::tags) else {
            self.set_status("Tag index is still building");
            return;
        };
        let options = tags
            .into_iter()
            .map(|tag| {
                DialogOption::with_hint(
                    format!("#{}", tag.name),
                    format!("{} documents · {} mentions", tag.documents, tag.mentions),
                )
            })
            .collect::<Vec<_>>();
        if options.is_empty() {
            self.set_status("No tags found");
            return;
        }
        self.open_dialog(DialogState::new(
            "Rename tag · Select source",
            String::new(),
            DialogMode::SingleSelect,
            DialogPurpose::TagRenameSource,
            options,
        ));
    }

    fn refresh_command_palette(&mut self) {
        let query = self
            .dialog
            .as_ref()
            .map(|dialog| dialog.input.trim().to_string())
            .unwrap_or_default();
        let mut matches = APP_COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, command)| self.command_available(command.id))
            .filter_map(|(index, command)| {
                command_match_score(command, &query).map(|score| (score, index, command.id))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(score, index, _)| (*score, *index));
        self.command_matches = matches.into_iter().map(|(_, _, id)| id).collect();
        let options = self
            .command_matches
            .iter()
            .filter_map(|id| {
                let command = command_definition(*id)?;
                let (label, description) = if *id == AppCommand::ToggleMouseSupport {
                    if self.mouse_captured {
                        (
                            "Interface: Disable mouse support",
                            "Disable mouse support to select and copy text with the terminal",
                        )
                    } else {
                        (
                            "Interface: Enable mouse support",
                            "Restore mouse clicking and scrolling",
                        )
                    }
                } else {
                    (command.label, command.description)
                };
                Some(DialogOption::with_hint(label, description))
            })
            .collect::<Vec<_>>();
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.options = options;
            dialog.checked = vec![false; dialog.options.len()];
            dialog.selected = dialog.selected.min(dialog.options.len().saturating_sub(1));
        }
    }

    fn execute_selected_palette_command(&mut self) -> Option<Command> {
        let selected = self.dialog_selected();
        let Some(command) = self.command_matches.get(selected).copied() else {
            self.set_status("No matching command");
            return None;
        };
        self.close_dialog();
        self.command_matches.clear();
        self.execute_app_command(command)
    }

    fn execute_app_command(&mut self, command: AppCommand) -> Option<Command> {
        match command {
            AppCommand::InterruptAgent => self.cancel_agent(),
            AppCommand::ClearAgentSession => self.clear_agent_session(),
            AppCommand::OpenTerminal => self.toggle_terminal(),
            AppCommand::ToggleMouseSupport => {
                self.mouse_captured = !self.mouse_captured;
                self.set_status(if self.mouse_captured {
                    "Mouse support enabled"
                } else {
                    "Mouse support disabled; terminal text selection available"
                });
                return Some(Command::SetMouseCapture(self.mouse_captured));
            }
            AppCommand::NewNote => self.begin_new_note(),
            AppCommand::NewNoteFromTemplate => self.begin_new_note_from_template(),
            AppCommand::EditTemplate => {
                return Some(Command::Edit(self.storage.template_path.clone()));
            }
            AppCommand::EditCurrentNote => {
                return self.current_note_path().map(Command::Edit);
            }
            AppCommand::RenameCurrentNote => self.rename_current_note(),
            AppCommand::DeleteCurrentNote => self.delete_current_note(),
            AppCommand::ArchiveCurrentNote => self.manage_current_note(false),
            AppCommand::RestoreCurrentNote => self.manage_current_note(true),
            AppCommand::EditAiConfig => {
                return Some(Command::Edit(self.storage.ai_config_path.clone()))
            }
            AppCommand::SwitchTheme => self.open_theme_picker(),
            AppCommand::BrowseTags => self.open_tag_picker(),
            AppCommand::RenameTag => self.open_tag_rename_picker(),
            AppCommand::EditAgentInstructions => {
                return Some(Command::Edit(self.storage.agents_path.clone()));
            }
            AppCommand::EditAgentMemory => {
                return Some(Command::Edit(self.storage.memory_path.clone()));
            }
        }
        None
    }

    fn command_available(&self, command: AppCommand) -> bool {
        match command {
            AppCommand::InterruptAgent
            | AppCommand::ClearAgentSession
            | AppCommand::OpenTerminal
            | AppCommand::ToggleMouseSupport
            | AppCommand::NewNote
            | AppCommand::NewNoteFromTemplate
            | AppCommand::EditTemplate => true,
            AppCommand::EditCurrentNote
            | AppCommand::RenameCurrentNote
            | AppCommand::DeleteCurrentNote => self.current_note_path().is_some(),
            AppCommand::ArchiveCurrentNote => self.current_note_archived() == Some(false),
            AppCommand::RestoreCurrentNote => self.current_note_archived() == Some(true),
            AppCommand::EditAiConfig
            | AppCommand::SwitchTheme
            | AppCommand::BrowseTags
            | AppCommand::RenameTag
            | AppCommand::EditAgentInstructions
            | AppCommand::EditAgentMemory => true,
        }
    }

    fn current_note_path(&self) -> Option<PathBuf> {
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

    fn rename_current_note(&mut self) {
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

    fn delete_current_note(&mut self) {
        let Some(path) = self.current_note_path() else {
            self.set_status("No note is open");
            return;
        };
        self.pending_file = Some(path);
        self.set_overlay(Overlay::ConfirmDeleteFile);
    }

    fn manage_current_note(&mut self, restore: bool) {
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
    pub fn open_dialog(&mut self, dialog: DialogState) {
        if self.overlay == Some(Overlay::Terminal) {
            self.discard_terminal_return_overlay();
        }
        self.dialog_result = None;
        self.dialog = Some(dialog);
        self.overlay = Some(Overlay::Dialog);
    }

    #[allow(dead_code)]
    pub fn take_dialog_result(&mut self) -> Option<DialogResult> {
        self.dialog_result.take()
    }

    pub(crate) fn set_overlay(&mut self, overlay: Overlay) {
        if self.overlay == Some(Overlay::Terminal) && overlay != Overlay::Terminal {
            self.discard_terminal_return_overlay();
        }
        self.overlay = Some(overlay);
        self.dialog = if overlay == Overlay::Terminal {
            None
        } else {
            Some(self.dialog_for_overlay(overlay))
        };
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
        if self.overlay == Some(Overlay::Terminal) {
            return;
        }
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
            Overlay::ConfirmDeleteDaily => DialogState::new(
                "Delete daily note",
                "Delete this daily note?",
                DialogMode::Confirm,
                DialogPurpose::DeleteDaily,
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
                let round_limit =
                    request.is_some_and(|request| request.kind == AskUserKind::RoundLimit);
                let mut dialog = DialogState::new(
                    if round_limit {
                        "Agent round limit"
                    } else {
                        "Agent question"
                    },
                    request
                        .map(|request| request.question.clone())
                        .unwrap_or_default(),
                    if round_limit {
                        DialogMode::SingleSelect
                    } else {
                        DialogMode::SelectOrInput
                    },
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
            Overlay::Terminal => unreachable!("terminal overlay does not use a dialog"),
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

    fn begin_new_note(&mut self) {
        self.begin_new_note_with_template(false);
    }

    fn begin_new_note_from_template(&mut self) {
        self.begin_new_note_with_template(true);
    }

    fn begin_new_note_with_template(&mut self, from_template: bool) {
        self.pending_daily_date = None;
        self.new_note_from_template = from_template;
        self.new_file_input.clear();
        self.new_file_cursor = 0;
        self.files_context = FilesContext::NewTarget;
        self.focus = Focus::Files;
        self.open_file_name_dialog(DialogPurpose::NewFile);
    }

    fn handle_new_target(&mut self, key: KeyEvent) -> Option<Command> {
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

    fn handle_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        self.handle_dialog_key(key)
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) -> Option<Command> {
        let Some(dialog) = self.dialog.clone() else {
            self.overlay = None;
            return None;
        };
        match dialog.purpose {
            DialogPurpose::DeleteDaily => {
                return match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.handle_delete_daily_overlay(key)
                    }
                    _ => self.handle_delete_daily_overlay(key),
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
            DialogPurpose::CommandPalette => return self.handle_command_palette(key),
            DialogPurpose::ThemePicker => return self.handle_theme_picker(key),
            DialogPurpose::TagPicker => return self.handle_tag_picker(key),
            DialogPurpose::TagRenameSource => return self.handle_tag_rename_source(key),
            DialogPurpose::AgentPrompt
            | DialogPurpose::NewFile
            | DialogPurpose::RenameFile
            | DialogPurpose::TagRenameTarget => return self.handle_text_dialog(key),
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
            DialogMode::CommandPalette => return self.handle_command_palette(key),
            DialogMode::Approval | DialogMode::Informational => {}
        }
        None
    }

    fn handle_theme_picker(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_dialog(),
            KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
            KeyCode::Enter => {
                let selection = self
                    .dialog
                    .as_ref()
                    .and_then(DialogState::selected_option)
                    .map(|option| option.label.clone());
                let selection = selection?;
                match self.storage.select_theme(&selection) {
                    Ok(loaded) => {
                        let active = loaded.active.clone();
                        self.apply_loaded_theme(loaded);
                        self.set_status(if selection == active {
                            format!("Theme: {active}")
                        } else {
                            format!("Theme: {active} ({selection})")
                        });
                        self.close_dialog();
                    }
                    Err(error) => self.set_error(format!("Theme switch error: {error}")),
                }
            }
            _ => {}
        }
        None
    }

    fn handle_tag_picker(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_dialog(),
            KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
            KeyCode::Enter => {
                let tag = self
                    .dialog
                    .as_ref()
                    .and_then(DialogState::selected_option)
                    .map(|option| option.label.clone())?;
                self.close_dialog();
                self.search_query = tag;
                self.search_index = 0;
                self.center_view = CenterView::Search;
                self.focus = Focus::Center;
                self.recompute_search();
            }
            _ => {}
        }
        None
    }

    fn handle_tag_rename_source(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_dialog(),
            KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
            KeyCode::Enter => {
                let source = self
                    .dialog
                    .as_ref()
                    .and_then(DialogState::selected_option)
                    .map(|option| option.label.trim_start_matches('#').to_string())?;
                self.pending_tag_rename = Some(source.clone());
                self.open_dialog(DialogState::new(
                    format!("Rename #{source}"),
                    "Enter the new tag name",
                    DialogMode::FreeText,
                    DialogPurpose::TagRenameTarget,
                    Vec::new(),
                ));
            }
            _ => {}
        }
        None
    }

    fn submit_tag_rename(&mut self) {
        let Some(from) = self.pending_tag_rename.clone() else {
            self.set_status("No source tag selected");
            return;
        };
        let to = self
            .dialog
            .as_ref()
            .map(|dialog| dialog.input.clone())
            .unwrap_or_default();
        let Some(paths) = self
            .workspace_index
            .with_index(|index| index.tag_paths(&from))
        else {
            self.set_status("Tag index is still building");
            return;
        };
        let plan = match TagRenamePlan::prepare(&self.storage, paths, &from, &to) {
            Ok(plan) => plan,
            Err(error) => {
                self.set_error(format!("Tag rename error: {error}"));
                return;
            }
        };
        match plan.apply() {
            Ok(outcome) => {
                self.workspace_index
                    .refresh_paths(&self.storage, outcome.paths.clone());
                self.pending_tag_rename = None;
                self.close_dialog();
                self.reload_workspace();
                self.set_status(format!(
                    "Renamed #{} to #{} in {} documents ({} mentions)",
                    outcome.from, outcome.to, outcome.documents, outcome.mentions
                ));
            }
            Err(error) => self.set_error(format!("Tag rename error: {error}")),
        }
    }

    fn handle_command_palette(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.close_dialog();
                self.command_matches.clear();
            }
            KeyCode::Up => self.move_dialog_selection(-1),
            KeyCode::Down => self.move_dialog_selection(1),
            KeyCode::Enter => return self.execute_selected_palette_command(),
            KeyCode::Backspace => {
                self.delete_dialog_backward();
                self.refresh_command_palette();
            }
            KeyCode::Delete => {
                self.delete_dialog_forward();
                self.refresh_command_palette();
            }
            KeyCode::Left => self.move_dialog_cursor(CursorMove::Left),
            KeyCode::Right => self.move_dialog_cursor(CursorMove::Right),
            KeyCode::Home => self.move_dialog_cursor(CursorMove::LineStart),
            KeyCode::End => self.move_dialog_cursor(CursorMove::LineEnd),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_dialog_char(character);
                self.refresh_command_palette();
            }
            _ => {}
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
        if self
            .ask_user_request
            .as_ref()
            .is_some_and(|request| request.kind == AskUserKind::RoundLimit)
        {
            match key.code {
                KeyCode::Esc => {
                    let _ = self.send_user_response(AskUserResponse::Answer("Stop".to_string()));
                }
                KeyCode::Up => self.move_dialog_selection(-1),
                KeyCode::Down => self.move_dialog_selection(1),
                KeyCode::Enter => {
                    if let Some(answer) = self
                        .dialog
                        .as_ref()
                        .and_then(DialogState::selected_option)
                        .map(|option| option.label.clone())
                    {
                        let _ = self.send_user_response(AskUserResponse::Answer(answer));
                    }
                }
                _ => {}
            }
            return None;
        }
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
                self.sync_dialog_owner_state();
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
                    Some(DialogPurpose::TagRenameTarget) => self.submit_tag_rename(),
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
                    Some(DialogPurpose::AgentPrompt) => self.ai_source_date = None,
                    Some(DialogPurpose::NewFile) => {
                        self.pending_daily_date = None;
                        self.files_context = FilesContext::Browse;
                    }
                    Some(DialogPurpose::RenameFile) => {
                        self.pending_file = None;
                        self.files_context = FilesContext::Browse;
                    }
                    Some(DialogPurpose::TagRenameTarget) => {
                        self.pending_tag_rename = None;
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
        self.sync_dialog_owner_state();
    }

    fn delete_dialog_backward(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        delete_backward(&mut dialog.input, &mut dialog.cursor);
        self.sync_dialog_owner_state();
    }

    fn delete_dialog_forward(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        delete_forward(&mut dialog.input, &mut dialog.cursor);
        self.sync_dialog_owner_state();
    }

    fn move_dialog_cursor(&mut self, movement: CursorMove) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        dialog.cursor = move_cursor(&dialog.input, dialog.cursor, movement);
        self.sync_dialog_owner_state();
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

    fn sync_dialog_owner_state(&mut self) {
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

    fn handle_delete_daily_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(date) = self.pending_daily_date.take() {
                    let note = self.daily_note_clone(date);
                    match self.storage.remove_daily(&date.to_string()) {
                        Ok(true) => {
                            if let Some(note) = note {
                                self.record_undo(UndoOp::Delete(note));
                            }
                            self.set_status("Deleted");
                            self.reload();
                            self.reload_todos();
                        }
                        Ok(false) => self.set_status("Daily note not found"),
                        Err(error) => self.set_error(format!("Error: {error}")),
                    }
                }
                self.overlay = None;
                self.dialog = None;
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_daily_date = None;
                self.overlay = None;
                self.dialog = None;
                None
            }
            _ => None,
        }
    }

    fn handle_delete_file_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
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
                            self.document_render_lru
                                .remove(&DocumentKind::File(path.clone()));
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
                                self.center_view = CenterView::Daily;
                            }
                            self.reload_files();
                        }
                        Err(error) => self.set_error(format!("Error: {error}")),
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

    fn move_daily_selection(&mut self, delta: i32) {
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
            Err(error) => self.set_error(format!("Error: {error}")),
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

    fn open_tag_search(&mut self, name: &str) {
        self.close_dialog();
        self.search_query = format!("#{name}");
        self.search_index = 0;
        self.center_view = CenterView::Search;
        self.focus = Focus::Center;
        self.recompute_search();
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
            Err(error) => self.set_error(format!("Error: {error}")),
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

    fn open_file_document(&mut self, path: &Path, return_to: DocumentReturn) {
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

    fn sync_file_tree_to_note(&mut self, path: &Path) {
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

    fn open_daily_document(&mut self, date: NaiveDate, return_to: DocumentReturn) {
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

    fn show_document(
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

    fn stash_current_document(&mut self) {
        let Some(mut document) = self.document.take() else {
            return;
        };
        if let Some(render) = document.render_cache.take() {
            self.document_render_lru
                .insert(document.kind, document.source, render);
        }
    }

    fn close_document(&mut self) {
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

    fn act(&mut self, action: Action) -> Option<Command> {
        let date = self.selected_date()?;
        self.dispatch_action(date, action)
    }

    fn dispatch_action(&mut self, date: NaiveDate, action: Action) -> Option<Command> {
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

    fn daily_edit_command(&self, date: NaiveDate) -> Option<Command> {
        self.storage
            .daily_file_path(&date.to_string())
            .ok()
            .filter(|path| path.is_file())
            .map(Command::Edit)
    }

    fn cancel_file_context(&mut self) {
        self.pending_daily_date = None;
        self.new_note_from_template = false;
        self.pending_file = None;
        self.files_context = FilesContext::Browse;
        self.focus = Focus::Center;
    }

    fn perform_move_to(&mut self, path: &Path) {
        let Some(date) = self.pending_daily_date else {
            self.cancel_file_context();
            return;
        };
        self.perform_move_to_date(path, date);
    }

    fn perform_move_to_date(&mut self, path: &Path, date: NaiveDate) {
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
