//! Application state and event handling.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::agent::{
    Agent, AgentEvent, AgentRuntime, ApprovalDecision, ApprovalRequest, AskUserKind,
    AskUserRequest, AskUserResponse, PermissionMode,
};
use crate::agent_session::{AgentConversation, AgentPanelEntry, AgentSession, TokenUsage};
use crate::embedded_terminal::{is_terminal_toggle, EmbeddedTerminal, TerminalSnapshot};
use crate::model::{
    Action, ButtonHitbox, DailyNote, DialogOptionHitbox, FileGroup, FileGroupHitbox, FileHitbox,
    FileListRow, LinkHitbox, LinkTarget, NoteFile, SearchHit, SearchHitbox, TagHitbox, TodoHitbox,
    TodoItem, WikiLinkCandidate, WikiLinkHitbox,
};
use crate::notification::NotificationService;
use crate::storage::{LoadedTheme, Storage};
use crate::workspace_index::{TagRenamePlan, WorkspaceIndex, WorkspaceIndexHandle};

const FORMAT_DAILY_NOTE_PROMPT: &str = "Read this daily note, then edit it in place to improve its Markdown formatting and readability. Preserve every fact, idea, task, link, and the author's meaning. Only improve structure and presentation, such as headings, paragraphs, lists, spacing, and emphasis. Do not add new factual content, and do not merely describe the changes.";

fn point_in_rect(col: u16, row: u16, area: Rect) -> bool {
    col >= area.x
        && col < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn agent_debug_logging_enabled() -> bool {
    std::env::var("NOLE_DEBUG").is_ok_and(|value| value == "1")
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
    OpenLink(String),
    OpenPath(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppCommand {
    InterruptAgent,
    ClearAgentSession,
    OpenTerminal,
    NewNote,
    NewNoteFromTemplate,
    EditTemplate,
    EditCurrentNote,
    RenameCurrentNote,
    DeleteCurrentNote,
    ArchiveCurrentNote,
    RestoreCurrentNote,
    EditAiConfig,
    SwitchTheme,
    BrowseTags,
    RenameTag,
    EditAgentInstructions,
    EditAgentMemory,
}

struct AppCommandDefinition {
    id: AppCommand,
    label: &'static str,
    description: &'static str,
    keywords: &'static str,
}

const APP_COMMANDS: &[AppCommandDefinition] = &[
    AppCommandDefinition {
        id: AppCommand::InterruptAgent,
        label: "Agent: Interrupt task",
        description: "Stop the active Agent task",
        keywords: "agent interrupt cancel stop task",
    },
    AppCommandDefinition {
        id: AppCommand::ClearAgentSession,
        label: "Agent: Clear session",
        description: "Delete the saved conversation and panel history",
        keywords: "agent clear reset new session context conversation",
    },
    AppCommandDefinition {
        id: AppCommand::OpenTerminal,
        label: "Terminal: Open",
        description: "Open the workspace terminal",
        keywords: "terminal shell console pty open toggle workspace",
    },
    AppCommandDefinition {
        id: AppCommand::NewNote,
        label: "Note: New",
        description: "Create and open a new note",
        keywords: "note new create file blank title",
    },
    AppCommandDefinition {
        id: AppCommand::NewNoteFromTemplate,
        label: "Note: New from template",
        description: "Create and open a note from template.mb",
        keywords: "note new create file template",
    },
    AppCommandDefinition {
        id: AppCommand::EditTemplate,
        label: "Template: Edit",
        description: "Open template.mb in your editor",
        keywords: "template edit note new mb editor",
    },
    AppCommandDefinition {
        id: AppCommand::EditCurrentNote,
        label: "Note: Edit",
        description: "Open the current note in your editor",
        keywords: "note edit editor current article",
    },
    AppCommandDefinition {
        id: AppCommand::RenameCurrentNote,
        label: "Note: Rename",
        description: "Rename the current note without changing its format",
        keywords: "note rename name current article",
    },
    AppCommandDefinition {
        id: AppCommand::DeleteCurrentNote,
        label: "Note: Delete",
        description: "Delete the current note after confirmation",
        keywords: "note delete remove current article",
    },
    AppCommandDefinition {
        id: AppCommand::ArchiveCurrentNote,
        label: "Note: Archive",
        description: "Move the current note into Archives",
        keywords: "note archive achieve current article",
    },
    AppCommandDefinition {
        id: AppCommand::RestoreCurrentNote,
        label: "Note: Restore",
        description: "Move the current archived note back into Notes",
        keywords: "note restore unarchive current article",
    },
    AppCommandDefinition {
        id: AppCommand::EditAiConfig,
        label: "Config: Edit AI settings",
        description: "Open config/ai.toml in your editor",
        keywords: "config configuration ai anthropic tavily model settings editor",
    },
    AppCommandDefinition {
        id: AppCommand::EditAgentInstructions,
        label: "Config: Edit Agent instructions",
        description: "Open config/AGENTS.md in your editor",
        keywords: "config configuration agent instructions agents md editor",
    },
    AppCommandDefinition {
        id: AppCommand::EditAgentMemory,
        label: "Config: Edit Agent memory",
        description: "Open MEMORY.md in your editor",
        keywords: "config configuration agent memory md editor",
    },
    AppCommandDefinition {
        id: AppCommand::SwitchTheme,
        label: "Theme: Switch",
        description: "Choose the active theme",
        keywords: "theme switch colors palette appearance random default",
    },
    AppCommandDefinition {
        id: AppCommand::BrowseTags,
        label: "Tags: Browse",
        description: "Browse tags by document and mention count",
        keywords: "tags hashtags browse search documents mentions",
    },
    AppCommandDefinition {
        id: AppCommand::RenameTag,
        label: "Tags: Rename",
        description: "Rename a tag across the workspace",
        keywords: "tags hashtags rename refactor workspace",
    },
];

fn command_definition(id: AppCommand) -> Option<&'static AppCommandDefinition> {
    APP_COMMANDS.iter().find(|command| command.id == id)
}

fn command_match_score(command: &AppCommandDefinition, query: &str) -> Option<u8> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let candidate = format!(
        "{} {} {}",
        command.label, command.description, command.keywords
    )
    .to_lowercase();
    if candidate.contains(&query) {
        return Some(0);
    }
    if query
        .split_whitespace()
        .all(|term| candidate.contains(term))
    {
        return Some(1);
    }
    fuzzy_match(&candidate, &query).then_some(2)
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
    Delete(DailyNote),
    Archive(DailyNote),
    Move {
        daily_note: DailyNote,
        target: PathBuf,
        appended: String,
    },
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
    Daily,
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
    ConfirmDeleteDaily,
    ConfirmDeleteFile,
    Help,
    AiPrompt,
    Approval,
    AskUser,
    WikiLinkChoice,
    Terminal,
    /// A caller-provided command dialog. Its mode and purpose live in
    /// [`DialogState`], allowing new command-style interactions without
    /// adding another overlay variant.
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
    CommandPalette,
}

/// Business purpose of a dialog. The mode controls interaction while the
/// purpose controls the result that is sent to the owning subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogPurpose {
    DeleteDaily,
    DeleteFile,
    AgentPrompt,
    AgentApproval,
    AskUser,
    WikiLinkChoice,
    Help,
    NewFile,
    RenameFile,
    CommandPalette,
    ThemePicker,
    TagPicker,
    TagRenameSource,
    TagRenameTarget,
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
    Daily(NaiveDate),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentReturn {
    Daily,
    Search,
}

/// A regular or daily note rendered as Markdown in the center pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub kind: DocumentKind,
    pub title: String,
    pub source: String,
    pub scroll: u16,
    /// One-based source line to reveal on the next render.
    pub target_line: Option<usize>,
    pub return_to: DocumentReturn,
    pub(crate) render_cache: Option<DocumentRenderCache>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocumentRenderCache {
    width: usize,
    pub rendered: crate::markdown::RenderedMarkup,
}

const DOCUMENT_CACHE_CAPACITY: usize = 8;
const DOCUMENT_CACHE_MAX_CELLS: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CachedDocumentRender {
    kind: DocumentKind,
    source: String,
    render: DocumentRenderCache,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DocumentRenderLru {
    entries: VecDeque<CachedDocumentRender>,
}

impl DocumentRenderLru {
    fn insert(&mut self, kind: DocumentKind, source: String, render: DocumentRenderCache) {
        self.remove(&kind);
        self.entries.push_front(CachedDocumentRender {
            kind,
            source,
            render,
        });
        while self.entries.len() > DOCUMENT_CACHE_CAPACITY
            || (self.entries.len() > 1 && self.approximate_cells() > DOCUMENT_CACHE_MAX_CELLS)
        {
            self.entries.pop_back();
        }
    }

    fn take(&mut self, kind: &DocumentKind, source: &str) -> Option<DocumentRenderCache> {
        let index = self.entries.iter().position(|entry| &entry.kind == kind)?;
        let entry = self.entries.remove(index)?;
        (entry.source == source).then_some(entry.render)
    }

    fn remove(&mut self, kind: &DocumentKind) {
        self.entries.retain(|entry| &entry.kind != kind);
    }

    fn retarget_file(&mut self, from: &Path, to: &Path) {
        for entry in &mut self.entries {
            if matches!(&entry.kind, DocumentKind::File(path) if path == from) {
                entry.kind = DocumentKind::File(to.to_path_buf());
            }
        }
    }

    fn approximate_cells(&self) -> usize {
        self.entries.iter().fold(0usize, |total, entry| {
            total.saturating_add(
                entry
                    .render
                    .width
                    .saturating_mul(entry.render.rendered.lines.len()),
            )
        })
    }
}

impl Document {
    pub(crate) fn replace_source(&mut self, source: String) {
        if self.source != source {
            self.source = source;
            self.render_cache = None;
        }
    }

    pub(crate) fn ensure_rendered(&mut self, width: usize, theme: crate::theme::Theme) -> bool {
        if self
            .render_cache
            .as_ref()
            .is_some_and(|cache| cache.width == width)
        {
            return false;
        }
        self.render_cache = Some(DocumentRenderCache {
            width,
            rendered: crate::markdown::render_at_width(&self.source, width, theme),
        });
        true
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DailyCardRenderCache {
    pub width: usize,
    pub date: NaiveDate,
    pub date_label: String,
    pub body: String,
    pub lines: Vec<ratatui::text::Line<'static>>,
    pub links: Vec<crate::markdown::RenderedLink>,
    pub tags: Vec<crate::markdown::RenderedTag>,
    pub images: Vec<mbtui::ImagePlacement>,
    pub button_line: usize,
    pub button_start: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentEntryRenderCache {
    pub width: usize,
    pub entry: AgentPanelEntry,
    pub lines: Vec<ratatui::text::Line<'static>>,
    pub links: Vec<crate::markdown::RenderedLink>,
    pub images: Vec<mbtui::ImagePlacement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DailyVirtualItem {
    pub date: NaiveDate,
    pub cache: Option<DailyCardRenderCache>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DailyVirtualList {
    pub width: usize,
    pub geometry: crate::vlist::VList,
    pub items: Vec<DailyVirtualItem>,
}

impl Default for DailyVirtualList {
    fn default() -> Self {
        Self {
            width: 0,
            geometry: crate::vlist::VList::new(12),
            items: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentVirtualList {
    pub width: usize,
    pub geometry: crate::vlist::VList,
    pub caches: Vec<Option<AgentEntryRenderCache>>,
}

impl Default for AgentVirtualList {
    fn default() -> Self {
        Self {
            width: 0,
            geometry: crate::vlist::VList::new(4),
            caches: Vec::new(),
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

    ai_events: Option<Receiver<AgentEvent>>,
    ai_approval_sender: Option<mpsc::Sender<ApprovalDecision>>,
    ai_user_sender: Option<mpsc::Sender<AskUserResponse>>,
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
            workspace_index: WorkspaceIndexHandle::default(),
            pending_tag_rename: None,
            help_scroll: 0,
            status: String::new(),
            animation_tick: 0,
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
            ai_events: None,
            ai_approval_sender: None,
            ai_user_sender: None,
            agent_input_buffer: Arc::new(Mutex::new(Vec::new())),
            ai_running: false,
            permission_mode: PermissionMode::Approve,
            permission_bypass: Arc::new(AtomicBool::new(false)),
            agent_panel,
            agent_vlist: AgentVirtualList::default(),
            agent_scroll,
            agent_usage,
            agent_timed_output_tokens,
            agent_response_duration,
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

    /// Reload everything that may have changed while `$EDITOR` was running.
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
                AgentEvent::AssistantDelta(delta) => {
                    match self.agent_panel.last_mut() {
                        Some(AgentPanelEntry::Assistant {
                            text, streaming, ..
                        }) if *streaming => text.push_str(&delta),
                        _ => self.agent_panel.push(AgentPanelEntry::Assistant {
                            text: delta,
                            streaming: true,
                            final_output: false,
                        }),
                    }
                    self.agent_scroll = u16::MAX;
                }
                AgentEvent::AssistantMessageFinished { final_output } => {
                    for entry in &mut self.agent_panel {
                        if let AgentPanelEntry::Assistant {
                            streaming,
                            final_output: entry_final,
                            ..
                        } = entry
                        {
                            if *streaming {
                                *streaming = false;
                                *entry_final = final_output;
                            }
                        }
                    }
                }
                AgentEvent::BufferedInputConsumed(count) => {
                    for followup in self
                        .agent_panel
                        .iter_mut()
                        .filter_map(|entry| match entry {
                            AgentPanelEntry::Prompt { muted, .. } if *muted => Some(muted),
                            _ => None,
                        })
                        .take(count)
                    {
                        *followup = false;
                    }
                }
                AgentEvent::ToolStarted(message) => {
                    self.agent_panel.push(AgentPanelEntry::Tool {
                        text: message.clone(),
                        active: true,
                    });
                    self.agent_scroll = u16::MAX;
                    self.set_status(message);
                }
                AgentEvent::ToolFinished(message) => {
                    if let Some(AgentPanelEntry::Tool { text, active }) =
                        self.agent_panel.iter_mut().rev().find(|entry| {
                            matches!(entry, AgentPanelEntry::Tool { active: true, .. })
                        })
                    {
                        *text = message.clone();
                        *active = false;
                    } else {
                        self.agent_panel.push(AgentPanelEntry::Tool {
                            text: message.clone(),
                            active: false,
                        });
                    }
                    self.agent_scroll = u16::MAX;
                    self.set_status(message);
                }
                AgentEvent::Usage(usage) => self.agent_usage.add(usage),
                AgentEvent::ResponseTiming {
                    output_tokens,
                    elapsed,
                } => {
                    self.agent_timed_output_tokens =
                        self.agent_timed_output_tokens.saturating_add(output_tokens);
                    self.agent_response_duration =
                        self.agent_response_duration.saturating_add(elapsed);
                }
                AgentEvent::Round { current, limit } => {
                    self.agent_round = current;
                    self.agent_round_limit = limit;
                }
                AgentEvent::ConversationUpdated(conversation) => {
                    self.agent_conversation = conversation;
                    if let Err(error) = self.persist_agent_session() {
                        self.set_error(format!("Agent session save error: {error}"));
                    }
                }
                AgentEvent::Notification(message) => {
                    self.notifications.notify(message);
                    self.set_status("Agent sent a notification");
                }
                AgentEvent::FileMoved { from, to } => {
                    self.handle_agent_file_moved(&from, &to);
                }
                AgentEvent::OpenFile(path) => {
                    self.open_file_document(&path, DocumentReturn::Daily);
                    if self
                        .document
                        .as_ref()
                        .is_some_and(|document| document.kind == DocumentKind::File(path.clone()))
                    {
                        self.set_status(format!("Agent opened {}", path.display()));
                    }
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
                    self.set_status(if request.kind == AskUserKind::RoundLimit {
                        "Agent reached its request-round limit"
                    } else {
                        "Agent is waiting for your answer"
                    });
                    self.ask_user_option = 0;
                    self.ask_user_input.clear();
                    self.ask_user_cursor = 0;
                    self.ask_user_request = Some(request);
                    self.set_overlay(Overlay::AskUser);
                }
                AgentEvent::Finished(result) => {
                    let completed_successfully = result.is_ok();
                    self.ai_running = false;
                    self.ai_cancel = None;
                    match result {
                        Ok(output) => {
                            self.agent_scroll = u16::MAX;
                            if output.is_empty() {
                                self.notifications
                                    .notify("Agent stopped at the request-round limit");
                                self.set_status("Agent paused at the request-round limit");
                            } else {
                                self.notifications.notify("Agent finished");
                                self.set_status("Agent finished");
                            }
                        }
                        Err(error) => {
                            for entry in &mut self.agent_panel {
                                if let AgentPanelEntry::Assistant { streaming, .. } = entry {
                                    *streaming = false;
                                }
                            }
                            self.agent_panel
                                .push(AgentPanelEntry::Error(format!("Agent failed: {error}")));
                            self.agent_scroll = u16::MAX;
                            self.set_error(format!("AI error: {error}"));
                        }
                    }
                    self.clear_ask_user();
                    self.reload_workspace();
                    if completed_successfully {
                        let pending = self
                            .agent_input_buffer
                            .lock()
                            .map(|mut buffer| std::mem::take(&mut *buffer))
                            .unwrap_or_default();
                        if !pending.is_empty() {
                            self.mark_buffered_prompts_consumed(pending.len());
                            self.start_agent_worker(pending.join("\n\n"));
                        }
                    }
                }
            }
        }
        if disconnected && self.ai_running {
            self.ai_running = false;
            self.ai_cancel = None;
            self.agent_panel.push(AgentPanelEntry::Error(
                "Agent worker stopped unexpectedly".to_string(),
            ));
            self.agent_scroll = u16::MAX;
            self.clear_ask_user();
            self.set_error("AI error: worker stopped unexpectedly");
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

    pub fn toggle_terminal(&mut self) {
        if self.overlay == Some(Overlay::Terminal) {
            self.restore_terminal_return_overlay();
            return;
        }
        if self.terminal.is_none() {
            match EmbeddedTerminal::spawn(&self.storage.root) {
                Ok(terminal) => self.terminal = Some(terminal),
                Err(error) => {
                    self.set_error(format!("Terminal error: {error}"));
                    return;
                }
            }
        }
        self.terminal_return_overlay = self.overlay.take();
        self.terminal_return_dialog = self.dialog.take();
        self.overlay = Some(Overlay::Terminal);
    }

    pub fn poll_terminal(&mut self) {
        let result = self.terminal.as_mut().map(EmbeddedTerminal::try_wait);
        match result {
            Some(Ok(Some(_))) => {
                self.terminal = None;
                if self.overlay == Some(Overlay::Terminal) {
                    self.restore_terminal_return_overlay();
                }
                self.set_status("Terminal session ended");
            }
            Some(Err(error)) => self.close_terminal_with_error(error),
            _ => {}
        }
    }

    pub(crate) fn terminal_snapshot(&mut self, rows: u16, cols: u16) -> Option<TerminalSnapshot> {
        let resize = self
            .terminal
            .as_mut()
            .map(|terminal| terminal.resize(rows, cols));
        if let Some(Err(error)) = resize {
            self.close_terminal_with_error(error);
            return None;
        }
        self.terminal.as_ref().map(EmbeddedTerminal::snapshot)
    }

    fn write_terminal_key(&mut self, key: KeyEvent) {
        let result = self
            .terminal
            .as_mut()
            .map(|terminal| terminal.write_key(key));
        if let Some(Err(error)) = result {
            self.close_terminal_with_error(error);
        }
    }

    fn write_terminal_paste(&mut self, text: &str) {
        let result = self
            .terminal
            .as_mut()
            .map(|terminal| terminal.write_paste(text));
        if let Some(Err(error)) = result {
            self.close_terminal_with_error(error);
        }
    }

    fn close_terminal_with_error(&mut self, error: impl std::fmt::Display) {
        self.terminal = None;
        if self.overlay == Some(Overlay::Terminal) {
            self.restore_terminal_return_overlay();
        }
        self.set_error(format!("Terminal error: {error}"));
    }

    #[cfg(test)]
    fn terminal_process_id(&self) -> Option<u32> {
        self.terminal
            .as_ref()
            .and_then(EmbeddedTerminal::process_id)
    }

    fn restore_terminal_return_overlay(&mut self) {
        self.overlay = self.terminal_return_overlay.take();
        self.dialog = self.terminal_return_dialog.take();
    }

    fn discard_terminal_return_overlay(&mut self) {
        self.terminal_return_overlay = None;
        self.terminal_return_dialog = None;
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
            .filter_map(|id| command_definition(*id))
            .map(|command| DialogOption::with_hint(command.label, command.description))
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
            KeyCode::Right => {
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

    fn retarget_open_document(&mut self, from: &Path, to: &Path) -> bool {
        self.document_render_lru.retarget_file(from, to);
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
            KeyCode::Char('C') => self.execute_app_command(AppCommand::ClearAgentSession),
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

    fn send_user_response(&mut self, response: AskUserResponse) -> anyhow::Result<()> {
        let round_limit = self
            .ask_user_request
            .as_ref()
            .is_some_and(|request| request.kind == AskUserKind::RoundLimit);
        let sender = self
            .ai_user_sender
            .as_ref()
            .context("Agent user-response channel is unavailable")?;
        sender
            .send(response.clone())
            .context("sending response to Agent")?;
        self.set_status(if round_limit {
            match &response {
                AskUserResponse::Answer(answer) if answer == "Continue" => "Agent continuing",
                _ => "Agent stopping at the request-round limit",
            }
        } else {
            match response {
                AskUserResponse::Answer(_) => "Answer sent to Agent",
                AskUserResponse::Cancelled => "Agent question cancelled",
            }
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

    pub fn apply_workspace_index(&mut self, index: WorkspaceIndex) {
        self.workspace_index.replace(index);
        if self.status == "Workspace index is still building" {
            self.status.clear();
        }
        if self.center_view == CenterView::Search && !self.search_query.trim().is_empty() {
            self.recompute_search();
        }
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

    fn open_agent_prompt(&mut self, date: NaiveDate) {
        if self.daily_note_clone(date).is_none() {
            self.set_status("Daily note not found");
            return;
        }
        self.ai_source_date = Some(date);
        self.ai_prompt_input.clear();
        self.ai_prompt_cursor = 0;
        self.set_overlay(Overlay::AiPrompt);
    }

    fn daily_edit_command(&self, date: NaiveDate) -> Option<Command> {
        self.storage
            .daily_file_path(&date.to_string())
            .ok()
            .filter(|path| path.is_file())
            .map(Command::Edit)
    }

    fn submit_agent_prompt(&mut self) {
        let Some(date) = self.ai_source_date.take() else {
            self.overlay = None;
            return;
        };
        let Ok(path) = self.storage.daily_file_path(&date.to_string()) else {
            self.overlay = None;
            self.set_status("Daily note not found");
            return;
        };
        if !path.is_file() {
            self.overlay = None;
            self.set_status("Daily note not found");
            return;
        }
        let display_path = path
            .strip_prefix(&self.storage.root)
            .unwrap_or(&path)
            .to_string_lossy();
        let requested = self.ai_prompt_input.trim();
        let (prompt, display_prompt) = if requested.is_empty() {
            (
                format!(
                    "The user wants you to format the daily note at: {display_path}\n\n{FORMAT_DAILY_NOTE_PROMPT}"
                ),
                format!("Format {display_path}"),
            )
        } else {
            (
                format!(
                    "The user wants you to work on the daily note at: {display_path}\n\n{requested}"
                ),
                requested.to_string(),
            )
        };
        self.overlay = None;
        self.dialog = None;
        if self.ai_running {
            self.buffer_agent_prompt(prompt, display_prompt);
        } else {
            self.start_agent(prompt, display_prompt);
        }
    }

    fn submit_compose_to_agent(&mut self) {
        let Some(prompt) = self.compose_agent_prompt() else {
            self.set_status("Enter a prompt for Agent");
            return;
        };
        let display_prompt = self.input.trim().to_string();
        let accepted = if self.ai_running {
            self.buffer_agent_prompt(prompt, display_prompt)
        } else {
            self.start_agent(prompt, display_prompt)
        };
        if accepted {
            self.input.clear();
            self.input_cursor = 0;
        }
    }

    fn buffer_agent_prompt(&mut self, prompt: String, display_prompt: String) -> bool {
        let queued = {
            match self.agent_input_buffer.lock() {
                Ok(mut buffer) => {
                    buffer.push(prompt);
                    true
                }
                Err(_) => false,
            }
        };
        if !queued {
            self.set_error("Agent input buffer is unavailable");
            return false;
        }
        self.agent_panel.push(AgentPanelEntry::Prompt {
            text: display_prompt,
            muted: true,
        });
        self.agent_scroll = u16::MAX;
        self.set_status("Prompt buffered for Agent");
        true
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
                DocumentKind::Daily(_) => None,
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
        self.agent_panel.push(AgentPanelEntry::Prompt {
            text: display_prompt,
            muted: false,
        });
        self.agent_scroll = u16::MAX;
        self.start_agent_worker(prompt)
    }

    fn start_agent_worker(&mut self, prompt: String) -> bool {
        if self.ai_running {
            self.set_status("AI is already working");
            return false;
        }
        let config_path = self.storage.ai_config_path.clone();
        let root = self.storage.root.clone();
        let (event_sender, event_receiver) = mpsc::channel();
        let (approval_sender, approval_receiver) = mpsc::channel();
        let (user_sender, user_receiver) = mpsc::channel();
        if let Ok(mut buffer) = self.agent_input_buffer.lock() {
            buffer.clear();
        }
        self.ai_events = Some(event_receiver);
        self.ai_approval_sender = Some(approval_sender);
        self.ai_user_sender = Some(user_sender);
        self.ai_running = true;
        self.agent_round = 0;
        self.agent_round_limit = 0;
        self.set_status("AI is working...");
        let bypass = self.permission_bypass.clone();
        let input_buffer = self.agent_input_buffer.clone();
        let workspace_index = self.workspace_index.clone();
        let mut conversation = self.agent_conversation.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.ai_cancel = Some(cancelled.clone());
        thread::spawn(move || {
            let result = Agent::from_config(
                &config_path,
                &root,
                AgentRuntime::new(
                    event_sender.clone(),
                    approval_receiver,
                    user_receiver,
                    input_buffer,
                    bypass,
                    cancelled,
                )
                .with_workspace_index(workspace_index),
            )
            .and_then(|agent| agent.run(&prompt, &mut conversation));
            let result = match result {
                Ok(output) => Ok(output),
                Err(error) => {
                    if agent_debug_logging_enabled() {
                        eprintln!("[nole debug] Agent error: {error:#}");
                    }
                    Err(error.to_string())
                }
            };
            if result.is_ok() {
                let _ = event_sender.send(AgentEvent::ConversationUpdated(conversation));
            }
            let _ = event_sender.send(AgentEvent::Finished(result));
        });
        true
    }

    fn mark_buffered_prompts_consumed(&mut self, count: usize) {
        for muted in self
            .agent_panel
            .iter_mut()
            .filter_map(|entry| match entry {
                AgentPanelEntry::Prompt { muted, .. } if *muted => Some(muted),
                _ => None,
            })
            .take(count)
        {
            *muted = false;
        }
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
        if let Ok(mut buffer) = self.agent_input_buffer.lock() {
            buffer.clear();
        }
        self.approval_request = None;
        self.clear_ask_user();
        if self.overlay == Some(Overlay::Approval) {
            self.overlay = None;
        }
        for entry in &mut self.agent_panel {
            if let AgentPanelEntry::Assistant { streaming, .. } = entry {
                *streaming = false;
            }
        }
        for entry in self.agent_panel.iter_mut().rev() {
            if let AgentPanelEntry::Tool { active, .. } = entry {
                if *active {
                    *active = false;
                }
            }
        }
        self.agent_panel
            .push(AgentPanelEntry::Error("Cancelled".to_string()));
        self.agent_scroll = u16::MAX;
        self.notifications.notify("Agent task cancelled");
        self.set_status("Agent task cancelled");
    }

    fn clear_agent_session(&mut self) {
        let was_running = self.ai_running;
        if was_running {
            self.cancel_agent();
        }
        let had_saved_session = match self.storage.clear_agent_session() {
            Ok(had_saved_session) => had_saved_session,
            Err(error) => {
                self.set_error(format!("Agent session clear error: {error}"));
                return;
            }
        };
        let had_history = self.agent_conversation.clear();
        let had_panel_content = !self.agent_panel.is_empty();
        self.agent_panel.clear();
        if let Ok(mut buffer) = self.agent_input_buffer.lock() {
            buffer.clear();
        }
        self.agent_scroll = 0;
        self.agent_usage = TokenUsage::default();
        self.agent_timed_output_tokens = 0;
        self.agent_response_duration = Duration::ZERO;
        self.agent_round = 0;
        self.agent_round_limit = 0;
        if was_running || had_saved_session || had_history || had_panel_content {
            self.set_status("Agent session cleared");
        } else {
            self.set_status("Agent session is already empty");
        }
    }

    fn persist_agent_session(&self) -> anyhow::Result<()> {
        let session = AgentSession::from_parts(
            &self.agent_conversation,
            &self.agent_panel,
            self.agent_usage,
            self.agent_timed_output_tokens,
            self.agent_response_duration,
        );
        self.storage.write_agent_session(&session)
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
            Some(DocumentKind::Daily(date)) => self.append_to_open_daily(&date.to_string(), &body),
            None => self.append_to_today(&body),
        };
        if let Err(error) = result {
            self.set_error(format!("Error: {error}"));
        }
    }

    fn append_to_open_note(&mut self, path: &Path, body: &str) -> anyhow::Result<()> {
        self.storage.append_document(path, body)?;
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

    fn append_to_open_daily(&mut self, date: &str, body: &str) -> anyhow::Result<()> {
        let note = self.storage.append_daily(date, body)?;
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

    fn append_to_today(&mut self, body: &str) -> anyhow::Result<()> {
        self.storage.append_to_today(body)?;
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

    fn undo(&mut self) {
        let Some(operation) = self.undo_stack.pop() else {
            self.set_status("Nothing to undo");
            return;
        };
        let status = match operation {
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

    fn add_daily_note(app: &mut App, body: &str) {
        app.storage.append_to_today(body).unwrap();
        app.reload();
        app.selected = app.daily_notes.len() - 1;
        app.focus = Focus::Center;
    }

    fn refresh_test_index(app: &mut App) {
        app.apply_workspace_index(WorkspaceIndex::build(&app.storage));
    }

    #[test]
    fn starts_with_daily_center_focused() {
        let (app, _directory) = make_app();
        assert_eq!(app.focus, Focus::Center);
        assert_eq!(app.center_view, CenterView::Daily);
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
    fn animation_phase_advances_for_daily_agent_or_bypass() {
        let (mut app, _directory) = make_app();
        app.advance_animation();
        assert_eq!(app.animation_tick, 1);
        app.center_view = CenterView::Document;
        app.advance_animation();
        assert_eq!(app.animation_tick, 1);
        app.focus = Focus::Compose;
        app.advance_animation();
        assert_eq!(app.animation_tick, 2);
        app.focus = Focus::Center;
        app.ai_running = true;
        app.advance_animation();
        app.advance_animation();
        assert_eq!(app.animation_tick, 4);
        app.ai_running = false;
        app.advance_animation();
        assert_eq!(app.animation_tick, 4);
        app.permission_mode = PermissionMode::Bypass;
        app.advance_animation();
        assert_eq!(app.animation_tick, 5);
    }

    #[test]
    fn command_palette_filters_and_clears_the_agent_session() {
        let (mut app, _directory) = make_app();
        app.agent_conversation = AgentConversation::seeded_for_test();
        app.agent_panel.push(AgentPanelEntry::Prompt {
            text: "Previous prompt".to_string(),
            muted: false,
        });
        app.persist_agent_session().unwrap();
        assert!(app.storage.agent_session_path.exists());

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(app.overlay, Some(Overlay::Dialog));
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::CommandPalette)
        );
        assert_eq!(
            app.command_matches.len(),
            APP_COMMANDS
                .iter()
                .filter(|command| app.command_available(command.id))
                .count()
        );

        app.handle_paste("clear");
        assert_eq!(
            app.command_matches.first(),
            Some(&AppCommand::ClearAgentSession)
        );
        assert_eq!(
            app.dialog
                .as_ref()
                .and_then(DialogState::selected_option)
                .map(|option| option.label.as_str()),
            Some("Agent: Clear session")
        );
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.overlay, None);
        assert!(!app.agent_conversation.clear());
        assert!(app.agent_panel.is_empty());
        assert!(!app.storage.agent_session_path.exists());
        assert_eq!(app.status, "Agent session cleared");
    }

    #[test]
    fn app_restores_the_single_agent_session() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let conversation = AgentConversation::seeded_for_test();
        let panel = vec![AgentPanelEntry::Assistant {
            text: "Persisted answer".to_string(),
            streaming: false,
            final_output: true,
        }];
        storage
            .write_agent_session(&AgentSession::from_parts(
                &conversation,
                &panel,
                TokenUsage {
                    input_tokens: 10,
                    output_tokens: 4,
                    cache_creation_input_tokens: 2,
                    cache_read_input_tokens: 3,
                },
                4,
                Duration::from_secs(2),
            ))
            .unwrap();

        let mut app = App::new(storage).unwrap();

        assert!(app.agent_conversation.clear());
        assert_eq!(app.agent_panel, panel);
        assert_eq!(app.agent_scroll, u16::MAX);
        assert_eq!(app.agent_usage.input_tokens, 10);
        assert_eq!(app.agent_timed_output_tokens, 4);
        assert_eq!(app.agent_response_duration, Duration::from_secs(2));
    }

    #[test]
    fn conversation_update_overwrites_the_saved_agent_session() {
        let (mut app, _directory) = make_app();
        app.agent_panel.push(AgentPanelEntry::Assistant {
            text: "Completed answer".to_string(),
            streaming: false,
            final_output: true,
        });
        let (sender, receiver) = mpsc::channel();
        app.ai_events = Some(receiver);

        sender
            .send(AgentEvent::ConversationUpdated(
                AgentConversation::seeded_for_test(),
            ))
            .unwrap();
        app.poll_agent();

        let (mut conversation, panel, _, _, _) = app
            .storage
            .load_agent_session()
            .unwrap()
            .unwrap()
            .into_parts();
        assert!(conversation.clear());
        assert_eq!(panel, app.agent_panel);
    }

    #[test]
    fn failed_session_delete_keeps_the_in_memory_session() {
        let (mut app, _directory) = make_app();
        app.agent_conversation = AgentConversation::seeded_for_test();
        fs::create_dir(&app.storage.agent_session_path).unwrap();

        app.clear_agent_session();

        assert!(app.agent_conversation.clear());
        assert!(app.status.starts_with("Agent session clear error:"));
        assert!(app.notifications.visible().is_some());
    }

    #[test]
    fn command_palette_interrupts_the_running_agent() {
        let (mut app, _directory) = make_app();
        let cancelled = Arc::new(AtomicBool::new(false));
        app.ai_cancel = Some(cancelled.clone());
        app.ai_running = true;

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_paste("interrupt");
        assert_eq!(
            app.command_matches.first(),
            Some(&AppCommand::InterruptAgent)
        );
        app.handle_key(key(KeyCode::Enter));

        assert!(cancelled.load(Ordering::Relaxed));
        assert!(!app.ai_running);
        assert_eq!(app.status, "Agent task cancelled");
    }

    #[test]
    fn command_palette_creates_and_opens_a_regular_note() {
        let (mut app, _directory) = make_app();
        fs::write(&app.storage.template_path, "# From template\n").unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_paste("note new");
        assert_eq!(app.command_matches.first(), Some(&AppCommand::NewNote));
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.files_context, FilesContext::NewTarget);
        assert_eq!(app.overlay, Some(Overlay::Dialog));
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::NewFile)
        );

        app.handle_paste("Scratch");
        app.handle_key(key(KeyCode::Enter));

        let path = app.storage.data_dir.join("Scratch.md");
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "# Scratch\n\n");
        assert_eq!(app.files_context, FilesContext::Browse);
        assert_eq!(app.focus, Focus::Center);
        assert!(matches!(
            app.document.as_ref().map(|document| &document.kind),
            Some(DocumentKind::File(opened)) if opened == &path
        ));
        assert_eq!(
            app.document
                .as_ref()
                .map(|document| document.source.as_str()),
            Some("# Scratch\n\n")
        );
    }

    #[test]
    fn command_palette_creates_a_note_from_the_template_only_when_requested() {
        let (mut app, _directory) = make_app();
        fs::write(&app.storage.template_path, "# From template\n").unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_paste("new from template");
        assert_eq!(
            app.command_matches.first(),
            Some(&AppCommand::NewNoteFromTemplate)
        );
        app.handle_key(key(KeyCode::Enter));
        app.handle_paste("Templated");
        app.handle_key(key(KeyCode::Enter));

        let path = app.storage.data_dir.join("Templated.md");
        assert_eq!(fs::read_to_string(&path).unwrap(), "# From template\n");
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(path))
        );
    }

    #[test]
    fn command_palette_adds_contextual_note_and_agent_output_commands() {
        let (mut app, _directory) = make_app();
        let current = app.storage.data_dir.join("Current.md");
        let other = app.storage.data_dir.join("Other.md");
        fs::write(&current, "# Current\n").unwrap();
        fs::write(&other, "# Other\n").unwrap();
        app.reload_files();
        app.open_file_document(&current, DocumentReturn::Daily);
        app.selected_file = Some(other);
        app.agent_panel.push(AgentPanelEntry::Assistant {
            text: "Final response".to_string(),
            streaming: false,
            final_output: true,
        });

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));

        assert!(app.command_matches.contains(&AppCommand::EditCurrentNote));
        assert!(app.command_matches.contains(&AppCommand::RenameCurrentNote));
        assert!(app.command_matches.contains(&AppCommand::DeleteCurrentNote));
        assert!(app
            .command_matches
            .contains(&AppCommand::ArchiveCurrentNote));
        assert!(!app
            .command_matches
            .contains(&AppCommand::RestoreCurrentNote));

        app.handle_paste("rename");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.pending_file.as_ref(), Some(&current));
        assert_eq!(app.files_context, FilesContext::Rename);
        assert_eq!(app.overlay, Some(Overlay::Dialog));
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::RenameFile)
        );
    }

    #[test]
    fn archived_note_gets_restore_instead_of_archive_command() {
        let (mut app, _directory) = make_app();
        let note = app.storage.data_dir.join("Archived.md");
        fs::write(&note, "archive me\n").unwrap();
        let archived = app.storage.archive_note(&note).unwrap();
        app.reload_files();
        app.open_file_document(&archived, DocumentReturn::Daily);

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));

        assert!(app
            .command_matches
            .contains(&AppCommand::RestoreCurrentNote));
        assert!(!app
            .command_matches
            .contains(&AppCommand::ArchiveCurrentNote));

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_key(key(KeyCode::Char('u')));

        let restored = app.storage.data_dir.join("Archived.md");
        assert!(!archived.exists());
        assert!(restored.exists());
        assert_eq!(app.status, "Note restored");
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(restored))
        );
    }

    #[test]
    fn opening_a_note_keeps_the_file_tree_selection_in_sync() {
        let (mut app, _directory) = make_app();
        let first = app.storage.data_dir.join("First.md");
        let second = app.storage.data_dir.join("Second.md");
        fs::write(&first, "first\n").unwrap();
        fs::write(&second, "second\n").unwrap();
        app.reload_files();
        app.selected_file = Some(first);
        app.notes_expanded = false;

        app.open_file_document(&second, DocumentReturn::Search);

        assert_eq!(app.selected_file.as_ref(), Some(&second));
        assert!(app.notes_expanded);
        assert!(matches!(
            app.visible_file_rows().get(app.file_row),
            Some(FileListRow::File(index)) if app.note_files[*index].path == second
        ));
    }

    #[test]
    fn right_from_the_file_tree_opens_its_selected_note_before_focusing_content() {
        let (mut app, _directory) = make_app();
        let first = app.storage.data_dir.join("First.md");
        let second = app.storage.data_dir.join("Second.md");
        fs::write(&first, "first\n").unwrap();
        fs::write(&second, "second\n").unwrap();
        app.reload_files();
        app.open_file_document(&first, DocumentReturn::Daily);
        app.open_files();
        let second_row = app
            .visible_file_rows()
            .iter()
            .position(|row| {
                matches!(row, FileListRow::File(index) if app.note_files[*index].path == second)
            })
            .unwrap();
        app.select_file_row(second_row);
        assert_eq!(app.selected_file.as_ref(), Some(&second));
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(first))
        );

        app.handle_key(key(KeyCode::Right));

        assert_eq!(app.focus, Focus::Center);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(second.clone()))
        );
        assert_eq!(app.selected_file.as_ref(), Some(&second));
    }

    #[test]
    fn right_from_the_file_tree_returns_to_daily_without_opening_a_note() {
        let (mut app, _directory) = make_app();
        let note = app.storage.data_dir.join("Selected.md");
        fs::write(&note, "selected note\n").unwrap();
        app.reload_files();
        app.center_view = CenterView::Daily;
        app.document = None;
        app.open_files();
        let note_row = app
            .visible_file_rows()
            .iter()
            .position(|row| {
                matches!(row, FileListRow::File(index) if app.note_files[*index].path == note)
            })
            .unwrap();
        app.select_file_row(note_row);

        app.handle_key(key(KeyCode::Right));

        assert_eq!(app.focus, Focus::Center);
        assert_eq!(app.center_view, CenterView::Daily);
        assert!(app.document.is_none());
        assert_eq!(app.selected_file.as_ref(), Some(&note));
    }

    #[test]
    fn editable_support_files_open_through_the_editor_pipeline() {
        let (mut app, _directory) = make_app();
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);

        for (query, expected) in [
            ("ai settings", app.storage.ai_config_path.clone()),
            ("agent instructions", app.storage.agents_path.clone()),
            ("agent memory", app.storage.memory_path.clone()),
            ("template edit", app.storage.template_path.clone()),
        ] {
            app.handle_key(ctrl_p);
            app.handle_paste(query);
            let command = app.handle_key(key(KeyCode::Enter));
            assert_eq!(command, Some(Command::Edit(expected)));
            assert_eq!(app.overlay, None);
        }
    }

    #[test]
    fn command_palette_switches_theme_and_persists_the_selection() {
        let (mut app, _directory) = make_app();
        let custom =
            crate::theme::DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#010203\"");
        fs::write(app.storage.themes_dir.join("custom.toml"), custom).unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_paste("theme switch");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::ThemePicker)
        );

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.overlay, None);
        assert_eq!(app.theme_selection, "custom");
        assert_eq!(app.active_theme, "custom");
        assert_eq!(app.storage.load_theme_selection().unwrap(), "custom");
        assert_eq!(app.theme.surface_panel, ratatui::style::Color::Rgb(1, 2, 3));
    }

    #[test]
    fn command_palette_browses_tags_with_counts_and_opens_exact_search() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "daily #rust and #rust\nnot #rustlang");
        fs::write(app.storage.data_dir.join("Project.md"), "note #rust\n").unwrap();
        refresh_test_index(&mut app);

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_paste("tags browse");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::TagPicker)
        );
        let rust = app
            .dialog
            .as_ref()
            .unwrap()
            .options
            .iter()
            .find(|option| option.label == "#rust")
            .unwrap();
        assert_eq!(rust.hint.as_deref(), Some("2 documents · 3 mentions"));

        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.center_view, CenterView::Search);
        assert_eq!(app.search_query, "#rust");
        assert_eq!(app.search_results.len(), 2);
        assert!(app.search_results.iter().all(|hit| match hit {
            SearchHit::FileLine { text, .. } | SearchHit::DocumentLine { text, .. } => {
                !text.contains("#rustlang")
            }
        }));
    }

    #[test]
    fn command_palette_renames_an_exact_tag_across_the_workspace() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "daily #old and `#old`");
        let note = app.storage.data_dir.join("Project.md");
        fs::write(&note, "note #old and #oldlang\n").unwrap();
        refresh_test_index(&mut app);

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_paste("tags rename");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::TagRenameSource)
        );
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.pending_tag_rename.as_deref(), Some("old"));
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::TagRenameTarget)
        );
        app.handle_paste("new/tag");
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.overlay, None);
        assert!(app.status.contains("2 documents (2 mentions)"));
        assert_eq!(
            fs::read_to_string(note).unwrap(),
            "note #new/tag and #oldlang\n"
        );
        let daily = app
            .storage
            .daily_file_path(&app.daily_notes[0].date.to_string())
            .unwrap();
        assert_eq!(
            fs::read_to_string(daily).unwrap(),
            "daily #new/tag and `#old`\n"
        );
        assert_eq!(
            app.workspace_index
                .with_index(|index| index.exact_tag_hits("new/tag", None).len()),
            Some(2)
        );
    }

    #[test]
    fn ctrl_p_toggles_the_command_palette_without_replacing_other_dialogs() {
        let (mut app, _directory) = make_app();
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        app.handle_key(ctrl_p);
        assert_eq!(app.overlay, Some(Overlay::Dialog));
        app.handle_key(ctrl_p);
        assert_eq!(app.overlay, None);

        app.open_help();
        app.handle_key(ctrl_p);
        assert_eq!(app.overlay, Some(Overlay::Help));
    }

    #[test]
    fn tab_switches_permission_mode_without_changing_focus() {
        let (mut app, _directory) = make_app();
        assert_eq!(app.focus, Focus::Center);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.permission_mode, PermissionMode::Bypass);
        assert_eq!(app.focus, Focus::Center);
        assert!(app.permission_bypass.load(Ordering::Relaxed));
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.permission_mode, PermissionMode::Approve);
        assert!(!app.permission_bypass.load(Ordering::Relaxed));
    }

    #[test]
    fn ai_action_opens_an_optional_prompt_overlay() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "card body");
        let date = app.selected_date().unwrap();
        app.dispatch_action(date, Action::Ai);
        assert_eq!(app.overlay, Some(Overlay::AiPrompt));
        assert_eq!(app.ai_source_date, Some(date));
        app.handle_paste("custom prompt");
        assert_eq!(app.ai_prompt_input, "custom prompt");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn daily_ai_custom_prompt_includes_the_daily_file_path() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "card body that should not become the prompt");
        let date = app.selected_date().unwrap();
        let path = app.storage.daily_file_path(&date.to_string()).unwrap();
        let display_path = path
            .strip_prefix(&app.storage.root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.ai_running = true;

        app.dispatch_action(date, Action::Ai);
        app.handle_paste("Extract the action items");
        app.handle_key(key(KeyCode::Enter));

        let prompts = app.agent_input_buffer.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains(&display_path));
        assert!(prompts[0].contains("Extract the action items"));
        assert!(!prompts[0].contains("card body that should not become the prompt"));
        assert!(matches!(
            app.agent_panel.last(),
            Some(AgentPanelEntry::Prompt { text, muted: true })
                if text == "Extract the action items"
        ));
    }

    #[test]
    fn empty_daily_ai_prompt_requests_in_place_markdown_formatting() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "unformatted card body");
        let date = app.selected_date().unwrap();
        let path = app.storage.daily_file_path(&date.to_string()).unwrap();
        let display_path = path
            .strip_prefix(&app.storage.root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        app.ai_running = true;

        app.dispatch_action(date, Action::Ai);
        app.handle_key(key(KeyCode::Enter));

        let prompts = app.agent_input_buffer.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains(&display_path));
        assert!(prompts[0].contains(FORMAT_DAILY_NOTE_PROMPT));
        assert!(!prompts[0].contains("unformatted card body"));
        assert!(matches!(
            app.agent_panel.last(),
            Some(AgentPanelEntry::Prompt { text, muted: true })
                if text == &format!("Format {display_path}")
        ));
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
        app.set_overlay(Overlay::Approval);
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
                kind: AskUserKind::Tool,
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
                kind: AskUserKind::Tool,
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
    fn round_limit_dialog_submits_continue_and_escape_submits_stop() {
        let (mut app, _directory) = make_app();
        let (event_sender, event_receiver) = mpsc::channel();
        let (answer_sender, answer_receiver) = mpsc::channel();
        app.ai_events = Some(event_receiver);
        app.ai_user_sender = Some(answer_sender);
        let request = AskUserRequest {
            kind: AskUserKind::RoundLimit,
            question: "Continue for another segment?".to_string(),
            options: vec!["Continue".to_string(), "Stop".to_string()],
        };

        event_sender
            .send(AgentEvent::AskUser(request.clone()))
            .unwrap();
        app.poll_agent();
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            answer_receiver.try_recv().unwrap(),
            AskUserResponse::Answer("Continue".to_string())
        );
        assert_eq!(app.status, "Agent continuing");

        event_sender.send(AgentEvent::AskUser(request)).unwrap();
        app.poll_agent();
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(
            answer_receiver.try_recv().unwrap(),
            AskUserResponse::Answer("Stop".to_string())
        );
        assert_eq!(app.status, "Agent stopping at the request-round limit");
    }

    #[test]
    fn agent_panel_appends_streaming_activity_and_final_reply() {
        let (mut app, _directory) = make_app();
        let (sender, receiver) = mpsc::channel();
        app.ai_events = Some(receiver);
        app.ai_running = true;
        app.agent_panel = vec![
            AgentPanelEntry::Prompt {
                text: "First follow-up".to_string(),
                muted: true,
            },
            AgentPanelEntry::Prompt {
                text: "Second follow-up".to_string(),
                muted: true,
            },
        ];
        sender.send(AgentEvent::BufferedInputConsumed(1)).unwrap();
        sender
            .send(AgentEvent::AssistantDelta("I need to inspect ".to_string()))
            .unwrap();
        sender
            .send(AgentEvent::AssistantDelta("the source first.".to_string()))
            .unwrap();
        sender
            .send(AgentEvent::ToolStarted("Calling Read File...".to_string()))
            .unwrap();
        sender
            .send(AgentEvent::Round {
                current: 2,
                limit: 25,
            })
            .unwrap();
        sender
            .send(AgentEvent::Usage(TokenUsage {
                input_tokens: 1_000,
                output_tokens: 200,
                cache_creation_input_tokens: 300,
                cache_read_input_tokens: 700,
            }))
            .unwrap();
        sender
            .send(AgentEvent::ResponseTiming {
                output_tokens: 200,
                elapsed: Duration::from_secs(2),
            })
            .unwrap();
        app.poll_agent();
        assert_eq!(app.status, "Calling Read File...");
        assert!(matches!(
            &app.agent_panel[2],
            AgentPanelEntry::Assistant { text, streaming: true, .. }
                if text == "I need to inspect the source first."
        ));
        assert!(matches!(
            &app.agent_panel[3],
            AgentPanelEntry::Tool { text, active: true } if text == "Calling Read File..."
        ));
        assert_eq!(app.agent_round, 2);
        assert_eq!(app.agent_round_limit, 25);
        assert_eq!(app.agent_usage.total_input(), 2_000);
        assert_eq!(app.agent_timed_output_tokens, 200);
        assert_eq!(app.agent_response_duration, Duration::from_secs(2));
        assert!(matches!(
            &app.agent_panel[0],
            AgentPanelEntry::Prompt { muted: false, .. }
        ));
        assert!(matches!(
            &app.agent_panel[1],
            AgentPanelEntry::Prompt { muted: true, .. }
        ));

        sender
            .send(AgentEvent::ToolFinished("Completed Read File.".to_string()))
            .unwrap();
        app.poll_agent();
        assert!(matches!(
            &app.agent_panel[3],
            AgentPanelEntry::Tool { text, active: false } if text == "Completed Read File."
        ));

        sender
            .send(AgentEvent::AssistantMessageFinished {
                final_output: false,
            })
            .unwrap();
        sender
            .send(AgentEvent::AssistantDelta("final reply".to_string()))
            .unwrap();
        sender
            .send(AgentEvent::AssistantMessageFinished { final_output: true })
            .unwrap();
        sender
            .send(AgentEvent::Finished(Ok("final reply".to_string())))
            .unwrap();
        app.poll_agent();
        assert_eq!(app.agent_panel.len(), 5);
        assert!(matches!(
            app.agent_panel.last(),
            Some(AgentPanelEntry::Assistant { text, streaming: false, final_output: true })
                if text == "final reply"
        ));
        assert_eq!(app.status, "Agent finished");
        assert_eq!(
            app.notifications.visible().as_deref(),
            Some("Agent finished")
        );
        assert_eq!(app.notifications.take_bells(), 1);
    }

    #[test]
    fn agent_terminal_outcomes_send_distinct_notifications() {
        let (mut app, _directory) = make_app();
        let (sender, receiver) = mpsc::channel();
        app.ai_events = Some(receiver);
        app.ai_running = true;

        sender
            .send(AgentEvent::Finished(Ok(String::new())))
            .unwrap();
        app.poll_agent();

        assert_eq!(app.status, "Agent paused at the request-round limit");
        assert_eq!(
            app.notifications.visible().as_deref(),
            Some("Agent stopped at the request-round limit")
        );
        assert_eq!(app.notifications.take_bells(), 1);

        let (sender, receiver) = mpsc::channel();
        app.ai_events = Some(receiver);
        app.ai_running = true;
        sender
            .send(AgentEvent::Finished(Err("network unavailable".to_string())))
            .unwrap();
        app.poll_agent();

        assert_eq!(app.status, "AI error: network unavailable");
        assert_eq!(
            app.notifications.visible().as_deref(),
            Some("AI error: network unavailable")
        );
        assert_eq!(app.notifications.take_bells(), 1);
    }

    #[test]
    fn application_errors_notify_but_agent_tool_failures_do_not() {
        let (mut app, _directory) = make_app();
        app.set_error("Open error: application unavailable");
        assert_eq!(app.status, "Open error: application unavailable");
        assert_eq!(
            app.notifications.visible().as_deref(),
            Some("Open error: application unavailable")
        );
        assert_eq!(app.notifications.take_bells(), 1);

        let (sender, receiver) = mpsc::channel();
        app.ai_events = Some(receiver);
        sender
            .send(AgentEvent::ToolStarted("Calling Read File...".to_string()))
            .unwrap();
        sender
            .send(AgentEvent::ToolFinished(
                "Failed Read File: file not found".to_string(),
            ))
            .unwrap();
        app.poll_agent();

        assert_eq!(app.status, "Failed Read File: file not found");
        assert_eq!(
            app.notifications.visible().as_deref(),
            Some("Open error: application unavailable")
        );
        assert_eq!(app.notifications.take_bells(), 0);
    }

    #[test]
    fn agent_open_file_event_displays_the_note_in_the_tui() {
        let (mut app, _directory) = make_app();
        let note = app.storage.data_dir.join("Agent View.md");
        fs::write(&note, "# Opened by Agent\n").unwrap();
        let note = fs::canonicalize(note).unwrap();
        let (sender, receiver) = mpsc::channel();
        app.ai_events = Some(receiver);

        sender.send(AgentEvent::OpenFile(note.clone())).unwrap();
        app.poll_agent();

        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(app.focus, Focus::Center);
        assert!(matches!(
            app.document.as_ref().map(|document| &document.kind),
            Some(DocumentKind::File(path)) if path == &note
        ));
        assert_eq!(app.status, format!("Agent opened {}", note.display()));
    }

    #[test]
    fn c_cancels_a_running_agent_only_from_the_agent_panel() {
        let (mut app, _directory) = make_app();
        let cancelled = Arc::new(AtomicBool::new(false));
        app.ai_cancel = Some(cancelled.clone());
        app.ai_running = true;
        app.agent_panel.push(AgentPanelEntry::Tool {
            text: "Fetching Web...".to_string(),
            active: true,
        });
        app.focus = Focus::Center;

        app.handle_key(key(KeyCode::Char('c')));
        assert!(app.ai_running);
        assert!(!cancelled.load(Ordering::Relaxed));

        app.focus = Focus::Agent;
        app.handle_key(key(KeyCode::Char('c')));
        assert!(!app.ai_running);
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(
            matches!(app.agent_panel.last(), Some(AgentPanelEntry::Error(text)) if text == "Cancelled")
        );
        assert!(matches!(
            &app.agent_panel[0],
            AgentPanelEntry::Tool { active: false, .. }
        ));
        assert!(app.ai_events.is_none());
        assert!(app.ai_approval_sender.is_none());
        assert!(app.ai_user_sender.is_none());
        assert_eq!(app.status, "Agent task cancelled");
        assert_eq!(
            app.notifications.visible().as_deref(),
            Some("Agent task cancelled")
        );
        assert_eq!(app.notifications.take_bells(), 1);
    }

    #[test]
    fn uppercase_c_cancels_work_and_clears_the_agent_session() {
        let (mut app, _directory) = make_app();
        let cancelled = Arc::new(AtomicBool::new(false));
        app.ai_cancel = Some(cancelled.clone());
        app.ai_running = true;
        app.agent_panel = vec![
            AgentPanelEntry::Prompt {
                text: "Current prompt".to_string(),
                muted: false,
            },
            AgentPanelEntry::Tool {
                text: "Searching Web...".to_string(),
                active: true,
            },
            AgentPanelEntry::Assistant {
                text: "Looking for sources.".to_string(),
                streaming: true,
                final_output: false,
            },
        ];
        app.agent_conversation = AgentConversation::seeded_for_test();
        app.focus = Focus::Agent;

        app.handle_key(key(KeyCode::Char('C')));

        assert!(cancelled.load(Ordering::Relaxed));
        assert!(!app.ai_running);
        assert!(!app.agent_conversation.clear());
        assert!(app.agent_panel.is_empty());
        assert_eq!(app.status, "Agent session cleared");
    }

    #[test]
    fn recording_from_a_document_appends_silently_and_pins_scroll_to_end() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Article.md");
        fs::write(&path, "# Article\n\nInspiration\n").unwrap();
        app.open_file_document(&path, DocumentReturn::Daily);
        let document = app.document.as_mut().unwrap();
        document.scroll = 1;
        document.target_line = Some(1);
        app.status = "Old status".to_string();
        app.handle_key(key(KeyCode::Char('i')));
        assert_eq!(app.focus, Focus::Compose);
        app.handle_paste("new idea");
        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(path.clone()))
        );
        assert_eq!(app.document.as_ref().unwrap().scroll, u16::MAX);
        assert_eq!(app.document.as_ref().unwrap().target_line, None);
        assert!(app.daily_notes.is_empty());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Article\n\nInspiration\n\nnew idea\n"
        );
        assert_eq!(
            app.document.as_ref().unwrap().source,
            "# Article\n\nInspiration\n\nnew idea\n"
        );
        assert!(app.notifications.visible().is_none());
        assert!(app.status.is_empty());
    }

    #[test]
    fn recording_from_a_daily_preview_appends_to_that_date() {
        let (mut app, _directory) = make_app();
        app.storage.append_daily("2026-07-26", "first").unwrap();
        app.reload();
        app.open_daily_document(
            NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            DocumentReturn::Daily,
        );
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
    fn reload_keeps_the_selected_daily_note_by_date() {
        let (mut app, _directory) = make_app();
        let first = app.storage.append_daily("2026-07-26", "first").unwrap();
        let second = app.storage.append_daily("2026-07-27", "second").unwrap();
        app.reload();
        app.selected = app
            .daily_notes
            .iter()
            .position(|note| note.date == second.date)
            .unwrap();

        app.storage.remove_daily(&first.date.to_string()).unwrap();
        app.reload();

        assert_eq!(app.selected_date(), Some(second.date));
    }

    #[test]
    fn workspace_reload_applies_theme_changes_and_invalidates_render_caches() {
        let (mut app, _directory) = make_app();
        app.daily_vlist.width = 80;
        app.agent_vlist.width = 40;
        let custom =
            crate::theme::DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#010203\"");
        fs::write(app.storage.themes_dir.join("custom.toml"), custom).unwrap();
        app.storage.write_theme_selection("custom").unwrap();

        app.reload_workspace();

        assert_eq!(app.theme.surface_panel, ratatui::style::Color::Rgb(1, 2, 3));
        assert_eq!(app.daily_vlist.width, 0);
        assert_eq!(app.agent_vlist.width, 0);
    }

    #[test]
    fn compose_paste_normalizes_newlines_at_character_cursor() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Compose;
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
        app.open_file_document(&path, DocumentReturn::Daily);
        app.input = "Summarize the key point".to_string();

        let prompt = app.compose_agent_prompt().unwrap();
        assert!(prompt.contains("currently viewing note: data/Reference.md"));
        assert!(prompt.ends_with("Summarize the key point"));

        app.document = Some(Document {
            kind: DocumentKind::Daily(NaiveDate::from_ymd_opt(2026, 7, 27).unwrap()),
            title: "Daily".to_string(),
            source: String::new(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });
        assert_eq!(
            app.compose_agent_prompt().as_deref(),
            Some("Summarize the key point")
        );
    }

    #[test]
    fn ctrl_enter_sends_compose_to_agent_without_creating_a_chat_card() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Compose;
        let daily_count = app.daily_notes.len();
        app.agent_usage.input_tokens = 1_234;
        app.agent_timed_output_tokens = 400;
        app.agent_response_duration = Duration::from_secs(2);
        app.input = "Direct Agent prompt".to_string();
        app.input_cursor = app.input.chars().count();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

        assert!(app.ai_running);
        assert!(matches!(
            app.agent_panel.last(),
            Some(AgentPanelEntry::Prompt { text, muted: false }) if text == "Direct Agent prompt"
        ));
        assert!(app.input.is_empty());
        assert_eq!(app.input_cursor, 0);
        assert_eq!(app.daily_notes.len(), daily_count);
        assert_eq!(app.agent_usage.input_tokens, 1_234);
        assert_eq!(app.agent_timed_output_tokens, 400);
        assert_eq!(app.agent_response_duration, Duration::from_secs(2));
    }

    #[test]
    fn ctrl_enter_buffers_and_clears_compose_while_agent_is_busy() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Compose;
        app.ai_running = true;
        app.agent_panel.push(AgentPanelEntry::Prompt {
            text: "Initial prompt".to_string(),
            muted: false,
        });
        app.input = "Additional prompt".to_string();
        app.input_cursor = app.input.chars().count();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

        app.input = "One more detail".to_string();
        app.input_cursor = app.input.chars().count();
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

        assert!(app.input.is_empty());
        assert_eq!(app.input_cursor, 0);
        assert_eq!(
            app.agent_panel,
            [
                AgentPanelEntry::Prompt {
                    text: "Initial prompt".to_string(),
                    muted: false,
                },
                AgentPanelEntry::Prompt {
                    text: "Additional prompt".to_string(),
                    muted: true,
                },
                AgentPanelEntry::Prompt {
                    text: "One more detail".to_string(),
                    muted: true,
                }
            ]
        );
        assert_eq!(
            *app.agent_input_buffer.lock().unwrap(),
            ["Additional prompt", "One more detail"]
        );
        assert_eq!(app.status, "Prompt buffered for Agent");
    }

    #[test]
    fn f_and_t_change_only_focus() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.handle_key(key(KeyCode::Char('f')));
        assert_eq!(app.focus, Focus::Files);
        assert_eq!(app.center_view, CenterView::Daily);
        app.handle_key(key(KeyCode::Char('t')));
        assert_eq!(app.focus, Focus::Todo);
        assert_eq!(app.center_view, CenterView::Daily);
    }

    #[test]
    fn arrows_move_focus_across_the_workspace() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "selected card");
        app.focus = Focus::Center;

        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.focus, Focus::Files);
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.focus, Focus::Center);
        assert_eq!(app.center_view, CenterView::Daily);

        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.focus, Focus::Todo);
        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.focus, Focus::Center);

        app.todo_items.clear();
        app.agent_panel.push(AgentPanelEntry::Assistant {
            text: "final reply".to_string(),
            streaming: false,
            final_output: true,
        });
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
        app.agent_panel.push(AgentPanelEntry::Assistant {
            text: "final reply".to_string(),
            streaming: false,
            final_output: true,
        });
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
    fn enter_on_agent_panel_does_not_move_output_to_daily() {
        let (mut app, _directory) = make_app();
        let original_count = app.daily_notes.len();
        app.agent_panel = vec![
            AgentPanelEntry::Prompt {
                text: "User prompt".to_string(),
                muted: false,
            },
            AgentPanelEntry::Assistant {
                text: "Agent final reply".to_string(),
                streaming: false,
                final_output: true,
            },
        ];
        app.focus = Focus::Agent;

        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.daily_notes.len(), original_count);
        assert_eq!(app.agent_panel.len(), 2);
        assert_eq!(app.focus, Focus::Agent);
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
    fn file_enter_opens_center_document_and_escape_returns_to_daily() {
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
        assert_eq!(app.center_view, CenterView::Daily);
        assert_eq!(app.focus, Focus::Center);
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
        app.open_file_document(&path, DocumentReturn::Daily);

        assert_eq!(
            app.handle_key(key(KeyCode::Char('e'))),
            Some(Command::Edit(path))
        );
        assert_eq!(app.center_view, CenterView::Document);
    }

    #[test]
    fn document_render_cache_survives_scroll_and_invalidates_on_content_or_width() {
        let mut document = Document {
            kind: DocumentKind::File(PathBuf::from("cached.md")),
            title: "Cached".to_string(),
            source: "```rust\nfn main() {}\n```".repeat(100),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        };

        assert!(document.ensure_rendered(80, crate::theme::Theme::default()));
        assert!(!document.ensure_rendered(80, crate::theme::Theme::default()));
        document.scroll = 20;
        assert!(!document.ensure_rendered(80, crate::theme::Theme::default()));

        assert!(document.ensure_rendered(100, crate::theme::Theme::default()));
        assert!(!document.ensure_rendered(100, crate::theme::Theme::default()));
        document.replace_source("updated".to_string());
        assert!(document.render_cache.is_none());
        assert!(document.ensure_rendered(100, crate::theme::Theme::default()));
    }

    #[test]
    fn reopening_a_document_restores_its_app_level_render_cache() {
        let (mut app, _directory) = make_app();
        let first = app.storage.data_dir.join("First.md");
        let second = app.storage.data_dir.join("Second.md");
        fs::write(&first, "```rust\nfn main() {}\n```".repeat(100)).unwrap();
        fs::write(&second, "# Second").unwrap();

        app.open_file_document(&first, DocumentReturn::Daily);
        assert!(app
            .document
            .as_mut()
            .unwrap()
            .ensure_rendered(80, crate::theme::Theme::default()));
        app.open_file_document(&second, DocumentReturn::Daily);
        assert_eq!(app.document_render_lru.entries.len(), 1);

        app.open_file_document(&first, DocumentReturn::Daily);
        let document = app.document.as_mut().unwrap();
        assert!(document.render_cache.is_some());
        assert!(!document.ensure_rendered(80, crate::theme::Theme::default()));
    }

    #[test]
    fn reopening_a_changed_document_rejects_the_stale_render_cache() {
        let (mut app, _directory) = make_app();
        let first = app.storage.data_dir.join("First.md");
        let second = app.storage.data_dir.join("Second.md");
        fs::write(&first, "old source").unwrap();
        fs::write(&second, "second source").unwrap();

        app.open_file_document(&first, DocumentReturn::Daily);
        app.document
            .as_mut()
            .unwrap()
            .ensure_rendered(80, crate::theme::Theme::default());
        app.open_file_document(&second, DocumentReturn::Daily);
        fs::write(&first, "new source").unwrap();

        app.open_file_document(&first, DocumentReturn::Daily);
        assert!(app.document.as_ref().unwrap().render_cache.is_none());
        assert_eq!(app.document.as_ref().unwrap().source, "new source");
    }

    #[test]
    fn inactive_document_cache_follows_a_file_rename() {
        let (mut app, _directory) = make_app();
        let from = app.storage.data_dir.join("Before.md");
        let to = app.storage.data_dir.join("After.md");
        let other = app.storage.data_dir.join("Other.md");
        fs::write(&from, "cached source").unwrap();
        fs::write(&other, "other source").unwrap();

        app.open_file_document(&from, DocumentReturn::Daily);
        app.document
            .as_mut()
            .unwrap()
            .ensure_rendered(80, crate::theme::Theme::default());
        app.open_file_document(&other, DocumentReturn::Daily);
        fs::rename(&from, &to).unwrap();
        assert!(!app.retarget_open_document(&from, &to));

        app.open_file_document(&to, DocumentReturn::Daily);
        assert!(!app
            .document
            .as_mut()
            .unwrap()
            .ensure_rendered(80, crate::theme::Theme::default()));
    }

    #[test]
    fn document_render_lru_evicts_the_oldest_entries() {
        let (mut app, _directory) = make_app();
        let paths = (0..DOCUMENT_CACHE_CAPACITY + 3)
            .map(|index| {
                let path = app.storage.data_dir.join(format!("Note{index}.md"));
                fs::write(&path, format!("note {index}")).unwrap();
                path
            })
            .collect::<Vec<_>>();

        for path in &paths {
            app.open_file_document(path, DocumentReturn::Daily);
            app.document
                .as_mut()
                .unwrap()
                .ensure_rendered(80, crate::theme::Theme::default());
        }

        assert_eq!(
            app.document_render_lru.entries.len(),
            DOCUMENT_CACHE_CAPACITY
        );
        assert!(app.document_render_lru.entries.iter().all(|entry| {
            entry.kind != DocumentKind::File(paths[0].clone())
                && entry.kind != DocumentKind::File(paths[1].clone())
        }));
    }

    #[test]
    fn file_document_supports_rename_delete_and_archive_shortcuts() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "# Project\n").unwrap();
        app.reload_files();
        app.open_file_document(&path, DocumentReturn::Daily);

        app.handle_key(key(KeyCode::Char('r')));
        assert_eq!(app.overlay, Some(Overlay::Dialog));
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::RenameFile)
        );
        assert_eq!(app.pending_file.as_ref(), Some(&path));
        app.handle_key(key(KeyCode::Esc));

        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteFile));
        assert_eq!(app.pending_file.as_ref(), Some(&path));
        app.handle_key(key(KeyCode::Esc));

        app.handle_key(key(KeyCode::Char('a')));
        let archived = app.storage.archives_dir.join("Project.md");
        assert!(!path.exists());
        assert!(archived.exists());
        assert_eq!(app.center_view, CenterView::Document);
        assert_eq!(
            app.document.as_ref().map(|document| &document.kind),
            Some(&DocumentKind::File(archived))
        );
        assert_eq!(app.status, "Note archived");
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
        app.open_file_document(&path, DocumentReturn::Daily);

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
        app.open_file_document(&path, DocumentReturn::Daily);
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
        add_daily_note(&mut app, "file this");
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
    fn enter_confirms_file_deletion_dialog() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("DeleteMe.md");
        fs::write(&path, "delete me").unwrap();
        app.open_files();
        app.file_index = app
            .note_files
            .iter()
            .position(|file| file.path == path)
            .unwrap();
        app.sync_selected_file();

        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteFile));
        app.handle_key(key(KeyCode::Enter));

        assert!(!path.exists());
        assert_eq!(app.overlay, None);
        assert_eq!(app.status, "Deleted DeleteMe.md");
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
        app.open_file_document(&from, DocumentReturn::Daily);
        app.focus = Focus::Files;

        app.handle_key(key(KeyCode::Char('r')));
        if let Some(dialog) = app.dialog.as_mut() {
            dialog.input = "Renamed".to_string();
            dialog.cursor = dialog.input.chars().count();
        }
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
        app.open_file_document(&from, DocumentReturn::Daily);
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
    fn search_result_daily_edit_returns_physical_file_command() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "needle");
        refresh_test_index(&mut app);
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
        let expected = app
            .storage
            .daily_file_path(&app.daily_notes[0].date.to_string())
            .unwrap();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('e'))),
            Some(Command::Edit(expected))
        );
        assert_eq!(app.center_view, CenterView::Document);
    }

    #[test]
    fn file_search_result_keeps_its_source_line_as_a_document_anchor() {
        let (mut app, _directory) = make_app();
        let path = app.storage.data_dir.join("Project.md");
        fs::write(&path, "# Project\n\nintro\n\nunique needle\n").unwrap();
        app.reload_files();
        refresh_test_index(&mut app);
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
    fn full_text_search_orders_daily_active_and_archived_results() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "shared needle in Daily");
        let active = app.storage.data_dir.join("Active.md");
        let archived = app.storage.archives_dir.join("Archived.md");
        fs::write(&active, "shared needle in active note\n").unwrap();
        fs::write(&archived, "shared needle in archived note\n").unwrap();
        refresh_test_index(&mut app);

        app.open_search();
        app.handle_paste("shared needle");

        assert_eq!(app.search_results.len(), 3);
        let daily = app
            .storage
            .daily_file_path(&app.daily_notes[0].date.to_string())
            .unwrap();
        assert!(matches!(
            &app.search_results[0],
            SearchHit::FileLine { path, .. } if path == &daily
        ));
        assert!(matches!(
            &app.search_results[1],
            SearchHit::FileLine { path, .. } if path == &active
        ));
        assert!(matches!(
            &app.search_results[2],
            SearchHit::FileLine { path, .. } if path == &archived
        ));

        let daily_date = app.daily_notes[0].date;
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            app.document.as_ref(),
            Some(Document {
                kind: DocumentKind::Daily(date),
                target_line: Some(1),
                ..
            }) if *date == daily_date
        ));
    }

    #[test]
    fn daily_edit_returns_its_physical_file() {
        let (mut app, _directory) = make_app();
        add_daily_note(&mut app, "before");
        let expected = app
            .storage
            .daily_file_path(&app.daily_notes[0].date.to_string())
            .unwrap();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('e'))),
            Some(Command::Edit(expected))
        );
    }

    #[test]
    fn workspace_reload_refreshes_an_open_daily_note_from_disk() {
        let (mut app, _directory) = make_app();
        let note = app.storage.append_daily("2026-07-26", "before").unwrap();
        app.reload();
        app.open_daily_document(note.date, DocumentReturn::Daily);
        let path = app.storage.daily_file_path(&note.date.to_string()).unwrap();
        fs::write(path, "after\n").unwrap();

        app.reload_workspace();

        assert_eq!(app.document.as_ref().unwrap().source, "after");
        assert_eq!(app.daily_notes[0].body, "after");
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
        assert_eq!(app.center_view, CenterView::Daily);
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
    fn file_embed_clicks_open_existing_files_from_any_location() {
        let (mut app, _directory) = make_app();
        let attachment = app.storage.data_dir.join("report.pdf");
        fs::write(&attachment, b"report").unwrap();
        app.link_hitboxes.push(LinkHitbox {
            target: LinkTarget::EmbeddedFile(attachment.clone()),
            area: Rect::new(4, 3, 7, 1),
        });
        assert_eq!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }),
            Some(Command::OpenPath(fs::canonicalize(&attachment).unwrap()))
        );

        app.link_hitboxes.clear();
        app.link_hitboxes.push(LinkHitbox {
            target: LinkTarget::EmbeddedFile(app.storage.data_dir.join("missing.pdf")),
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
        assert!(app.status.starts_with("Embed error:"));

        let outside = tempfile::NamedTempFile::new().unwrap();
        app.link_hitboxes.clear();
        app.link_hitboxes.push(LinkHitbox {
            target: LinkTarget::EmbeddedFile(outside.path().to_path_buf()),
            area: Rect::new(4, 3, 7, 1),
        });
        assert_eq!(
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }),
            Some(Command::OpenPath(fs::canonicalize(outside.path()).unwrap()))
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
        app.center_view = CenterView::Daily;
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
        add_daily_note(&mut app, "file this");
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
        if let Some(dialog) = app.dialog.as_mut() {
            dialog.input = "Taken".to_string();
            dialog.cursor = dialog.input.chars().count();
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.files_context, FilesContext::Rename);
        assert!(app.pending_file.is_some());
        assert!(app.status.starts_with("Error:"));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_toggle_retains_one_session_and_shell_exit_discards_it() {
        let (mut app, _directory) = make_app();
        let toggle = KeyEvent::new(KeyCode::Char('`'), KeyModifiers::CONTROL);
        app.handle_key(toggle);
        assert_eq!(app.overlay, Some(Overlay::Terminal));
        let process_id = app.terminal_process_id();
        assert!(process_id.is_some());

        app.handle_key(toggle);
        assert_eq!(app.overlay, None);
        assert_eq!(app.terminal_process_id(), process_id);

        app.open_help();
        app.handle_key(toggle);
        assert_eq!(app.overlay, Some(Overlay::Terminal));
        assert_eq!(app.terminal_process_id(), process_id);
        app.handle_key(toggle);
        assert_eq!(app.overlay, Some(Overlay::Help));
        assert_eq!(app.terminal_process_id(), process_id);
        app.handle_key(key(KeyCode::Esc));

        app.handle_key(toggle);
        assert_eq!(app.overlay, Some(Overlay::Terminal));

        for character in "exit".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        app.handle_key(key(KeyCode::Enter));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while app.terminal_process_id().is_some() && std::time::Instant::now() < deadline {
            app.poll_terminal();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.terminal_process_id(), None);
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn command_palette_includes_workspace_terminal() {
        let (mut app, _directory) = make_app();
        app.open_command_palette();
        assert!(app.command_matches.contains(&AppCommand::OpenTerminal));
        assert!(app
            .dialog
            .as_ref()
            .unwrap()
            .options
            .iter()
            .any(|option| option.label == "Terminal: Open"));
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
        add_daily_note(&mut app, "remove me");
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteDaily));
        app.handle_key(key(KeyCode::Char('y')));
        assert!(app.daily_notes.is_empty());
        app.handle_key(key(KeyCode::Char('u')));
        assert_eq!(app.daily_notes.len(), 1);
        assert_eq!(app.daily_notes[0].body, "remove me");
    }
}
