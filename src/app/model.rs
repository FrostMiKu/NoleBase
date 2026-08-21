//! Data-model enums and the command-palette definition table.

use std::path::PathBuf;

use ratatui::layout::Rect;

use super::fuzzy_match;
use crate::attachment::AttachmentId;
use crate::model::DailyNote;
use crate::storage::AppendReceipt;

/// A request for the terminal owner, which controls suspension and process exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Edit(PathBuf),
    OpenLink(String),
    OpenPath(PathBuf),
    CopyText(String),
    SetMouseCapture(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AppCommand {
    InterruptAgent,
    CopyLastAgentOutput,
    ClearAgentSession,
    OpenTerminal,
    ToggleMouseSupport,
    NewNote,
    NewNoteFromTemplate,
    EditTemplate,
    EditCurrentNote,
    ExportCurrentFile,
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
    BrowseSkills,
    BrowseAttachments,
    PasteClipboardAsAttachment,
}

pub(super) struct AppCommandDefinition {
    pub(super) id: AppCommand,
    pub(super) label: &'static str,
    pub(super) description: &'static str,
    pub(super) keywords: &'static str,
}

pub(super) const APP_COMMANDS: &[AppCommandDefinition] = &[
    AppCommandDefinition {
        id: AppCommand::InterruptAgent,
        label: "Agent: Interrupt task",
        description: "Stop the active Agent task",
        keywords: "agent interrupt cancel stop task",
    },
    AppCommandDefinition {
        id: AppCommand::CopyLastAgentOutput,
        label: "Agent: Copy last output",
        description: "Copy the latest Agent response to the clipboard",
        keywords: "agent copy last latest output response clipboard",
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
        id: AppCommand::ToggleMouseSupport,
        label: "Interface: Mouse support",
        description: "Toggle mouse support for interaction or terminal text selection",
        keywords: "interface mouse support enable disable selection select copy terminal",
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
        id: AppCommand::ExportCurrentFile,
        label: "File: Export…",
        description: "Publish the current file outside Nole",
        keywords: "file export original html publish current",
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
        id: AppCommand::BrowseSkills,
        label: "Skill: Browse",
        description: "Browse and preview Agent skills",
        keywords: "skill skills agent browse workflow instructions",
    },
    AppCommandDefinition {
        id: AppCommand::BrowseAttachments,
        label: "Attachments: Browse",
        description: "Browse attachments by name, type, size, and references",
        keywords: "attachment attachments browse files media open trash delete",
    },
    AppCommandDefinition {
        id: AppCommand::PasteClipboardAsAttachment,
        label: "Attachments: Paste from clipboard",
        description:
            "Import clipboard files or image into attachments and insert Markdown references",
        keywords: "attachment paste clipboard image file",
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

pub(super) fn command_definition(id: AppCommand) -> Option<&'static AppCommandDefinition> {
    APP_COMMANDS.iter().find(|command| command.id == id)
}

pub(super) fn command_match_score(command: &AppCommandDefinition, query: &str) -> Option<u8> {
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
pub(super) enum CursorMove {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
}

#[derive(Debug, Clone)]
pub(super) enum UndoOp {
    Append {
        receipt: AppendReceipt,
        input: String,
    },
    Delete(DailyNote),
    Move {
        daily_note: DailyNote,
        target: PathBuf,
        appended: String,
    },
}

/// One row of the attachment browser: store metadata joined with the number
/// of distinct managed notes that reference it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentEntry {
    pub id: AttachmentId,
    pub name: String,
    /// Short display type: media type or file extension when unrecognized.
    pub kind: String,
    pub size: u64,
    /// Distinct managed notes referencing the attachment (not occurrences).
    pub locations: usize,
}

/// Keyboard focus is independent from the content shown in the center pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Center,
    Compose,
    Files,
    Views,
    Agent,
}

/// Content currently occupying the center pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterView {
    Daily,
    Chat,
    Todo,
    Document,
    Search,
    DocumentSearch,
    Tags,
    Attachments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarSelection {
    Files,
    Views,
}

impl CenterView {
    /// Sidebar whose selection represents this center view while the center has
    /// focus. A directly focused sidebar still shows its own selection.
    pub const fn sidebar_selection(self) -> SidebarSelection {
        match self {
            Self::Document => SidebarSelection::Files,
            Self::Daily
            | Self::Chat
            | Self::Todo
            | Self::Search
            | Self::DocumentSearch
            | Self::Tags
            | Self::Attachments => SidebarSelection::Views,
        }
    }
}

/// A top-level page exposed by the workspace view switcher.
///
/// Adding a page starts here: register it in [`WorkspaceView::ALL`], then add
/// its center renderer and input handler. The sidebar reads this registry and
/// never owns its own list of pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceView {
    pub center_view: CenterView,
    pub label: &'static str,
    pub description: &'static str,
}

impl WorkspaceView {
    pub const ALL: &'static [Self] = &[
        Self {
            center_view: CenterView::Chat,
            label: "Agent",
            description: "AI conversation",
        },
        Self {
            center_view: CenterView::Todo,
            label: "TODO",
            description: "Tasks",
        },
        Self {
            center_view: CenterView::Search,
            label: "Search",
            description: "Find notes",
        },
        Self {
            center_view: CenterView::Tags,
            label: "Tag",
            description: "Browse tags",
        },
        Self {
            center_view: CenterView::Attachments,
            label: "Attachment",
            description: "Browse attachments",
        },
        Self {
            center_view: CenterView::Daily,
            label: "Daily",
            description: "Daily notes",
        },
    ];

    pub fn index_of(center_view: CenterView) -> Option<usize> {
        Self::ALL
            .iter()
            .position(|view| view.center_view == center_view)
    }
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
    PrivateTerminalInput,
    WikiLinkChoice,
    Terminal,
    /// A caller-provided command dialog. Its mode and purpose live in
    /// [`DialogState`], allowing new command-style interactions without
    /// adding another overlay variant.
    Dialog,
}

/// Screen geometry recorded by the renderer after each layout pass.
///
/// Mouse wheel events use these rectangles instead of the keyboard focus, so a
/// wheel over Files/Views/Center always affects the pane under the pointer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutSnapshot {
    pub files: Option<Rect>,
    pub center: Option<Rect>,
    pub compose: Option<Rect>,
    pub views: Option<Rect>,
    pub agent: Option<Rect>,
    pub overlay: Option<Rect>,
}
