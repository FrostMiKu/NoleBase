//! TUI mouse geometry: screen-rectangle hitboxes rebuilt by the renderer
//! every frame. Domain types live in [`crate::core::model`].

use std::path::PathBuf;

use chrono::NaiveDate;
use ratatui::layout::Rect;

use crate::core::model::{Action, FileGroup, LinkTarget};

/// A recorded screen rectangle for a clickable button, rebuilt each frame.
#[derive(Debug, Clone)]
pub struct ButtonHitbox {
    pub date: NaiveDate,
    pub action: Action,
    pub area: Rect,
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

/// A recorded screen rectangle for one tag-note card, rebuilt each frame.
#[derive(Debug, Clone)]
pub struct TagNoteHitbox {
    pub index: usize,
    pub area: Rect,
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

/// A recorded screen rectangle for a clickable backlink row in the document
/// view, rebuilt each frame.
#[derive(Debug, Clone)]
pub struct BacklinkHitbox {
    pub path: PathBuf,
    pub area: Rect,
}
