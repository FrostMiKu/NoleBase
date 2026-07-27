//! Core data model: messages, actions, and button hitboxes.

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::{DateTime, Local};
use ratatui::layout::Rect;

/// One daily chat card. `id` is its `YYYY-MM-DD` date.
#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub created_at: DateTime<Local>,
    pub body: String,
}

/// The per-message actions exposed in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Ai,
    Move,
    Archive,
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
            Action::Archive => "archive",
            Action::New => "new",
            Action::View => "view",
            Action::Edit => "edit",
            Action::Delete => "del",
        }
    }

    /// Ordered list of actions rendered left to right on each message card.
    pub fn all() -> &'static [Action] {
        &[
            Action::Move,
            Action::Archive,
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
    pub message_id: String,
    pub action: Action,
    pub area: Rect,
}

/// A managed markdown file shown in the persistent file list.
#[derive(Debug, Clone)]
pub struct NoteFile {
    pub path: PathBuf,
    pub modified: SystemTime,
}

/// A recorded screen rectangle for a clickable file row in the sidebar.
#[derive(Debug, Clone)]
pub struct FileHitbox {
    pub path: PathBuf,
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

/// One content-search match.
#[derive(Debug, Clone)]
pub enum SearchHit {
    /// A chat message whose body matched.
    Message { id: String, text: String },
    /// A matching line in a `.md` file.
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
