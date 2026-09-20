//! Transport DTOs for the web API. Core models never gain `Serialize` for
//! the web; every wire shape is defined here and mapped explicitly, so core
//! types can evolve without becoming a public API contract. Paths on the wire
//! are workspace-relative, `/`-separated strings — absolute local paths must
//! not leak (the fallback keeps only the file name).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::agent::{AgentEvent, AgentStopReason, ApprovalKind};
use crate::core::agent_session::{AgentPanelEntry, TokenUsage};
use crate::core::model::{DailyNote, NoteFile, SearchHit, TodoItem};
use crate::core::storage::Storage;
use crate::core::workspace::Workspace;
use crate::core::workspace_index::{TagDocument, TagSummary};
use crate::core::workspace_watcher::WorkspaceEvent;

/// `GET /api/health`
#[derive(Serialize)]
pub struct HealthDto {
    pub ok: bool,
    pub version: &'static str,
}

/// `GET /api/workspace` — cheap bootstrap counts.
#[derive(Serialize)]
pub struct WorkspaceOverviewDto {
    pub notes: usize,
    pub archives: usize,
    pub daily: usize,
    pub todos: usize,
    pub index_ready: bool,
}

/// One managed note in `GET /api/notes`.
#[derive(Serialize)]
pub struct NoteSummaryDto {
    pub path: String,
    pub name: String,
    pub archived: bool,
    pub modified: Option<String>,
}

/// One daily note in `GET /api/daily`. `path` is the workspace-relative
/// document address so a client can open the note without guessing the
/// daily file naming convention.
#[derive(Serialize)]
pub struct DailyNoteDto {
    pub date: String,
    pub body: String,
    pub path: String,
}

/// One `- [ ]` task in `GET /api/todos`.
#[derive(Serialize)]
pub struct TodoDto {
    pub checked: bool,
    pub text: String,
}

/// One content-search match in `GET /api/search`.
#[derive(Serialize)]
pub struct SearchHitDto {
    pub path: Option<String>,
    pub line_no: usize,
    pub text: String,
}

/// One tag summary in `GET /api/tags`.
#[derive(Serialize)]
pub struct TagSummaryDto {
    pub name: String,
    pub documents: usize,
    pub mentions: usize,
}

/// One document containing a tag in `GET /api/tags/{tag}`.
#[derive(Serialize)]
pub struct TagDocumentDto {
    pub path: String,
    pub modified: Option<String>,
}

/// `GET /api/document` response.
#[derive(Serialize)]
pub struct DocumentDto {
    pub path: String,
    pub source: String,
}

/// `PUT /api/document` request. Document storage has no revision token, so
/// there is no optimistic-concurrency field; a write replaces the current
/// content unconditionally.
#[derive(Deserialize)]
pub struct DocumentWriteRequest {
    pub path: String,
    pub source: String,
}

/// `POST /api/notes` request: the user-entered note name, optionally seeded
/// from the workspace template instead of the default heading.
#[derive(Deserialize)]
pub struct CreateNoteRequest {
    pub name: String,
    #[serde(default)]
    pub from_template: bool,
}

/// `POST /api/document/rename` request. `new_name` is a bare file name; the
/// workspace owns normalization, collision checks, and wikilink migration.
#[derive(Deserialize)]
pub struct RenameDocumentRequest {
    pub path: String,
    pub new_name: String,
}

/// `POST /api/document/archive` and `POST /api/document/restore` request: the
/// managed document addressed by workspace-relative path.
#[derive(Deserialize)]
pub struct DocumentPathRequest {
    pub path: String,
}

/// `POST /api/daily/{date}/move` request: the note the day's content is
/// filed into, addressed by workspace-relative path.
#[derive(Deserialize)]
pub struct MoveDailyRequest {
    pub target_path: String,
}

/// Shared mutation response: the canonical workspace-relative path of the
/// affected document as produced by the workspace — clients never guess file
/// extensions, directories, or daily file naming.
#[derive(Serialize)]
pub struct DocumentMutationDto {
    pub path: String,
}

/// Token usage embedded in the agent `usage` event.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsageDto {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl From<TokenUsage> for TokenUsageDto {
    fn from(usage: TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
        }
    }
}

/// One settled transcript entry in `GET /api/agent/history`. History entries
/// are persisted snapshots, so the live-only fields (`streaming`, `active`)
/// of `AgentPanelEntry` are dropped from the wire shape.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "entry", rename_all = "snake_case")]
pub enum AgentHistoryEntryDto {
    Prompt {
        text: String,
    },
    Assistant {
        text: String,
    },
    Thinking {
        text: String,
    },
    Tool {
        text: String,
        preview: Option<String>,
    },
    Error(String),
}

impl From<&AgentPanelEntry> for AgentHistoryEntryDto {
    fn from(entry: &AgentPanelEntry) -> Self {
        match entry {
            AgentPanelEntry::Prompt { text, .. } => Self::Prompt { text: text.clone() },
            AgentPanelEntry::Assistant { text, .. } => Self::Assistant { text: text.clone() },
            AgentPanelEntry::Thinking { text, .. } => Self::Thinking { text: text.clone() },
            AgentPanelEntry::Tool { text, preview, .. } => Self::Tool {
                text: text.clone(),
                preview: preview.clone(),
            },
            AgentPanelEntry::Error(text) => Self::Error(text.clone()),
        }
    }
}

/// `GET /api/agent/history` — the persisted session transcript for
/// (re)connecting clients, plus the conversation length so the client can
/// show session context without a `conversation_updated` event.
#[derive(Serialize)]
pub struct AgentHistoryDto {
    pub entries: Vec<AgentHistoryEntryDto>,
    pub message_count: usize,
}

/// How an approval request is presented. Mirrors [`ApprovalKind`] with the
/// command variant inlined as fields.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalKindDto {
    Diff,
    Confirm,
    DestructiveConfirm,
    Command {
        purpose: String,
        label: String,
        code: String,
    },
}

/// The workspace event stream (`/api/ws/workspace`). One JSON object per
/// watcher batch; `paths` are workspace-relative.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebWorkspaceEvent {
    ContentChanged {
        paths: Vec<String>,
    },
    AttachmentsChanged {
        paths: Vec<String>,
    },
    AgentReadsDirty {
        paths: Vec<String>,
    },
    WatcherFailed {
        message: String,
    },
    /// A consumer was too slow and skipped events; clients should resync
    /// their workspace state over HTTP.
    ResyncRequired {
        skipped: u64,
    },
}

impl WebWorkspaceEvent {
    pub fn from_workspace_event(workspace: &Workspace, event: &WorkspaceEvent) -> Self {
        match event {
            WorkspaceEvent::ContentChanged { markdown_paths } => Self::ContentChanged {
                paths: markdown_paths
                    .iter()
                    .map(|path| relative_path(workspace, path))
                    .collect(),
            },
            WorkspaceEvent::AttachmentsChanged { paths } => Self::AttachmentsChanged {
                paths: paths
                    .iter()
                    .map(|path| relative_path(workspace, path))
                    .collect(),
            },
            WorkspaceEvent::AgentReadsDirty { paths } => Self::AgentReadsDirty {
                paths: paths
                    .iter()
                    .map(|path| relative_path(workspace, path))
                    .collect(),
            },
            WorkspaceEvent::WatcherFailed(message) => Self::WatcherFailed {
                message: scrub_workspace_paths(workspace, message),
            },
        }
    }
}

/// The agent event stream (`/api/ws/agent`). Every [`AgentEvent`] variant is
/// mapped; none is silently dropped. [`AgentEvent::ConversationUpdated`]
/// carries the full provider-message conversation for caller-side
/// continuation, which is deliberately not a web contract — the web layer
/// keeps its own session state, so only the shape summary crosses the wire.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebAgentEvent {
    AssistantDelta {
        text: String,
    },
    AssistantMessageFinished {
        text: String,
        final_output: bool,
    },
    ThinkingDelta {
        text: String,
    },
    ThinkingFinished,
    ToolPreparing {
        index: usize,
        message: String,
    },
    ToolPreparationFinished {
        index: usize,
        id: Option<String>,
    },
    JobStarted {
        id: String,
        label: String,
    },
    JobSettled {
        id: String,
        failed: bool,
    },
    BufferedInputConsumed {
        count: usize,
    },
    JobDelivered {
        labels: Vec<String>,
    },
    ToolStarted {
        id: String,
        message: String,
    },
    ToolUpdated {
        id: String,
        message: String,
    },
    ToolFinished {
        id: String,
        message: String,
        preview: Option<String>,
    },
    Usage {
        usage: TokenUsageDto,
    },
    ContextWindow {
        tokens: u64,
        capacity: u64,
    },
    ResponseTiming {
        output_tokens: u64,
        elapsed_ms: u64,
    },
    Retry,
    Round {
        current: u32,
    },
    ConversationUpdated {
        message_count: usize,
    },
    Notification {
        message: String,
    },
    FileMoved {
        from: String,
        to: String,
    },
    OpenFile {
        path: String,
    },
    ApprovalRequested {
        title: String,
        message: String,
        kind: ApprovalKindDto,
    },
    AskUserRequested {
        question: String,
        options: Vec<String>,
    },
    TerminalInputRequested {
        session_id: String,
        purpose: String,
        prompt: String,
    },
    Stopped {
        reason: String,
    },
    Finished {
        ok: bool,
        text: Option<String>,
        error: Option<String>,
    },
    /// A consumer was too slow and skipped events. Approval or ask-user
    /// requests may have been missed; clients should resync over HTTP.
    ResyncRequired {
        skipped: u64,
    },
}

impl WebAgentEvent {
    pub fn from_agent_event(workspace: &Workspace, event: &AgentEvent) -> Self {
        match event {
            AgentEvent::AssistantDelta(text) => Self::AssistantDelta { text: text.clone() },
            AgentEvent::AssistantMessageFinished { text, final_output } => {
                Self::AssistantMessageFinished {
                    text: text.clone(),
                    final_output: *final_output,
                }
            }
            AgentEvent::ThinkingDelta(text) => Self::ThinkingDelta { text: text.clone() },
            AgentEvent::ThinkingFinished => Self::ThinkingFinished,
            AgentEvent::ToolPreparing { index, message } => Self::ToolPreparing {
                index: *index,
                message: message.clone(),
            },
            AgentEvent::ToolPreparationFinished { index, id } => Self::ToolPreparationFinished {
                index: *index,
                id: id.clone(),
            },
            AgentEvent::JobStarted { id, label } => Self::JobStarted {
                id: id.clone(),
                label: label.clone(),
            },
            AgentEvent::JobSettled { id, failed } => Self::JobSettled {
                id: id.clone(),
                failed: *failed,
            },
            AgentEvent::BufferedInputConsumed(count) => {
                Self::BufferedInputConsumed { count: *count }
            }
            AgentEvent::JobDelivered(labels) => Self::JobDelivered {
                labels: labels.clone(),
            },
            AgentEvent::ToolStarted { id, message } => Self::ToolStarted {
                id: id.clone(),
                message: message.clone(),
            },
            AgentEvent::ToolUpdated { id, message } => Self::ToolUpdated {
                id: id.clone(),
                message: message.clone(),
            },
            AgentEvent::ToolFinished {
                id,
                message,
                preview,
            } => Self::ToolFinished {
                id: id.clone(),
                message: message.clone(),
                preview: preview.clone(),
            },
            AgentEvent::Usage(usage) => Self::Usage {
                usage: (*usage).into(),
            },
            AgentEvent::ContextWindow { tokens, capacity } => Self::ContextWindow {
                tokens: *tokens,
                capacity: *capacity,
            },
            AgentEvent::ResponseTiming {
                output_tokens,
                elapsed,
            } => Self::ResponseTiming {
                output_tokens: *output_tokens,
                elapsed_ms: elapsed.as_millis().min(u64::MAX as u128) as u64,
            },
            AgentEvent::Retry => Self::Retry,
            AgentEvent::Round { current } => Self::Round { current: *current },
            AgentEvent::ConversationUpdated(conversation) => Self::ConversationUpdated {
                // `messages` is crate-visible; the count is all the wire
                // contract needs.
                message_count: conversation.messages.len(),
            },
            AgentEvent::Notification(message) => Self::Notification {
                message: scrub_workspace_paths(workspace, message),
            },
            AgentEvent::FileMoved { from, to } => Self::FileMoved {
                from: relative_path(workspace, from),
                to: relative_path(workspace, to),
            },
            AgentEvent::OpenFile(path) => Self::OpenFile {
                path: relative_path(workspace, path),
            },
            AgentEvent::Approval(request) => Self::ApprovalRequested {
                title: request.title.clone(),
                message: request.message.clone(),
                kind: match &request.kind {
                    ApprovalKind::Diff => ApprovalKindDto::Diff,
                    ApprovalKind::Confirm => ApprovalKindDto::Confirm,
                    ApprovalKind::DestructiveConfirm => ApprovalKindDto::DestructiveConfirm,
                    ApprovalKind::Command(command) => ApprovalKindDto::Command {
                        purpose: command.purpose.clone(),
                        label: command.label.clone(),
                        code: command.code.clone(),
                    },
                },
            },
            AgentEvent::AskUser(request) => Self::AskUserRequested {
                question: request.question.clone(),
                options: request.options.clone(),
            },
            AgentEvent::PrivateTerminalInput(request) => Self::TerminalInputRequested {
                session_id: request.session_id.clone(),
                purpose: request.purpose.clone(),
                prompt: request.prompt.clone(),
            },
            AgentEvent::Stopped(reason) => Self::Stopped {
                reason: match reason {
                    AgentStopReason::ToolApprovalDenied => "tool_approval_denied".to_string(),
                },
            },
            AgentEvent::Finished(result) => Self::Finished {
                ok: result.is_ok(),
                text: result
                    .as_ref()
                    .ok()
                    .map(|text| scrub_workspace_paths(workspace, text)),
                error: result
                    .as_ref()
                    .err()
                    .map(|error| scrub_workspace_paths(workspace, error)),
            },
        }
    }
}

/// Workspace-relative form of `path` for the wire. Outside paths reduce to
/// their file name so absolute local paths never leak.
pub fn relative_path(workspace: &Workspace, path: &std::path::Path) -> String {
    workspace.relative_path(path).unwrap_or_else(|| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Strip absolute workspace-root prefixes from a free-text message that
/// originates inside core (error chains, watcher diagnostics), so the wire
/// never carries local absolute paths. Both the lexical and the canonical
/// root spelling are replaced. Streamed model deltas are not scrubbed — a
/// path split across chunk boundaries would defeat it — but tool and
/// approval messages are root-relative by core design.
pub fn scrub_workspace_paths(workspace: &Workspace, text: &str) -> String {
    let root = workspace.storage().root.clone();
    let canonical = std::fs::canonicalize(&root).ok();
    let mut scrubbed = text.to_string();
    for spelling in std::iter::once(root).chain(canonical) {
        let spelling = spelling.to_string_lossy().into_owned();
        if spelling.is_empty() {
            continue;
        }
        scrubbed = scrubbed
            .replace(format!("{spelling}/").as_str(), "")
            .replace(format!("{spelling}\\").as_str(), "")
            .replace(&spelling, ".");
    }
    scrubbed
}

/// RFC 3339 timestamp for a file mtime. SystemTime values from filesystem
/// metadata are always representable, so the conversion is infallible.
pub fn modified_rfc3339(modified: std::time::SystemTime) -> Option<String> {
    Some(DateTime::<Utc>::from(modified).to_rfc3339())
}

impl WorkspaceOverviewDto {
    pub fn from_workspace(workspace: &Workspace) -> anyhow::Result<Self> {
        let files = workspace.note_files()?;
        let daily = workspace.daily_notes()?;
        let todos = workspace.todos()?;
        Ok(Self {
            notes: files.iter().filter(|file| !file.archived).count(),
            archives: files.iter().filter(|file| file.archived).count(),
            daily: daily.len(),
            todos: todos.len(),
            index_ready: workspace.workspace_index().with_index(|_| ()).is_some(),
        })
    }
}

impl NoteSummaryDto {
    pub fn from_note_file(workspace: &Workspace, file: &NoteFile) -> Self {
        Self {
            path: relative_path(workspace, &file.path),
            name: file
                .path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            archived: file.archived,
            modified: modified_rfc3339(file.modified),
        }
    }
}

impl DailyNoteDto {
    pub fn from_daily_note(workspace: &Workspace, note: &DailyNote) -> Self {
        let absolute = workspace
            .storage()
            .daily_dir
            .join(Storage::daily_file_name(note.date));
        Self {
            date: note.date.to_string(),
            body: note.body.clone(),
            path: relative_path(workspace, &absolute),
        }
    }
}

impl TodoDto {
    pub fn from_todo_item(item: &TodoItem) -> Self {
        Self {
            checked: item.checked,
            text: item.text.clone(),
        }
    }
}

impl SearchHitDto {
    pub fn from_search_hit(workspace: &Workspace, hit: &SearchHit) -> Self {
        match hit {
            SearchHit::FileLine {
                path,
                line_no,
                text,
            } => Self {
                path: Some(relative_path(workspace, path)),
                line_no: *line_no,
                text: text.clone(),
            },
            SearchHit::DocumentLine { line_no, text } => Self {
                path: None,
                line_no: *line_no,
                text: text.clone(),
            },
        }
    }
}

impl TagSummaryDto {
    pub fn from_tag_summary(summary: &TagSummary) -> Self {
        Self {
            name: summary.name.clone(),
            documents: summary.documents,
            mentions: summary.mentions,
        }
    }
}

impl TagDocumentDto {
    pub fn from_tag_document(workspace: &Workspace, document: &TagDocument) -> Self {
        Self {
            path: relative_path(workspace, &document.path),
            modified: modified_rfc3339(document.modified),
        }
    }
}
