//! Core data model for daily notes, regular notes, actions, and hitboxes.

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::NaiveDate;
use ratatui::layout::Rect;

/// One daily note backed by `daily/YYYY-MM-DD.md`.
#[derive(Debug, Clone)]
pub struct DailyNote {
    pub date: NaiveDate,
    pub body: String,
}

/// The actions exposed on each daily note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Ai,
    Move,
    New,
    View,
    Edit,
    Delete,
}

impl Action {
    /// The short label rendered on the button.
    pub fn label(self) -> &'static str {
        match self {
            Action::Ai => "AI",
            Action::Move => "move",
            Action::New => "new",
            Action::View => "view",
            Action::Edit => "edit",
            Action::Delete => "del",
        }
    }

    /// Ordered list of actions rendered left to right on each daily note.
    pub fn all() -> &'static [Action] {
        &[
            Action::Move,
            Action::New,
            Action::Edit,
            Action::Delete,
            Action::Ai,
        ]
    }
}

/// A recorded screen rectangle for a clickable button, rebuilt each frame.
#[derive(Debug, Clone)]
pub struct ButtonHitbox {
    pub date: NaiveDate,
    pub action: Action,
    pub area: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    External(String),
    WikiLink(String),
    EmbeddedFile(PathBuf),
    /// A content-addressed attachment URI
    /// (`nole-attachment://sha256/<64 lowercase hex>`), resolved through the
    /// attachment store only when activated.
    Attachment(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WikiLinkLocation {
    Daily,
    Notes,
    Archives,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLinkCandidate {
    pub path: PathBuf,
    pub location: WikiLinkLocation,
}

/// A recorded screen rectangle for a wikilink candidate in its chooser.
#[derive(Debug, Clone)]
pub struct WikiLinkHitbox {
    pub index: usize,
    pub area: Rect,
}

/// A selectable row inside the command dialog. The renderer rebuilds these
/// rectangles every frame so mouse and keyboard selection share one model.
#[derive(Debug, Clone)]
pub struct DialogOptionHitbox {
    pub index: usize,
    pub area: Rect,
}

/// One visible, clickable segment of a rendered Markdown link.
#[derive(Debug, Clone)]
pub struct LinkHitbox {
    pub target: LinkTarget,
    pub area: Rect,
}

/// One visible, clickable segment of a rendered Hashtag.
#[derive(Debug, Clone)]
pub struct TagHitbox {
    pub name: String,
    pub area: Rect,
}

/// A managed markdown file shown in the persistent file list.
#[derive(Debug, Clone)]
pub struct NoteFile {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub archived: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileGroup {
    Notes,
    Archives,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileListRow {
    Group(FileGroup),
    File(usize),
}

/// A recorded screen rectangle for a clickable file row in the sidebar.
#[derive(Debug, Clone)]
pub struct FileHitbox {
    pub path: PathBuf,
    pub area: Rect,
}

#[derive(Debug, Clone)]
pub struct FileGroupHitbox {
    pub group: FileGroup,
    pub area: Rect,
}

/// A `- [ ]` / `- [x]` task parsed from the daily notes.
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub checked: bool,
    pub text: String,
}

/// A recorded screen rectangle for a clickable todo row (its index into the
/// task list), rebuilt each frame.
#[derive(Debug, Clone)]
pub struct TodoHitbox {
    pub index: usize,
    pub area: Rect,
}

/// A recorded screen rectangle for a registered workspace view.
#[derive(Debug, Clone)]
pub struct WorkspaceViewHitbox {
    pub index: usize,
    pub area: Rect,
}

/// One content-search match.
#[derive(Debug, Clone)]
pub enum SearchHit {
    /// A matching line in a managed Markdown file.
    FileLine {
        path: std::path::PathBuf,
        line_no: usize,
        text: String,
    },
    /// A matching source line in the document currently being viewed.
    DocumentLine { line_no: usize, text: String },
}

/// A recorded screen rectangle for a clickable search result row.
#[derive(Debug, Clone)]
pub struct SearchHitbox {
    pub index: usize,
    pub area: Rect,
}

/// A recorded screen rectangle for a clickable attachment row in the browser.
#[derive(Debug, Clone)]
pub struct AttachmentHitbox {
    pub index: usize,
    pub area: Rect,
}
