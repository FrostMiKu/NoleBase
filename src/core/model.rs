//! Domain data model for daily notes, managed notes, tasks, and search.

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::NaiveDate;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    External(String),
    WikiLink(String),
    /// A local link or file embed, resolved against its containing document.
    LocalFile(PathBuf),
    /// A mutable attachment URI (`nole://attachment/<lowercase-hyphenated-uuid>`),
    /// resolved through the attachment store only when activated.
    Attachment(String),
    /// A rendered code-block action carrying the exact fenced source.
    CopyCode(String),
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

/// One distinct managed note shown as a full-body card in the tag card stream.
#[derive(Debug, Clone)]
pub struct TagNote {
    pub path: PathBuf,
    pub title: String,
    pub body: String,
    pub modified: SystemTime,
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

/// A `- [ ]` / `- [x]` task parsed from the daily notes.
#[derive(Debug, Clone)]
pub struct TodoItem {
    pub checked: bool,
    pub text: String,
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
