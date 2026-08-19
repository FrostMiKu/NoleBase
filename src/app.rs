//! Application state and event handling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::agent::{
    AgentEvent, AgentRuntime, AgentStopReason, AgentWorker, ApprovalDecision, ApprovalKind,
    ApprovalRequest, AskUserKind, AskUserRequest, AskUserResponse, PermissionMode,
    AGENT_STREAM_BUFFER,
};
use crate::agent_session::{AgentConversation, AgentPanelEntry, AgentSession, TokenUsage};
use crate::attachment::{AttachmentId, AttachmentStore};
use crate::attachment_usage::AttachmentUsageHandle;
use crate::embedded_terminal::{is_terminal_toggle, EmbeddedTerminal, TerminalSnapshot};
use crate::export::ExportFormat;
use crate::model::{
    Action, AttachmentHitbox, BacklinkHitbox, ButtonHitbox, DailyNote, DialogOptionHitbox,
    FileGroup, FileGroupHitbox, FileHitbox, FileListRow, LinkHitbox, LinkTarget, NoteFile,
    SearchHit, SearchHitbox, TagHitbox, TagNote, TagNoteHitbox, TodoHitbox, TodoItem,
    WikiLinkCandidate, WikiLinkHitbox, WikiLinkLocation, WorkspaceViewHitbox,
};
use crate::notification::NotificationService;
use crate::observable::Observable;
use crate::skill::Skill;
use crate::storage::{ExportOutcome, LoadedTheme, Storage};
use crate::workspace_index::{
    TagDocument, TagRenamePlan, TagSummary, WorkspaceIndex, WorkspaceIndexHandle,
};

pub(in crate::app) const FORMAT_DAILY_NOTE_PROMPT: &str = "Read this daily note, then edit it in place to improve its Markdown formatting and readability. Preserve every fact, idea, task, link, and the author's meaning. Only improve structure and presentation, such as headings, paragraphs, lists, spacing, and emphasis. Do not add new factual content, and do not merely describe the changes.";

mod agent;
mod dialog;
mod dialogs;
mod document;
mod documents;
mod input;
mod model;
#[cfg(test)]
mod skill_tests;
mod terminal;
#[cfg(test)]
mod tests;
mod text_input;
mod vlist;

pub(crate) use self::documents::human_size;
pub(in crate::app) use self::text_input::{edit_single_line, TextInputEdit};
pub(crate) use self::vlist::*;
pub use self::{dialog::*, document::*, model::*};

pub(in crate::app) const DAILY_PAGE_STEP: u16 = 5;
pub(in crate::app) const AGENT_PAGE_STEP: u16 = 8;
pub(in crate::app) const DOCUMENT_PAGE_STEP: u16 = 10;
pub(in crate::app) const DIALOG_PAGE_STEP: i32 = 8;
pub(crate) const CODE_COPY_FEEDBACK_TTL: Duration = Duration::from_secs(2);

/// Move a selection index by `delta` within `[0, len)`.
/// Clamps on both ends; an empty list keeps the index at zero.
pub(in crate::app) fn move_index(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let current = current.min(len - 1);
    (current as i32 + delta).clamp(0, len as i32 - 1) as usize
}

pub(in crate::app) fn is_up_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Up | KeyCode::Char('k'))
}

pub(in crate::app) fn is_down_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Down | KeyCode::Char('j'))
}

pub(in crate::app) fn is_left_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Left | KeyCode::Char('h'))
}

pub(in crate::app) fn is_right_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Right | KeyCode::Char('l'))
}

/// `Esc` or `q` — the two keys that leave the current context.
pub(in crate::app) fn is_cancel_key(code: KeyCode) -> bool {
    matches!(code, KeyCode::Esc | KeyCode::Char('q'))
}

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
    pub file_list_start: usize,
    pub notes_expanded: bool,
    pub archives_expanded: bool,
    pub file_query: String,
    pub file_query_cursor: usize,
    pub rename_input: String,
    pub rename_cursor: usize,
    pub new_file_input: String,
    pub new_file_cursor: usize,

    /// Daily note being moved, filed, or deleted by a contextual interaction.
    pub pending_daily_date: Option<NaiveDate>,
    new_note_from_template: bool,
    /// File awaiting rename or delete confirmation.
    pub pending_file: Option<PathBuf>,
    pub pending_export_source: Option<PathBuf>,
    pub pending_export_format: Option<ExportFormat>,
    /// Destination input the user submitted; kept across the background job
    /// so a failed export can restore the dialog for a direct retry.
    pub pending_export_destination: Option<String>,
    export_job: Option<mpsc::Receiver<ExportJobResult>>,
    export_job_format: Option<ExportFormat>,
    pub export_in_progress: bool,

    pub todo_items: Vec<TodoItem>,
    pub todo_query: String,
    pub todo_cursor: usize,
    pub todo_index: usize,
    pub todo_list_start: usize,
    pub workspace_view_index: usize,

    pub search_query: String,
    pub search_cursor: usize,
    pub search_results: Vec<SearchHit>,
    pub search_index: usize,
    pub search_list_start: usize,
    pub tag_query: String,
    pub tag_cursor: usize,
    pub tag_results: Vec<TagSummary>,
    pub tag_index: usize,
    pub tag_list_start: usize,
    tags_return_view: CenterView,
    /// The exact tag whose full-body card stream fills the Tags view. `None`
    /// shows the tag picker; `Some` shows its chronological card stream.
    pub active_tag: Option<String>,
    /// Distinct managed notes containing `active_tag`, oldest-first.
    pub tag_notes: Vec<TagNote>,
    pub tag_note_index: usize,
    pub tag_note_scroll: u16,
    pub(crate) reveal_selected_tag_note: bool,
    pub(crate) tag_note_vlist: TagNoteVirtualList,
    /// Rebuilt every frame by the renderer: one entry per visible card.
    pub tag_note_hitboxes: Vec<TagNoteHitbox>,
    workspace_index: WorkspaceIndexHandle,
    pending_tag_rename: Option<String>,

    /// The latest published wiki-link index, used to resolve backlinks for the
    /// open document and shared with agent wiki-link tools. Publish via
    /// [`App::apply_wiki_link_index`].
    wiki_links: crate::wiki_link_index::WikiLinkIndexHandle,
    /// Distinct managed notes linking to the currently open document.
    pub document_backlinks: Vec<PathBuf>,

    pub attachment_store: AttachmentStore,
    pub attachment_usage: AttachmentUsageHandle,
    pub attachment_entries: Vec<AttachmentEntry>,
    pub attachment_index: usize,
    pub attachment_list_start: usize,
    pub attachment_query: String,
    pub attachment_cursor: usize,
    /// Attachment awaiting trash confirmation, with the usage-index revision
    /// its "unreferenced" decision was based on.
    pending_attachment: Option<(AttachmentId, u64)>,
    pub skill_entries: Vec<Skill>,
    pub skill_index: usize,
    skill_browser_return: Option<SkillBrowserReturn>,

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
    pub workspace_view_hitboxes: Vec<WorkspaceViewHitbox>,
    pub search_hitboxes: Vec<SearchHitbox>,
    pub attachment_hitboxes: Vec<AttachmentHitbox>,
    pub backlink_hitboxes: Vec<BacklinkHitbox>,
    pub wiki_link_hitboxes: Vec<WikiLinkHitbox>,
    pub dialog_hitboxes: Vec<DialogOptionHitbox>,
    code_copy_pending_area: Option<Rect>,
    code_copy_feedback: Option<CodeCopyFeedback>,

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
    permission_mode_atomic: Arc<AtomicU8>,
    pub agent_panel: Vec<Arc<AgentPanelEntry>>,
    active_agent_tools: HashMap<String, usize>,
    pub(crate) agent_vlist: AgentVirtualList,
    pub agent_scroll: u16,
    pub(crate) agent_follow_tail: bool,
    pub(crate) show_full_thinking: bool,
    pub agent_usage: TokenUsage,
    pub agent_context_window: u64,
    pub agent_context_capacity: u64,
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

#[derive(Clone)]
struct SkillBrowserReturn {
    center_view: CenterView,
    focus: Focus,
    document: Option<Document>,
}

struct ExportJobResult {
    format: ExportFormat,
    outcome: Result<ExportOutcome, String>,
}

#[derive(Clone)]
struct CodeCopyFeedback {
    source: String,
    area: Rect,
    expires_at: Instant,
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
        let agent_follow_tail = !agent_panel.is_empty();
        let agent_scroll = 0;
        let agent_panel = agent_panel.into_iter().map(Arc::new).collect();
        let daily_notes = storage.load_daily_notes()?;
        let selected = daily_notes.len().saturating_sub(1);
        let mut note_files = storage.list_note_files()?;
        let first_note = note_files.first().map(|file| file.path.clone());
        note_files.extend(storage.list_archived_note_files()?);
        let file_row = usize::from(first_note.is_some());
        let todo_items = storage.load_todo_tasks();
        let images = crate::media::ImageService::new(&storage.root);
        let attachment_store = AttachmentStore::new(storage.attachments_dir.clone());
        let attachment_usage = AttachmentUsageHandle::new();
        let workspace_index = WorkspaceIndexHandle::default();
        let wiki_links = crate::wiki_link_index::WikiLinkIndexHandle::default();
        let agent_input_buffer = Arc::new(Mutex::new(Vec::new()));
        let permission_mode_atomic = Arc::new(AtomicU8::new(PermissionMode::Approve.code()));
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
                permission_mode_atomic.clone(),
                cancelled.clone(),
            )
            .with_workspace_index(workspace_index.clone())
            .with_wiki_link_index(wiki_links.clone()),
            attachment_usage.clone(),
        );
        let show_full_thinking = storage.show_full_thinking().unwrap_or(false);
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
            file_list_start: 0,
            notes_expanded: true,
            archives_expanded: false,
            file_query: String::new(),
            file_query_cursor: 0,
            rename_input: String::new(),
            rename_cursor: 0,
            new_file_input: String::new(),
            new_file_cursor: 0,
            pending_daily_date: None,
            new_note_from_template: false,
            pending_file: None,
            pending_export_source: None,
            pending_export_format: None,
            pending_export_destination: None,
            export_job: None,
            export_job_format: None,
            export_in_progress: false,
            todo_items,
            todo_query: String::new(),
            todo_cursor: 0,
            todo_index: 0,
            todo_list_start: 0,
            workspace_view_index: WorkspaceView::index_of(CenterView::Daily)
                .expect("Daily is a registered workspace view"),
            search_query: String::new(),
            search_cursor: 0,
            search_results: Vec::new(),
            search_index: 0,
            search_list_start: 0,
            tag_query: String::new(),
            tag_cursor: 0,
            tag_results: Vec::new(),
            tag_index: 0,
            tag_list_start: 0,
            tags_return_view: CenterView::Daily,
            active_tag: None,
            tag_notes: Vec::new(),
            tag_note_index: 0,
            tag_note_scroll: 0,
            reveal_selected_tag_note: false,
            tag_note_vlist: TagNoteVirtualList::default(),
            tag_note_hitboxes: Vec::new(),
            workspace_index,
            pending_tag_rename: None,
            wiki_links,
            document_backlinks: Vec::new(),
            attachment_store,
            attachment_usage,
            attachment_entries: Vec::new(),
            attachment_index: 0,
            attachment_list_start: 0,
            attachment_query: String::new(),
            attachment_cursor: 0,
            pending_attachment: None,
            skill_entries: Vec::new(),
            skill_index: 0,
            skill_browser_return: None,
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
            workspace_view_hitboxes: Vec::new(),
            search_hitboxes: Vec::new(),
            attachment_hitboxes: Vec::new(),
            backlink_hitboxes: Vec::new(),
            wiki_link_hitboxes: Vec::new(),
            dialog_hitboxes: Vec::new(),
            code_copy_pending_area: None,
            code_copy_feedback: None,
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
            permission_mode_atomic,
            agent_panel,
            active_agent_tools: HashMap::new(),
            agent_vlist: AgentVirtualList::default(),
            agent_scroll,
            agent_follow_tail,
            show_full_thinking,
            agent_usage,
            agent_context_window: 0,
            agent_context_capacity: 0,
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

    pub(crate) fn begin_code_copy(&mut self, area: Rect) {
        self.code_copy_pending_area = Some(area);
    }

    pub(crate) fn complete_code_copy(&mut self, source: &str, now: Instant) {
        let Some(area) = self.code_copy_pending_area.take() else {
            return;
        };
        self.code_copy_feedback = Some(CodeCopyFeedback {
            source: source.to_string(),
            area,
            expires_at: now + CODE_COPY_FEEDBACK_TTL,
        });
    }

    pub(crate) fn cancel_code_copy(&mut self) {
        self.code_copy_pending_area = None;
    }

    pub(crate) fn has_code_copy_feedback(&self) -> bool {
        self.code_copy_feedback.is_some()
    }

    pub(crate) fn visible_code_copy_feedback(&mut self, now: Instant) -> Option<Rect> {
        if self
            .code_copy_feedback
            .as_ref()
            .is_some_and(|feedback| now >= feedback.expires_at)
        {
            self.code_copy_feedback = None;
            return None;
        }
        let feedback = self.code_copy_feedback.as_ref()?;
        self.link_hitboxes
            .iter()
            .any(|hitbox| {
                hitbox.area == feedback.area
                    && matches!(
                        &hitbox.target,
                        LinkTarget::CopyCode(source) if source == &feedback.source
                    )
            })
            .then_some(feedback.area)
    }

    pub(super) fn reload_thinking_display_config(&mut self) {
        let Ok(show_full_thinking) = self.storage.show_full_thinking() else {
            return;
        };
        if self.show_full_thinking != show_full_thinking {
            self.show_full_thinking = show_full_thinking;
            self.agent_vlist = AgentVirtualList::default();
        }
    }

    pub fn reload(&mut self) {
        self.reload_thinking_display_config();
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
        self.ensure_visible_todo_selection();
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
        } else if self.center_view == CenterView::Tags {
            self.recompute_tags();
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
                    self.document_backlinks.clear();
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
                        self.document_backlinks.clear();
                        self.center_view = CenterView::Daily;
                        self.focus = Focus::Center;
                        self.set_error(format!("Reload error: {error}"));
                    }
                }
            }
            Some(DocumentKind::Skill(path)) => match self.storage.read_skill(&path) {
                Ok(updated) => {
                    if let Some(document) = self.document.as_mut() {
                        document.title = updated.id;
                        document.replace_source(updated.body);
                    }
                }
                Err(error) => {
                    self.document_render_lru
                        .remove(&DocumentKind::Skill(path.clone()));
                    self.document = None;
                    self.document_backlinks.clear();
                    self.return_to_skill_browser();
                    self.set_error(format!("Skill reload error: {error}"));
                }
            },
            None => {}
        }
    }

    /// Collect background Agent events without blocking the TUI.
    pub(crate) fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub(crate) fn set_error(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.notifications.notify(error.clone());
        self.status = error;
    }
}
