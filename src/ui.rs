//! Terminal rendering for the full-width workspace.

use std::collections::HashMap;
use std::path::Path;

mod agent;
mod attachments;
mod chat;
mod compose;
mod daily;
mod dialog;
mod diff;
mod document;
mod files;
mod footer;
mod input;
mod notification;
mod search;
#[cfg(test)]
mod skill_tests;
mod tags;
mod terminal;
#[cfg(test)]
mod tests;
mod todo;
mod util;
mod views;

use self::{
    agent::*, attachments::*, chat::*, compose::*, daily::*, dialog::*, diff::*, document::*,
    files::*, footer::*, input::*, notification::*, search::*, tags::*, terminal::*, todo::*,
    util::*, views::*,
};

use chrono::{DateTime, Local, NaiveDate};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agent::PermissionMode;
use crate::app::{
    human_size, App, CenterView, DialogMode, DialogPurpose, DialogState, FilesContext, Focus,
    LayoutSnapshot, Overlay, SidebarSelection, WorkspaceView,
};
use crate::embedded_terminal::{TerminalColor, TerminalSnapshot};
use crate::model::{
    Action, AttachmentHitbox, ButtonHitbox, FileGroup, FileGroupHitbox, FileHitbox, FileListRow,
    LinkHitbox, LinkTarget, SearchHit, SearchHitbox, TagHitbox, TodoHitbox, WorkspaceViewHitbox,
};
use crate::theme::Theme;

pub(in crate::ui) const DATE_FMT: &str = "%Y-%m-%d";
pub(in crate::ui) const DAILY_DATE_LABEL_WIDTH: usize = 10;
pub(in crate::ui) const WIDE_BREAKPOINT: u16 = 170;
pub(in crate::ui) const FILES_WIDTH: u16 = 33;
pub(in crate::ui) const RIGHT_SIDEBAR_WIDTH: u16 = 48;
pub(in crate::ui) const CENTER_MAX_WIDTH: u16 = 120;
pub(in crate::ui) const PANEL_PADDING: u16 = 1;
pub(in crate::ui) const DAILY_PADDING_X: usize = 1;
pub(in crate::ui) const PAGE_PADDING_X: usize = DAILY_PADDING_X + 12;
pub(in crate::ui) const DIALOG_WIDTH: u16 = 80;
pub(in crate::ui) const APPROVAL_UNIFIED_WIDTH: u16 = 110;
pub(in crate::ui) const APPROVAL_SIDE_BY_SIDE_WIDTH: u16 = 160;
pub(in crate::ui) const APPROVAL_SIDE_BY_SIDE_MIN_WIDTH: u16 = 140;
pub(in crate::ui) const SELECT_OPTION_HEIGHT: u16 = 2;
/// Minimum rows reserved for the agent panel header so its statistics line
/// stays visible when the workspace view list grows past the panel.
pub(in crate::ui) const MIN_AGENT_HEADER_ROWS: u16 = 3;
pub(in crate::ui) const WORKSPACE_VIEW_SPACED_HEIGHT: u16 = 3;
pub(in crate::ui) const WORKSPACE_VIEW_COMPACT_HEIGHT: u16 = 2;

/// Render one frame, rebuild mouse geometry, and return the requested cursor
/// position without changing the terminal's hardware cursor.
pub fn draw(frame: &mut Frame, app: &mut App) -> Option<Position> {
    app.layout = LayoutSnapshot::default();
    clear_hitboxes(app);
    let mut cursor_position = None;

    let root = frame.area();
    frame.render_widget(
        Block::default().style(
            Style::default()
                .fg(app.theme.text_primary)
                .bg(app.theme.surface_canvas),
        ),
        root,
    );
    let (body, footer) = body_and_footer(root);
    app.ensure_file_input_dialog();
    let file_input_modal = matches!(
        app.files_context,
        FilesContext::NewTarget | FilesContext::Rename
    );
    let interactive = app.overlay.is_none() && !file_input_modal;

    if root.width >= WIDE_BREAKPOINT {
        draw_wide_workspace(frame, app, body, interactive, &mut cursor_position);
    } else {
        draw_narrow_workspace(frame, app, body, interactive, &mut cursor_position);
    }
    draw_footer(frame, app, footer);
    if let Some(message) = app.notifications.visible() {
        draw_notification(frame, root, &message, app.theme);
    }

    if let Some(overlay) = app.overlay {
        // Background widgets may still be visible, but an overlay owns all input.
        // Keeping no base hitboxes makes that ownership explicit to mouse code.
        clear_hitboxes(app);
        let area = draw_overlay(frame, app, root, overlay, &mut cursor_position);
        app.layout.overlay = non_empty(area);
    }
    skip_vs16_continuation_cells(frame.buffer_mut());
    cursor_position
}

fn clear_hitboxes(app: &mut App) {
    app.hitboxes.clear();
    app.link_hitboxes.clear();
    app.tag_hitboxes.clear();
    app.wiki_link_hitboxes.clear();
    app.dialog_hitboxes.clear();
    app.file_hitboxes.clear();
    app.file_group_hitboxes.clear();
    app.todo_hitboxes.clear();
    app.workspace_view_hitboxes.clear();
    app.search_hitboxes.clear();
    app.attachment_hitboxes.clear();
    app.tag_note_hitboxes.clear();
}

fn body_and_footer(area: Rect) -> (Rect, Rect) {
    if area.height == 0 {
        return (area, Rect::new(area.x, area.y, area.width, 0));
    }
    (
        Rect::new(area.x, area.y, area.width, area.height - 1),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    )
}

fn draw_wide_workspace(
    frame: &mut Frame,
    app: &mut App,
    body: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    let files = Rect::new(body.x, body.y, FILES_WIDTH.min(body.width), body.height);
    let sidebar_width = RIGHT_SIDEBAR_WIDTH.min(body.width.saturating_sub(files.width));
    let sidebar = Rect::new(
        body.x + body.width.saturating_sub(sidebar_width),
        body.y,
        sidebar_width,
        body.height,
    );
    let center_region = Rect::new(
        files.x + files.width,
        body.y,
        body.width
            .saturating_sub(files.width)
            .saturating_sub(sidebar.width),
        body.height,
    );
    app.layout.files = non_empty(files);
    app.layout.center = non_empty(center_region);
    draw_files(frame, app, files, interactive, cursor_position);
    draw_center(frame, app, center_region, interactive, cursor_position);
    draw_right_sidebar(frame, app, sidebar, interactive);
}

fn workspace_views_height(area_height: u16) -> u16 {
    let maximum = area_height.saturating_sub(MIN_AGENT_HEADER_ROWS);
    let spacious = selection_list_height(
        WorkspaceView::ALL.len() as u16,
        WORKSPACE_VIEW_SPACED_HEIGHT,
    )
    .saturating_add(2);
    let item_height = if spacious <= maximum {
        WORKSPACE_VIEW_SPACED_HEIGHT
    } else {
        WORKSPACE_VIEW_COMPACT_HEIGHT
    };
    selection_list_height(WorkspaceView::ALL.len() as u16, item_height)
        .saturating_add(2)
        .min(maximum)
}

fn draw_right_sidebar(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
    let views_height = workspace_views_height(area.height);
    let agent = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(views_height),
    );
    let views = Rect::new(
        area.x,
        area.y.saturating_add(agent.height),
        area.width,
        views_height,
    );
    app.layout.agent = non_empty(agent);
    app.layout.views = non_empty(views);
    if app.center_view == CenterView::Chat {
        draw_agent_statistics(frame, app, agent);
    } else {
        draw_agent_output(frame, app, agent);
    }
    draw_workspace_views(frame, app, views, interactive);
}

fn draw_narrow_workspace(
    frame: &mut Frame,
    app: &mut App,
    body: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if app.focus == Focus::Files || app.files_context != FilesContext::Browse {
        app.layout.files = non_empty(body);
        draw_files(frame, app, body, interactive, cursor_position);
    } else if app.focus == Focus::Views {
        app.layout.views = non_empty(body);
        draw_workspace_views(frame, app, body, interactive);
    } else if app.focus == Focus::Agent {
        app.layout.agent = non_empty(body);
        if app.center_view == CenterView::Chat {
            draw_agent_statistics(frame, app, body);
        } else {
            draw_agent_output(frame, app, body);
        }
    } else {
        app.layout.center = non_empty(body);
        draw_center(frame, app, body, interactive, cursor_position);
    }
}

fn draw_center(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    let content = center_content_axis(area);
    match app.center_view {
        CenterView::Daily => draw_daily(frame, app, area, content, interactive, cursor_position),
        CenterView::Chat => draw_chat(frame, app, area, content, interactive, cursor_position),
        CenterView::Todo => draw_todo(frame, app, content, interactive, cursor_position),
        CenterView::Document => draw_document(frame, app, content, interactive, cursor_position),
        CenterView::Search | CenterView::DocumentSearch => {
            draw_search(frame, app, content, interactive, cursor_position)
        }
        CenterView::Tags => draw_tags(frame, app, content, interactive, cursor_position),
        CenterView::Attachments => {
            draw_attachments(frame, app, content, interactive, cursor_position)
        }
    }
}

fn draw_overlay(
    frame: &mut Frame,
    app: &mut App,
    root: Rect,
    overlay: Overlay,
    cursor_position: &mut Option<Position>,
) -> Rect {
    match overlay {
        Overlay::Terminal => draw_terminal(frame, app, root, cursor_position),
        _ => draw_dialog(frame, app, root, cursor_position),
    }
}
