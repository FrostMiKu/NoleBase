//! Terminal rendering for the full-width workspace.

use std::collections::HashMap;
use std::path::Path;

#[cfg(test)]
mod tests;
mod diff;

use self::diff::*;

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
    App, CenterView, DialogMode, DialogPurpose, DialogState, FilesContext, Focus, LayoutSnapshot,
    Overlay,
};
use crate::embedded_terminal::TerminalSnapshot;
use crate::model::{
    Action, ButtonHitbox, FileGroup, FileGroupHitbox, FileHitbox, FileListRow, LinkHitbox,
    LinkTarget, SearchHit, SearchHitbox, TagHitbox, TodoHitbox,
};
use crate::theme::Theme;

const DATE_FMT: &str = "%Y-%m-%d";
const WIDE_BREAKPOINT: u16 = 170;
const FILES_WIDTH: u16 = 33;
const RIGHT_SIDEBAR_WIDTH: u16 = 48;
const CENTER_MAX_WIDTH: u16 = 120;
const PANEL_PADDING: u16 = 1;
const DAILY_PADDING_X: usize = 1;
const PAGE_PADDING_X: usize = DAILY_PADDING_X + 12;
const DIALOG_WIDTH: u16 = 80;
const APPROVAL_UNIFIED_WIDTH: u16 = 110;
const APPROVAL_SIDE_BY_SIDE_WIDTH: u16 = 160;
const APPROVAL_SIDE_BY_SIDE_MIN_WIDTH: u16 = 140;
const SELECT_OPTION_HEIGHT: u16 = 2;


/// Clear a widget's rectangle without leaving a wide-character continuation
/// cell from the content underneath it. Ratatui's diff buffer can otherwise
/// miss the cell next to a one-column border when a CJK glyph straddles that
/// boundary.
fn clear_widget(frame: &mut Frame, area: Rect) {
    sanitize_floating_widget_sides(frame, area);
    frame.render_widget(Clear, area);
}

/// Erase only wide glyphs that actually cross an opaque widget's vertical
/// boundary. Continuation cells are deliberately left to Ratatui's normal
/// wide-cell diff instead of being emitted with their reset background.
fn sanitize_floating_widget_sides(frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let buffer = frame.buffer_mut();
    let bounds = buffer.area;
    let inside_right = area.x.saturating_add(area.width).saturating_sub(1);
    let bottom = area.y.saturating_add(area.height);

    for y in area.y..bottom {
        if area.x > bounds.x {
            clear_wide_cell(buffer, bounds, area.x - 1, y);
        }
        if inside_right != area.x {
            clear_wide_cell(buffer, bounds, inside_right, y);
        }
    }
}

fn clear_wide_cell(buffer: &mut Buffer, bounds: Rect, x: u16, y: u16) {
    let in_bounds = x >= bounds.x
        && y >= bounds.y
        && x < bounds.x.saturating_add(bounds.width)
        && y < bounds.y.saturating_add(bounds.height);
    if !in_bounds {
        return;
    }
    let cell = &mut buffer[(x, y)];
    if cell.symbol().width() > 1 {
        cell.set_symbol(" ").set_diff_option(CellDiffOption::None);
    }
}

/// Prevent Ratatui's VS16-specific diff path from emitting reset-style
/// continuation cells. Crossterm can otherwise paint a default-background
/// block after the emoji and shift the remainder of that terminal row.
fn skip_vs16_continuation_cells(buffer: &mut Buffer) {
    let area = buffer.area;
    let right = area.x.saturating_add(area.width);
    let bottom = area.y.saturating_add(area.height);

    for y in area.y..bottom {
        for x in area.x..right {
            let (cell_width, needs_skip) = {
                let cell = &buffer[(x, y)];
                let symbol = cell.symbol();
                (
                    UnicodeWidthStr::width(symbol)
                        .max(1)
                        .min((right - x) as usize),
                    symbol.contains('\u{fe0f}')
                        && matches!(
                            cell.diff_option,
                            CellDiffOption::None | CellDiffOption::AlwaysUpdate
                        ),
                )
            };
            if needs_skip && cell_width > 1 {
                for offset in 1..cell_width {
                    buffer[(x + offset as u16, y)].set_diff_option(CellDiffOption::Skip);
                }
            }
        }
    }
}

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
    app.search_hitboxes.clear();
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
    let todo_width = RIGHT_SIDEBAR_WIDTH.min(body.width.saturating_sub(files.width));
    let todo = Rect::new(
        body.x + body.width.saturating_sub(todo_width),
        body.y,
        todo_width,
        body.height,
    );
    let center_region = Rect::new(
        files.x + files.width,
        body.y,
        body.width
            .saturating_sub(files.width)
            .saturating_sub(todo.width),
        body.height,
    );
    app.layout.files = non_empty(files);
    app.layout.center = non_empty(center_region);
    draw_files(frame, app, files, interactive, cursor_position);
    draw_center(frame, app, center_region, interactive, cursor_position);
    draw_right_sidebar(frame, app, todo, interactive);
}

fn draw_right_sidebar(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
    let todo_height = area.height.div_ceil(3);
    let todo = Rect::new(area.x, area.y, area.width, todo_height);
    let agent = Rect::new(
        area.x,
        area.y.saturating_add(todo_height),
        area.width,
        area.height.saturating_sub(todo_height),
    );
    app.layout.todo = non_empty(todo);
    app.layout.agent = non_empty(agent);
    draw_todo(frame, app, todo, interactive);
    draw_agent_output(frame, app, agent);
}

fn draw_agent_output(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let title = if app.agent_round_limit == 0 {
        " Agent ".to_string()
    } else {
        format!(" Agent · ↻{}/{} ", app.agent_round, app.agent_round_limit,)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(title)
        .style(Style::default().bg(app.theme.surface_panel))
        .border_style(focus_border(app.focus == Focus::Agent, app.theme));
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if app.ai_running {
        draw_animated_border(frame, area, app.animation_tick, app.theme);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if let Some(stats) = agent_stats_line(app, inner.width) {
        frame.render_widget(
            Paragraph::new(stats),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        inner.y = inner.y.saturating_add(1);
        inner.height = inner.height.saturating_sub(1);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if app.agent_panel.is_empty() {
        app.agent_vlist.caches.clear();
        app.agent_vlist.geometry.resize(0);
        return;
    }
    let width = inner.width as usize;
    let view_height = inner.height as usize;
    sync_agent_vlist(app, width);
    let tail_pinned = app.agent_scroll == u16::MAX;
    let mut scroll =
        (app.agent_scroll as usize).min(app.agent_vlist.geometry.max_scroll(view_height));
    scroll = measure_visible_agent_entries(app, scroll, view_height, tail_pinned);
    app.agent_scroll = scroll.min(u16::MAX as usize) as u16;
    let (visible, rendered_links, rendered_images) = visible_agent_lines(app, scroll, view_height);
    let message_rows = Rect::new(
        area.x.saturating_add(1),
        inner.y,
        area.width.saturating_sub(2),
        inner.height,
    );
    fill_agent_message_rows(frame, message_rows, &visible);
    frame.render_widget(Paragraph::new(visible), inner);
    let image_base = app.storage.root.clone();
    app.images.render(
        frame,
        &rendered_images,
        inner,
        scroll,
        &image_base,
        app.theme,
    );
    register_link_hitboxes(
        &mut app.link_hitboxes,
        &rendered_links,
        inner,
        scroll,
        &image_base,
    );
}

fn sync_agent_vlist(app: &mut App, width: usize) {
    if app.agent_vlist.width != width {
        app.agent_vlist.width = width;
        app.agent_vlist.geometry = crate::vlist::VList::new(4);
        app.agent_vlist.caches.clear();
    }
    app.agent_vlist.caches.resize(app.agent_panel.len(), None);
    app.agent_vlist.geometry.resize(app.agent_panel.len());
    for (index, cache) in app.agent_vlist.caches.iter_mut().enumerate() {
        if cache
            .as_ref()
            .is_some_and(|cached| cached.entry != app.agent_panel[index])
        {
            *cache = None;
            app.agent_vlist.geometry.invalidate(index);
        }
    }
}

fn ensure_agent_entry_rendered(app: &mut App, index: usize) {
    if app.agent_vlist.caches[index].is_some() {
        return;
    }
    let entry = app.agent_panel[index].clone();
    let (lines, links, images) = render_agent_entry(
        &entry,
        app.agent_vlist.width,
        app.animation_tick,
        false,
        app.theme,
    );
    let height = lines.len();
    app.agent_vlist.caches[index] = Some(crate::app::AgentEntryRenderCache {
        width: app.agent_vlist.width,
        entry,
        lines,
        links,
        images,
    });
    app.agent_vlist.geometry.set_height(index, height);
}

fn measure_visible_agent_entries(
    app: &mut App,
    mut scroll: usize,
    view_height: usize,
    tail_pinned: bool,
) -> usize {
    loop {
        let range = app.agent_vlist.geometry.visible_range(scroll, view_height);
        if range.is_empty() {
            return 0;
        }
        let anchor = range.start;
        let anchor_offset = scroll.saturating_sub(app.agent_vlist.geometry.item_top(anchor));
        let missing = range
            .clone()
            .filter(|index| !app.agent_vlist.geometry.is_measured(*index))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            let maximum = app.agent_vlist.geometry.max_scroll(view_height);
            return if tail_pinned {
                maximum
            } else {
                scroll.min(maximum)
            };
        }
        for index in missing {
            ensure_agent_entry_rendered(app, index);
        }
        scroll = if tail_pinned {
            app.agent_vlist.geometry.max_scroll(view_height)
        } else {
            let height = app.agent_vlist.geometry.height(anchor);
            app.agent_vlist.geometry.item_top(anchor) + anchor_offset.min(height.saturating_sub(1))
        };
        scroll = scroll.min(app.agent_vlist.geometry.max_scroll(view_height));
    }
}

fn render_agent_entry(
    entry: &crate::agent_session::AgentPanelEntry,
    width: usize,
    tick: u64,
    animate: bool,
    theme: Theme,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let mut lines = Vec::new();
    let mut links = Vec::new();
    let mut images = Vec::new();
    match entry {
        crate::agent_session::AgentPanelEntry::Prompt { text, muted } => {
            let background = theme.surface_message_user;
            lines.push(agent_message_line(Line::default(), width, background));
            lines.push(agent_message_line(
                Line::from(Span::styled(
                    "User",
                    Style::default()
                        .fg(if *muted {
                            theme.text_muted
                        } else {
                            theme.ui_agent_user
                        })
                        .add_modifier(Modifier::BOLD),
                )),
                width,
                background,
            ));
            let row = lines.len();
            let mut rendered = crate::markdown::render_at_width(text, width, theme);
            if *muted {
                for line in &mut rendered.lines {
                    for span in &mut line.spans {
                        span.style = span.style.fg(theme.text_muted);
                    }
                }
            }
            links.extend(rendered.links.into_iter().map(|mut link| {
                link.row += row;
                link
            }));
            images.extend(rendered.images.into_iter().map(|mut image| {
                image.row += row;
                image
            }));
            lines.extend(
                rendered
                    .lines
                    .into_iter()
                    .map(|line| agent_message_line(line, width, background)),
            );
            lines.push(agent_message_line(Line::default(), width, background));
        }
        crate::agent_session::AgentPanelEntry::Assistant { text, .. } => {
            let background = theme.surface_message_agent;
            lines.push(agent_message_line(Line::default(), width, background));
            lines.push(agent_message_line(
                Line::from(Span::styled(
                    "Agent",
                    Style::default()
                        .fg(theme.ui_agent_assistant)
                        .add_modifier(Modifier::BOLD),
                )),
                width,
                background,
            ));
            let row = lines.len();
            let rendered = crate::markdown::render_at_width(text, width, theme);
            links.extend(rendered.links.into_iter().map(|mut link| {
                link.row += row;
                link
            }));
            images.extend(rendered.images.into_iter().map(|mut image| {
                image.row += row;
                image
            }));
            lines.extend(
                rendered
                    .lines
                    .into_iter()
                    .map(|line| agent_message_line(line, width, background)),
            );
            lines.push(agent_message_line(Line::default(), width, background));
        }
        crate::agent_session::AgentPanelEntry::Tool { text, active } => {
            lines.extend(if *active && animate {
                animated_activity_lines(text, width, tick, theme)
            } else {
                activity_lines(text, width, theme)
            });
        }
        crate::agent_session::AgentPanelEntry::Error(text) => lines.push(Line::from(Span::styled(
            text.clone(),
            Style::default().fg(theme.ui_error),
        ))),
    }
    (lines, links, images)
}

fn agent_message_line(line: Line<'static>, width: usize, background: Color) -> Line<'static> {
    let mut line = line_with_background(line.spans, width, Style::default().bg(background));
    line.style = line.style.bg(background);
    line
}

fn fill_agent_message_rows(frame: &mut Frame, area: Rect, lines: &[Line<'_>]) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for (offset, line) in lines.iter().take(area.height as usize).enumerate() {
        let Some(background) = line.style.bg else {
            continue;
        };
        let y = area.y.saturating_add(offset as u16);
        for x in area.x..area.x.saturating_add(area.width) {
            frame.buffer_mut()[(x, y)]
                .set_symbol(" ")
                .set_style(Style::default().bg(background));
        }
    }
}

fn visible_agent_lines(
    app: &mut App,
    scroll: usize,
    view_height: usize,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let end = scroll.saturating_add(view_height);
    let mut visible = Vec::with_capacity(view_height);
    let mut links = Vec::new();
    let mut images = Vec::new();
    let range = app.agent_vlist.geometry.visible_range(scroll, view_height);
    for index in range {
        let item_top = app.agent_vlist.geometry.item_top(index);
        let height = app.agent_vlist.geometry.height(index);
        let cached = app.agent_vlist.caches[index]
            .as_ref()
            .expect("visible Agent entries are measured before rendering");
        let active = matches!(
            cached.entry,
            crate::agent_session::AgentPanelEntry::Tool { active: true, .. }
        );
        let from = scroll.saturating_sub(item_top);
        let to = height.min(end.saturating_sub(item_top));
        let content_from = from;
        let content_to = to.min(cached.lines.len());
        if active {
            let animated = render_agent_entry(
                &cached.entry,
                cached.width,
                app.animation_tick,
                true,
                app.theme,
            )
            .0;
            visible.extend(
                animated[content_from.min(content_to)..content_to]
                    .iter()
                    .cloned(),
            );
        } else {
            visible.extend(
                cached.lines[content_from.min(content_to)..content_to]
                    .iter()
                    .cloned(),
            );
        }
        links.extend(cached.links.iter().filter_map(|link| {
            let global_row = item_top + link.row;
            (global_row >= scroll && global_row < end).then(|| {
                let mut link = link.clone();
                link.row = global_row;
                link
            })
        }));
        images.extend(cached.images.iter().filter_map(|image| {
            let mut image = image.clone();
            image.row += item_top;
            let image_end = image.row.saturating_add(image.height);
            (image_end > scroll && image.row < end).then_some(image)
        }));
    }
    (visible, links, images)
}

fn agent_stats_line(app: &App, width: u16) -> Option<Line<'static>> {
    if app.agent_usage.is_empty() && app.agent_retry_count == 0 {
        return None;
    }
    if app.agent_usage.is_empty() {
        return Some(Line::from(Span::styled(
            format!("Retry {}", app.agent_retry_count),
            Style::default().fg(app.theme.text_subtle),
        )));
    }
    let input = human_token_count(app.agent_usage.total_input());
    let output = human_token_count(app.agent_usage.output_tokens);
    let cache_read = human_token_count(app.agent_usage.cache_read_input_tokens);
    let cache_rate = if app.agent_usage.total_input() == 0 {
        0.0
    } else {
        app.agent_usage.cache_read_input_tokens as f64 * 100.0
            / app.agent_usage.total_input() as f64
    };
    let tps = if app.agent_response_duration.is_zero() {
        "--".to_string()
    } else {
        format!(
            "{:.1}",
            app.agent_timed_output_tokens as f64 / app.agent_response_duration.as_secs_f64()
        )
    };
    let tokens = format!("↑{input} ↓{output}");
    let full = format!("{tokens} · {tps} t/s · Cache {cache_read} {cache_rate:.0}%");
    let compact = format!("{tokens} · {tps}t/s · C{cache_read} {cache_rate:.0}%");
    let candidates = if app.agent_retry_count > 0 {
        vec![
            format!("{full} · Retry {}", app.agent_retry_count),
            format!("{compact} · R{}", app.agent_retry_count),
            format!("{tokens} · R{}", app.agent_retry_count),
            tokens,
        ]
    } else {
        vec![full, compact, tokens]
    };
    let text = candidates
        .into_iter()
        .find(|candidate| candidate.width() <= usize::from(width))
        .unwrap_or_default();
    Some(Line::from(Span::styled(
        text,
        Style::default().fg(app.theme.text_subtle),
    )))
}

fn human_token_count(tokens: u64) -> String {
    let (value, suffix) = if tokens >= 1_000_000 {
        (tokens as f64 / 1_000_000.0, "m")
    } else if tokens >= 1_000 {
        (tokens as f64 / 1_000.0, "k")
    } else {
        return tokens.to_string();
    };
    let formatted = format!("{value:.1}");
    format!("{}{suffix}", formatted.trim_end_matches(".0"))
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
    } else if app.focus == Focus::Todo {
        app.layout.todo = non_empty(body);
        draw_todo(frame, app, body, interactive);
    } else if app.focus == Focus::Agent {
        app.layout.agent = non_empty(body);
        draw_agent_output(frame, app, body);
    } else {
        app.layout.center = non_empty(body);
        draw_center(frame, app, body, interactive, cursor_position);
    }
}

fn center_content_axis(area: Rect) -> Rect {
    let width = area.width.min(CENTER_MAX_WIDTH);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y,
        width,
        area.height,
    )
}

fn non_empty(area: Rect) -> Option<Rect> {
    (area.width > 0 && area.height > 0).then_some(area)
}

fn inset_horizontal(area: Rect, padding: u16) -> Rect {
    let left = padding.min(area.width);
    let right = padding.min(area.width.saturating_sub(left));
    Rect::new(
        area.x.saturating_add(left),
        area.y,
        area.width.saturating_sub(left).saturating_sub(right),
        area.height,
    )
}

fn shared_selection_area(container: Rect, item_y: u16, item_height: u16) -> Rect {
    let selection_y = item_y.saturating_sub(1).max(container.y);
    let selection_end = item_y
        .saturating_add(item_height)
        .min(container.y.saturating_add(container.height));
    Rect::new(
        container.x,
        selection_y,
        container.width,
        selection_end.saturating_sub(selection_y),
    )
}

fn draw_selection_indicator(frame: &mut Frame, area: Rect, theme: Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for y in area.y..area.y.saturating_add(area.height) {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "▌",
                Style::default()
                    .fg(theme.selection_indicator)
                    .remove_modifier(Modifier::BOLD | Modifier::DIM),
            )),
            Rect::new(area.x, y, 1, 1),
        );
    }
}

fn selection_list_height(item_count: u16, item_height: u16) -> u16 {
    if item_count == 0 {
        0
    } else {
        1_u16.saturating_add(item_count.saturating_mul(item_height))
    }
}

fn visible_selection_items(list_height: u16, item_height: u16) -> usize {
    (list_height.saturating_sub(1) as usize).div_ceil(item_height as usize)
}

fn selection_item_y(container: Rect, row: usize, item_height: u16) -> u16 {
    container.y.saturating_add(1).saturating_add(
        u16::try_from(row)
            .unwrap_or(u16::MAX)
            .saturating_mul(item_height),
    )
}

fn draw_files(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let focused = app.focus == Focus::Files;
    let title = match app.files_context {
        FilesContext::Browse => " NólëBase ",
        FilesContext::Search => " NólëBase · search ",
        FilesContext::MoveTarget => " NólëBase · move to ",
        FilesContext::NewTarget => " NólëBase · new ",
        FilesContext::Rename => " NólëBase · rename ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(title)
        .style(Style::default().bg(app.theme.surface_panel))
        .border_style(focus_border(focused, app.theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (input_area, list_area) = match app.files_context {
        FilesContext::Search if inner.height > 0 => (
            Some(Rect::new(inner.x, inner.y, inner.width, 1)),
            Rect::new(
                inner.x,
                inner.y.saturating_add(2),
                inner.width,
                inner.height.saturating_sub(2),
            ),
        ),
        _ => (None, inner),
    };

    if let Some(input_area) = input_area {
        let (prompt, value) = match app.files_context {
            FilesContext::Search => ("/ ", app.file_query.as_str()),
            _ => ("", ""),
        };
        if let Some(position) = draw_single_line_input(
            frame,
            input_area,
            prompt,
            value,
            value.chars().count(),
            focused && interactive,
            app.theme,
        ) {
            *cursor_position = Some(position);
        }
        if inner.height > 1 {
            frame.render_widget(
                Paragraph::new("─".repeat(usize::from(inner.width)))
                    .style(Style::default().fg(app.theme.text_muted)),
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
            );
        }
    }

    if list_area.width == 0 || list_area.height == 0 {
        return;
    }

    let rows = app.visible_file_rows();
    if rows.is_empty() {
        let message = if app.files_context == FilesContext::Search && !app.file_query.is_empty() {
            "No matching notes"
        } else {
            "No notes yet"
        };
        frame.render_widget(
            Paragraph::new(message).alignment(Alignment::Center),
            list_area,
        );
        return;
    }

    let notes_count = app.note_files.iter().filter(|file| !file.archived).count();
    let archives_count = app.note_files.iter().filter(|file| file.archived).count();
    let searching = app.files_context == FilesContext::Search && !app.file_query.is_empty();
    let row_height = |row: &FileListRow| match row {
        FileListRow::Group(group) => {
            let has_visible_children = match group {
                FileGroup::Notes => (app.notes_expanded || searching) && notes_count > 0,
                FileGroup::Archives => (app.archives_expanded || searching) && archives_count > 0,
            };
            if has_visible_children {
                2
            } else {
                1
            }
        }
        FileListRow::File(_) => 3u16,
    };
    let selected_row = app.file_row.min(rows.len().saturating_sub(1));
    let mut start = selected_row;
    let mut used = row_height(&rows[selected_row]);
    while start > 0 {
        let previous = row_height(&rows[start - 1]);
        if used.saturating_add(previous) > list_area.height {
            break;
        }
        start -= 1;
        used = used.saturating_add(previous);
    }

    let mut y = list_area.y;
    for (row_index, row) in rows.iter().copied().enumerate().skip(start) {
        if y >= list_area.y.saturating_add(list_area.height) {
            break;
        }
        let layout_height = row_height(&row).min(list_area.y + list_area.height - y);
        let selected = row_index == selected_row;
        let row_style = if selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .bg(if focused {
                    app.theme.selection_background
                } else {
                    app.theme.selection_background_inactive
                })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let selection_area = if selected && matches!(row, FileListRow::File(_)) {
            Some(shared_selection_area(list_area, y, layout_height))
        } else {
            None
        };
        if let Some(selection_area) = selection_area {
            frame.render_widget(Block::default().style(row_style), selection_area);
        }
        match row {
            FileListRow::Group(group) => {
                let (label, expanded, count) = match group {
                    FileGroup::Notes => ("Notes", app.notes_expanded || searching, notes_count),
                    FileGroup::Archives => (
                        "Archives",
                        app.archives_expanded || searching,
                        archives_count,
                    ),
                };
                let marker = if expanded { "▼" } else { "▶" };
                let group_area = Rect::new(list_area.x, y, list_area.width, 1);
                frame.render_widget(Paragraph::new("").style(row_style), group_area);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            marker,
                            Style::default().fg(if selected {
                                app.theme.selection_foreground
                            } else {
                                app.theme.ui_group_marker
                            }),
                        ),
                        Span::raw(format!(" {label}")),
                    ]))
                    .style(row_style),
                    group_area,
                );
                let count = count.to_string();
                let count_width = (count.width() as u16).min(group_area.width);
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        count,
                        if selected {
                            Style::default()
                                .fg(app.theme.selection_foreground)
                                .add_modifier(Modifier::DIM)
                        } else {
                            Style::default().fg(app.theme.text_muted)
                        },
                    ))
                    .alignment(Alignment::Right),
                    Rect::new(
                        group_area.x + group_area.width.saturating_sub(count_width),
                        group_area.y,
                        count_width,
                        1,
                    ),
                );
                if interactive {
                    app.file_group_hitboxes.push(FileGroupHitbox {
                        group,
                        area: Rect::new(list_area.x, y, list_area.width, 1),
                    });
                }
            }
            FileListRow::File(absolute_index) => {
                let Some(file) = app.note_files.get(absolute_index) else {
                    continue;
                };
                let name = file
                    .path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?");
                let base_style = if file.archived && !selected {
                    row_style
                        .fg(app.theme.text_muted)
                        .add_modifier(Modifier::DIM)
                } else {
                    row_style
                };
                frame.render_widget(
                    Paragraph::new(Line::from(format!("  {name}"))).style(base_style),
                    Rect::new(list_area.x, y, list_area.width, 1),
                );
                let content_height = 2.min(layout_height);
                if content_height > 1 {
                    let modified: DateTime<Local> = file.modified.into();
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            format!("  {}", modified.format("%y/%m/%d %H:%M")),
                            if selected {
                                Style::default()
                                    .fg(app.theme.selection_foreground)
                                    .add_modifier(Modifier::DIM)
                            } else {
                                Style::default().fg(app.theme.text_muted)
                            },
                        )))
                        .style(row_style),
                        Rect::new(list_area.x, y + 1, list_area.width, 1),
                    );
                }
                if interactive {
                    app.file_hitboxes.push(FileHitbox {
                        path: file.path.clone(),
                        area: Rect::new(list_area.x, y, list_area.width, content_height),
                    });
                }
                if let Some(selection_area) = selection_area {
                    draw_selection_indicator(frame, selection_area, app.theme);
                }
            }
        }
        y = y.saturating_add(layout_height);
    }
}

fn draw_todo(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let focused = app.focus == Focus::Todo;
    let done = app.todo_items.iter().filter(|item| item.checked).count();
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(format!(" Todo {done}/{} ", app.todo_items.len()))
        .style(Style::default().bg(app.theme.surface_panel))
        .border_style(focus_border(focused, app.theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if app.todo_items.is_empty() {
        frame.render_widget(
            Paragraph::new("No todos yet").alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let visible_indices = app.visible_todo_indices();
    let selected = app.todo_index.min(app.todo_items.len().saturating_sub(1));
    let selected_position = visible_indices
        .iter()
        .position(|index| *index == selected)
        .unwrap_or(0);
    let text_width = inner.width.saturating_sub(4).max(1) as usize;
    let item_heights: Vec<usize> = visible_indices
        .iter()
        .filter_map(|index| app.todo_items.get(*index))
        .map(|item| {
            wrap_spans_to_width(&[Span::raw(item.text.replace('\n', " "))], text_width).len() + 1
        })
        .collect();
    let viewport_height = inner.height.saturating_sub(1) as usize;
    if viewport_height == 0 {
        return;
    }
    let mut start = selected_position;
    let mut used = item_heights[selected_position];
    while start > 0 && used + item_heights[start - 1] <= viewport_height {
        start -= 1;
        used += item_heights[start];
    }

    let mut y = inner.y.saturating_add(1);
    for index in visible_indices.iter().copied().skip(start) {
        if y >= inner.y.saturating_add(inner.height) {
            break;
        }
        let Some(item) = app.todo_items.get(index) else {
            continue;
        };
        let checked = if item.checked { "[x]" } else { "[ ]" };
        let item_selected = focused && index == selected;
        let marker_style = if item_selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .add_modifier(Modifier::BOLD)
        } else if item.checked {
            Style::default()
                .fg(app.theme.ui_task_done)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.ui_task_open)
        };
        let mut text_style = if item.checked {
            Style::default().add_modifier(Modifier::CROSSED_OUT)
        } else {
            Style::default()
        };
        if item_selected {
            text_style = text_style
                .fg(app.theme.selection_foreground)
                .bg(app.theme.selection_background);
        }
        let wrapped = wrap_spans_to_width(
            &[Span::styled(item.text.replace('\n', " "), text_style)],
            text_width,
        );
        let content_height = wrapped.len() as u16;
        let layout_height = content_height
            .saturating_add(1)
            .min(inner.y.saturating_add(inner.height).saturating_sub(y));
        let visible_height = content_height.min(layout_height);
        if item_selected {
            frame.render_widget(
                Block::default().style(
                    Style::default()
                        .fg(app.theme.selection_foreground)
                        .bg(app.theme.selection_background),
                ),
                shared_selection_area(inner, y, layout_height),
            );
        }
        for (row, mut spans) in wrapped
            .into_iter()
            .take(visible_height as usize)
            .enumerate()
        {
            let mut line = if row == 0 {
                vec![Span::styled(format!("{checked} "), marker_style)]
            } else {
                vec![Span::raw("    ")]
            };
            line.append(&mut spans);
            frame.render_widget(
                Paragraph::new(Line::from(line)),
                Rect::new(inner.x, y + row as u16, inner.width, 1),
            );
        }
        let item_area = Rect::new(inner.x, y, inner.width, layout_height);
        if interactive {
            app.todo_hitboxes.push(TodoHitbox {
                index,
                area: item_area,
            });
        }
        y = y.saturating_add(layout_height);
    }
}

fn focus_border(focused: bool, theme: Theme) -> Style {
    Style::default().fg(if focused {
        theme.ui_focus_border
    } else {
        theme.ui_border
    })
}

fn animated_color(position: usize, tick: u64, theme: Theme) -> Color {
    let stops = theme.animation_gradient.map(rgb_components);
    const STEPS: usize = 24;
    let phase = (position + tick as usize * 3) % (stops.len() * STEPS);
    let stop = phase / STEPS;
    let amount = phase % STEPS;
    let from = stops[stop];
    let to = stops[(stop + 1) % stops.len()];
    let blend = |a: u8, b: u8| {
        let a = usize::from(a);
        let b = usize::from(b);
        ((a * (STEPS - amount) + b * amount) / STEPS) as u8
    };
    Color::Rgb(
        blend(from.0, to.0),
        blend(from.1, to.1),
        blend(from.2, to.2),
    )
}

fn rgb_components(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(red, green, blue) => (red, green, blue),
        _ => unreachable!("theme colors are parsed as RGB"),
    }
}

fn animated_activity_lines(
    text: &str,
    width: usize,
    tick: u64,
    theme: Theme,
) -> Vec<Line<'static>> {
    let (status, detail) = activity_parts(text);
    let marker = activity_marker(width, theme);
    let available = width.saturating_sub(marker.width());
    let characters = compact_activity_line(status, available)
        .chars()
        .filter(|character| UnicodeWidthChar::width(*character).unwrap_or(0) > 0)
        .collect::<Vec<_>>();
    let mut spans = vec![marker];
    spans.extend(
        characters
            .into_iter()
            .enumerate()
            .map(|(index, character)| {
                Span::styled(
                    character.to_string(),
                    Style::default()
                        .fg(animated_color(index * 8, tick, theme))
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>(),
    );
    let mut lines = vec![Line::from(spans)];
    if let Some(detail) = detail {
        lines.push(activity_detail_line(detail, width, theme));
    }
    lines
}

fn activity_lines(text: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (status, detail) = activity_parts(text);
    let marker = activity_marker(width, theme);
    let available = width.saturating_sub(marker.width());
    let spans = vec![
        marker,
        Span::styled(
            compact_activity_line(status, available),
            Style::default().fg(theme.text_muted),
        ),
    ];
    let mut lines = vec![Line::from(spans)];
    if let Some(detail) = detail {
        lines.push(activity_detail_line(detail, width, theme));
    }
    lines
}

fn activity_parts(text: &str) -> (&str, Option<&str>) {
    let (status, detail) = text.split_once('\n').unwrap_or((text, ""));
    (status, (!detail.is_empty()).then_some(detail))
}

fn activity_detail_line(detail: &str, width: usize, theme: Theme) -> Line<'static> {
    let marker = activity_detail_marker(width, theme);
    let available = width.saturating_sub(marker.width());
    Line::from(vec![
        marker,
        Span::styled(
            compact_activity_line(detail, available),
            Style::default().fg(theme.text_muted),
        ),
    ])
}

fn compact_activity_line(text: &str, width: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if UnicodeWidthStr::width(text.as_str()) <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let content_width = width - 1;
    let mut result = String::new();
    let mut used = 0usize;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used.saturating_add(character_width) > content_width {
            break;
        }
        result.push(character);
        used = used.saturating_add(character_width);
    }
    result.push('…');
    result
}

fn activity_marker(width: usize, theme: Theme) -> Span<'static> {
    let marker = match width {
        0 => "",
        1 => "•",
        2 => "• ",
        _ => " • ",
    };
    Span::styled(
        marker,
        Style::default()
            .fg(theme.ui_activity_marker)
            .add_modifier(Modifier::BOLD),
    )
}

fn activity_detail_marker(width: usize, theme: Theme) -> Span<'static> {
    let marker = match width {
        0 => "",
        1 => "└",
        2 => "└─",
        3 => "└─ ",
        4 => " └─",
        _ => "   └─ ",
    };
    Span::styled(marker, Style::default().fg(theme.ui_border_subtle))
}

fn draw_animated_border(frame: &mut Frame, area: Rect, tick: u64, theme: Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mut position = 0usize;
    for x in area.x..area.x.saturating_add(area.width) {
        frame.buffer_mut()[(x, area.y)].set_fg(animated_color(position, tick, theme));
        position += 1;
    }
    for y in area.y.saturating_add(1)..area.y.saturating_add(area.height) {
        let x = area.x.saturating_add(area.width.saturating_sub(1));
        frame.buffer_mut()[(x, y)].set_fg(animated_color(position, tick, theme));
        position += 1;
    }
    if area.height > 1 {
        let y = area.y.saturating_add(area.height - 1);
        for x in (area.x..area.x.saturating_add(area.width.saturating_sub(1))).rev() {
            frame.buffer_mut()[(x, y)].set_fg(animated_color(position, tick, theme));
            position += 1;
        }
    }
    if area.width > 1 {
        for y in
            (area.y.saturating_add(1)..area.y.saturating_add(area.height.saturating_sub(1))).rev()
        {
            frame.buffer_mut()[(area.x, y)].set_fg(animated_color(position, tick, theme));
            position += 1;
        }
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
        CenterView::Document => draw_document(frame, app, content, interactive, cursor_position),
        CenterView::Search | CenterView::DocumentSearch => {
            draw_search(frame, app, content, interactive, cursor_position)
        }
    }
}

fn draw_daily(
    frame: &mut Frame,
    app: &mut App,
    surface: Rect,
    content: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if surface.width == 0 || surface.height == 0 {
        return;
    }
    let content = inset_horizontal(content, 2);
    if content.width == 0 || content.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Daily",
            Style::default()
                .fg(app.theme.ui_page_heading)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(content.x, content.y, content.width, 1),
    );

    let compose = compose_rect(content);
    app.layout.compose = non_empty(compose);

    let daily_top = content.y.saturating_add(2).min(content.y + content.height);
    let daily_view = Rect::new(
        content.x,
        daily_top,
        content.width,
        content
            .y
            .saturating_add(content.height)
            .saturating_sub(daily_top),
    );
    let unoccluded_height = compose
        .y
        .saturating_sub(1)
        .saturating_sub(daily_view.y)
        .min(daily_view.height);
    draw_daily_notes(frame, app, daily_view, unoccluded_height, interactive);

    if compose.width > 0 && compose.height > 0 {
        clear_widget(frame, compose);
        draw_compose(frame, app, compose, interactive, cursor_position);
    }
}

fn compose_rect(area: Rect) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let width = if area.width > 4 {
        area.width.saturating_sub(4).min(CENTER_MAX_WIDTH)
    } else {
        area.width
    };
    let desired_height = if area.height >= 14 { 7 } else { 5 };
    let height = desired_height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let bottom_margin = u16::from(area.height > height);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height + bottom_margin));
    Rect::new(x, y, width, height)
}

fn draw_daily_notes(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    unoccluded_height: u16,
    interactive: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width as usize;
    sync_daily_vlist(app, width);
    if app.daily_notes.is_empty() {
        frame.render_widget(
            Paragraph::new("No notes yet").alignment(Alignment::Center),
            area,
        );
        return;
    }

    let render_height = area.height as usize;
    let view_height = unoccluded_height.min(area.height) as usize;
    let tail_pinned = app.scroll == u16::MAX;
    let mut scroll = (app.scroll as usize)
        .min(app.daily_vlist.geometry.max_scroll(view_height))
        .min(u16::MAX as usize);
    if app.reveal_selected_daily {
        if app.selected < app.daily_notes.len() {
            ensure_daily_card_rendered(app, app.selected);
            let first = app.daily_vlist.geometry.item_top(app.selected);
            let button = first
                + app.daily_vlist.items[app.selected]
                    .cache
                    .as_ref()
                    .expect("selected DailyNote was rendered")
                    .button_line;
            scroll = stable_card_scroll(scroll, first, button, view_height);
        }
        app.reveal_selected_daily = false;
    }
    scroll = measure_visible_daily_cards(app, scroll, render_height, view_height, tail_pinned);
    scroll = scroll
        .min(app.daily_vlist.geometry.max_scroll(view_height))
        .min(u16::MAX as usize);
    app.scroll = scroll as u16;

    let end = scroll.saturating_add(render_height);
    let mut visible = Vec::with_capacity(render_height);
    let mut rendered_links = Vec::new();
    let mut rendered_tags = Vec::new();
    let mut rendered_images = Vec::new();
    let visible_range = app
        .daily_vlist
        .geometry
        .visible_range(scroll, render_height);
    for index in visible_range.clone() {
        let first = app.daily_vlist.geometry.item_top(index);
        let cached = app.daily_vlist.items[index]
            .cache
            .as_ref()
            .expect("visible DailyNotes are measured before rendering");
        let from = scroll.saturating_sub(first);
        let to = cached.lines.len().min(end.saturating_sub(first));
        for (local_row, line) in cached.lines[from.min(to)..to].iter().enumerate() {
            let actual_row = from + local_row;
            if actual_row == cached.button_line {
                visible.push(daily_button_line(
                    width,
                    cached.button_start,
                    index == app.selected,
                    app.theme,
                ));
            } else {
                visible.push(line.clone());
            }
        }
        rendered_links.extend(cached.links.iter().filter_map(|link| {
            let global_row = first + link.row;
            (global_row >= scroll && global_row < end).then(|| {
                let mut link = link.clone();
                link.row = global_row;
                link
            })
        }));
        rendered_tags.extend(cached.tags.iter().filter_map(|tag| {
            let global_row = first + tag.row;
            (global_row >= scroll && global_row < end).then(|| {
                let mut tag = tag.clone();
                tag.row = global_row;
                tag
            })
        }));
        rendered_images.extend(cached.images.iter().filter_map(|image| {
            let mut image = image.clone();
            image.row += first;
            let image_end = image.row.saturating_add(image.height);
            (image_end > scroll && image.row < end).then_some(image)
        }));
    }

    if interactive {
        let interactive_area = Rect::new(area.x, area.y, area.width, unoccluded_height);
        register_link_hitboxes(
            &mut app.link_hitboxes,
            &rendered_links,
            interactive_area,
            scroll,
            &app.storage.daily_dir,
        );
        register_tag_hitboxes(
            &mut app.tag_hitboxes,
            &rendered_tags,
            interactive_area,
            scroll,
        );
        for index in visible_range {
            let first = app.daily_vlist.geometry.item_top(index);
            let cached = app.daily_vlist.items[index]
                .cache
                .as_ref()
                .expect("visible DailyNotes are rendered");
            let button_line = first + cached.button_line;
            if button_line < scroll || button_line >= scroll.saturating_add(view_height) {
                continue;
            }
            let y = area.y + (button_line - scroll) as u16;
            register_buttons_clipped(
                &mut app.hitboxes,
                app.daily_notes[index].date,
                area.x.saturating_add(cached.button_start as u16),
                y,
                area,
            );
        }
    }

    frame.render_widget(Paragraph::new(visible), area);
    let image_base = app.storage.daily_dir.clone();
    app.images.render(
        frame,
        &rendered_images,
        area,
        scroll,
        &image_base,
        app.theme,
    );
    if let Some(cached) = app
        .daily_vlist
        .items
        .get(app.selected)
        .and_then(|item| item.cache.as_ref())
    {
        let first = app.daily_vlist.geometry.item_top(app.selected);
        let last = first + cached.lines.len().saturating_sub(2);
        draw_selected_card_border(
            frame,
            area,
            scroll,
            first,
            last,
            app.animation_tick,
            app.theme,
        );
    }
}

fn sync_daily_vlist(app: &mut App, width: usize) {
    let width_changed = app.daily_vlist.width != width;
    let same_items = !width_changed
        && app.daily_vlist.items.len() == app.daily_notes.len()
        && app
            .daily_vlist
            .items
            .iter()
            .zip(&app.daily_notes)
            .all(|(item, note)| item.date == note.date);
    if !same_items {
        let old_items = if width_changed {
            Vec::new()
        } else {
            std::mem::take(&mut app.daily_vlist.items)
        };
        let mut by_date = old_items
            .into_iter()
            .map(|item| (item.date, item))
            .collect::<HashMap<_, _>>();
        app.daily_vlist.items = app
            .daily_notes
            .iter()
            .map(|note| {
                by_date
                    .remove(&note.date)
                    .unwrap_or(crate::app::DailyVirtualItem {
                        date: note.date,
                        cache: None,
                    })
            })
            .collect();
        app.daily_vlist.geometry = crate::vlist::VList::new(12);
        app.daily_vlist.geometry.resize(app.daily_notes.len());
        for (index, item) in app.daily_vlist.items.iter().enumerate() {
            if let Some(cache) = &item.cache {
                app.daily_vlist
                    .geometry
                    .set_height(index, cache.lines.len());
            }
        }
        app.daily_vlist.width = width;
    } else {
        app.daily_vlist.geometry.resize(app.daily_notes.len());
    }

    for (index, (item, note)) in app
        .daily_vlist
        .items
        .iter_mut()
        .zip(&app.daily_notes)
        .enumerate()
    {
        let date_label = note.date.format(DATE_FMT).to_string();
        if item.cache.as_ref().is_some_and(|cached| {
            cached.width != width || cached.date_label != date_label || cached.body != note.body
        }) {
            item.cache = None;
            app.daily_vlist.geometry.invalidate(index);
        }
    }
}

fn ensure_daily_card_rendered(app: &mut App, index: usize) {
    if app.daily_vlist.items[index].cache.is_some() {
        return;
    }
    let date_label = app.daily_notes[index].date.format(DATE_FMT).to_string();
    let cached = render_daily_note(
        &app.daily_notes[index],
        date_label,
        app.daily_vlist.width,
        app.theme,
    );
    let height = cached.lines.len();
    app.daily_vlist.items[index].cache = Some(cached);
    app.daily_vlist.geometry.set_height(index, height);
}

fn measure_visible_daily_cards(
    app: &mut App,
    mut scroll: usize,
    render_height: usize,
    scroll_view_height: usize,
    tail_pinned: bool,
) -> usize {
    loop {
        let range = app
            .daily_vlist
            .geometry
            .visible_range(scroll, render_height);
        if range.is_empty() {
            return 0;
        }
        let anchor = range.start;
        let anchor_offset = scroll.saturating_sub(app.daily_vlist.geometry.item_top(anchor));
        let missing = range
            .clone()
            .filter(|index| !app.daily_vlist.geometry.is_measured(*index))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            let maximum = app.daily_vlist.geometry.max_scroll(scroll_view_height);
            return if tail_pinned {
                maximum
            } else {
                scroll.min(maximum)
            };
        }
        for index in missing {
            ensure_daily_card_rendered(app, index);
        }
        scroll = if tail_pinned {
            app.daily_vlist.geometry.max_scroll(scroll_view_height)
        } else {
            let height = app.daily_vlist.geometry.height(anchor);
            app.daily_vlist.geometry.item_top(anchor) + anchor_offset.min(height.saturating_sub(1))
        };
        scroll = scroll.min(app.daily_vlist.geometry.max_scroll(scroll_view_height));
    }
}

fn render_daily_note(
    note: &crate::model::DailyNote,
    date_label: String,
    width: usize,
    theme: Theme,
) -> crate::app::DailyCardRenderCache {
    let card_style = Style::default().bg(theme.surface_panel);
    let horizontal_padding = DAILY_PADDING_X.min(width.saturating_sub(1) / 2);
    let body_start = horizontal_padding + UnicodeWidthStr::width(date_label.as_str()) + 2;
    let (body_start, body_width) = centered_daily_body_axis(width, body_start);
    let mut lines = vec![
        line_with_background(Vec::new(), width, card_style),
        line_with_background(Vec::new(), width, card_style),
        line_with_background(
            vec![
                Span::raw(" ".repeat(body_start)),
                Span::styled(
                    date_label.clone(),
                    Style::default()
                        .fg(theme.text_subtle)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ],
            width,
            card_style,
        ),
        line_with_background(Vec::new(), width, card_style),
    ];
    let markdown = crate::markdown::render_at_width(&note.body, body_width, theme);
    let body_line_start = lines.len();
    let links = markdown
        .links
        .into_iter()
        .map(|mut link| {
            link.row += body_line_start;
            link.column += body_start;
            link
        })
        .collect();
    let tags = markdown
        .tags
        .into_iter()
        .map(|mut tag| {
            tag.row += body_line_start;
            tag.column += body_start;
            tag
        })
        .collect();
    let images = markdown
        .images
        .into_iter()
        .map(|mut image| {
            image.row += body_line_start;
            image.column += body_start;
            image
        })
        .collect();
    for markdown_line in markdown.lines {
        for body in wrap_spans_to_width(&markdown_line.spans, body_width) {
            let mut spans = Vec::with_capacity(body.len() + 1);
            spans.push(Span::raw(" ".repeat(body_start)));
            spans.extend(body);
            lines.push(line_with_background(spans, width, card_style));
        }
    }
    lines.push(line_with_background(Vec::new(), width, card_style));
    let button_start = width
        .saturating_sub(body_start)
        .saturating_sub(action_buttons_width());
    let button_line = lines.len();
    lines.push(daily_button_line(width, button_start, false, theme));
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(Line::default());
    crate::app::DailyCardRenderCache {
        width,
        date: note.date,
        date_label,
        body: note.body.clone(),
        lines,
        links,
        tags,
        images,
        button_line,
        button_start,
    }
}

fn daily_button_line(
    width: usize,
    button_start: usize,
    selected: bool,
    theme: Theme,
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(button_start))];
    spans.extend(render_button_line(selected, theme).spans);
    line_with_background(spans, width, Style::default().bg(theme.surface_panel))
}

fn draw_selected_card_border(
    frame: &mut Frame,
    area: Rect,
    scroll: usize,
    first: usize,
    last: usize,
    tick: u64,
    theme: Theme,
) {
    if area.width < 2 || first > last {
        return;
    }
    let visible_end = scroll.saturating_add(area.height as usize);
    let start = first.max(scroll);
    let end = last.min(visible_end.saturating_sub(1));
    if start > end {
        return;
    }
    let left = area.x;
    let right = area.x + area.width - 1;
    let width = area.width as usize;
    let height = last.saturating_sub(first).saturating_add(1);
    for row in start..=end {
        let y = area.y + (row - scroll) as u16;
        if row == first {
            for x in left..=right {
                let symbol = if x == left {
                    "┌"
                } else if x == right {
                    "┐"
                } else {
                    "─"
                };
                let position = (x - left) as usize;
                frame.buffer_mut()[(x, y)]
                    .set_symbol(symbol)
                    .set_style(animated_card_border_style(position, tick, theme));
            }
        } else if row == last {
            for x in left..=right {
                let symbol = if x == left {
                    "└"
                } else if x == right {
                    "┘"
                } else {
                    "─"
                };
                let position = width
                    .saturating_add(height.saturating_sub(1))
                    .saturating_add((right - x) as usize);
                frame.buffer_mut()[(x, y)]
                    .set_symbol(symbol)
                    .set_style(animated_card_border_style(position, tick, theme));
            }
        } else {
            let right_position = width.saturating_add(row.saturating_sub(first + 1));
            let left_position = width
                .saturating_add(height.saturating_sub(1))
                .saturating_add(width.saturating_sub(1))
                .saturating_add(last.saturating_sub(row + 1));
            frame.buffer_mut()[(left, y)]
                .set_symbol("│")
                .set_style(animated_card_border_style(left_position, tick, theme));
            frame.buffer_mut()[(right, y)]
                .set_symbol("│")
                .set_style(animated_card_border_style(right_position, tick, theme));
        }
    }
}

fn animated_card_border_style(position: usize, tick: u64, theme: Theme) -> Style {
    Style::default()
        .fg(animated_color(position, tick, theme))
        .bg(theme.surface_panel)
}

fn stable_card_scroll(scroll: usize, first: usize, button: usize, view_height: usize) -> usize {
    let card_height = button.saturating_sub(first).saturating_add(1);
    if card_height <= view_height {
        if first < scroll {
            first
        } else if button >= scroll.saturating_add(view_height) {
            button.saturating_sub(view_height.saturating_sub(1))
        } else {
            scroll
        }
    } else {
        let last_start = button.saturating_sub(view_height.saturating_sub(1));
        scroll.clamp(first, last_start)
    }
}

fn centered_daily_body_axis(width: usize, desired_start: usize) -> (usize, usize) {
    let start = desired_start.min(width.saturating_sub(1));
    let trailing = start.min(width.saturating_sub(start).saturating_sub(1));
    (start, width.saturating_sub(start + trailing).max(1))
}

fn action_buttons_width() -> usize {
    Action::all()
        .iter()
        .map(|action| action.label().width() + 2)
        .sum::<usize>()
        + Action::all().len().saturating_sub(1)
}

fn register_link_hitboxes(
    hitboxes: &mut Vec<LinkHitbox>,
    links: &[crate::markdown::RenderedLink],
    viewport: Rect,
    scroll: usize,
    base_dir: &Path,
) {
    let bottom = scroll.saturating_add(viewport.height as usize);
    for link in links
        .iter()
        .filter(|link| link.row >= scroll && link.row < bottom)
    {
        let column = link.column.min(viewport.width as usize);
        let width = link
            .width
            .min((viewport.width as usize).saturating_sub(column));
        if width == 0 {
            continue;
        }
        let target = match &link.target {
            LinkTarget::EmbeddedFile(path) if !path.is_absolute() => {
                LinkTarget::EmbeddedFile(base_dir.join(path))
            }
            target => target.clone(),
        };
        hitboxes.push(LinkHitbox {
            target,
            area: Rect::new(
                viewport.x.saturating_add(column as u16),
                viewport.y.saturating_add((link.row - scroll) as u16),
                width as u16,
                1,
            ),
        });
    }
}

fn register_tag_hitboxes(
    hitboxes: &mut Vec<TagHitbox>,
    tags: &[crate::markdown::RenderedTag],
    viewport: Rect,
    scroll: usize,
) {
    let bottom = scroll.saturating_add(viewport.height as usize);
    for tag in tags
        .iter()
        .filter(|tag| tag.row >= scroll && tag.row < bottom)
    {
        let column = tag.column.min(viewport.width as usize);
        let width = tag
            .width
            .min((viewport.width as usize).saturating_sub(column));
        if width == 0 {
            continue;
        }
        hitboxes.push(TagHitbox {
            name: tag.name.clone(),
            area: Rect::new(
                viewport.x.saturating_add(column as u16),
                viewport.y.saturating_add((tag.row - scroll) as u16),
                width as u16,
                1,
            ),
        });
    }
}

fn render_button_line(selected: bool, theme: Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, action) in Action::all().iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if *action == Action::Ai {
            Style::default()
                .fg(theme.text_on_accent)
                .bg(theme.ui_action_ai)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default()
                .fg(theme.text_on_accent)
                .bg(theme.ui_action)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.ui_action)
        };
        spans.push(Span::styled(format!("[{}]", action.label()), style));
    }
    Line::from(spans)
}

fn line_with_background(
    mut spans: Vec<Span<'static>>,
    width: usize,
    style: Style,
) -> Line<'static> {
    for span in &mut spans {
        span.style = style.patch(span.style);
    }
    let used: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    Line::from(spans)
}

fn register_buttons_clipped(
    hitboxes: &mut Vec<ButtonHitbox>,
    date: NaiveDate,
    start_x: u16,
    y: u16,
    viewport: Rect,
) {
    if y < viewport.y || y >= viewport.y.saturating_add(viewport.height) {
        return;
    }
    let right = viewport.x.saturating_add(viewport.width);
    let mut x = start_x;
    for (index, action) in Action::all().iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(1);
        }
        let width = action.label().width() as u16 + 2;
        let clipped_x = x.max(viewport.x);
        let clipped_right = x.saturating_add(width).min(right);
        if clipped_x < clipped_right {
            hitboxes.push(ButtonHitbox {
                date,
                action: *action,
                area: Rect::new(clipped_x, y, clipped_right - clipped_x, 1),
            });
        }
        x = x.saturating_add(width);
        if x >= right {
            break;
        }
    }
}

fn draw_compose(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    let focused = app.focus == Focus::Compose;
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(if focused {
            " Compose "
        } else {
            " Compose · i "
        })
        .style(Style::default().bg(app.theme.surface_compose))
        .border_style(focus_border(focused, app.theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if focused {
        draw_animated_border(frame, area, app.animation_tick, app.theme);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let (text_area, toolbar) = split_last_row(inner);
    if let Some(position) = draw_multiline_input(
        frame,
        text_area,
        &app.input,
        app.input_cursor,
        "Write something…",
        focused && interactive,
        app.theme,
    ) {
        *cursor_position = Some(position);
    }

    if toolbar.height > 0 {
        let lines = if app.input.is_empty() {
            0
        } else {
            app.input.lines().count().max(1)
        };
        let count = format!("{lines}l · {}c", app.input.chars().count());
        let hint = if focused && toolbar.width >= 72 {
            match app.center_view {
                CenterView::Document => {
                    "Enter append · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline"
                }
                _ => "Enter send · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline",
            }
        } else if focused && toolbar.width >= 42 {
            "Ctrl+Enter Agent · Ctrl+U recall"
        } else if focused && toolbar.width >= 25 {
            "Ctrl+Enter Agent"
        } else {
            ""
        };
        draw_left_right_line(frame, toolbar, &count, hint, app.theme.text_muted);
    }
}

fn draw_document(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if app.document.is_none() {
        frame.render_widget(
            Paragraph::new("No document").alignment(Alignment::Center),
            area,
        );
        return;
    }
    let content = inset_horizontal(area, 2);
    if content.width == 0 || content.height == 0 {
        return;
    }
    let compose = compose_rect(content);
    app.layout.compose = non_empty(compose);
    let header = Rect::new(content.x, content.y, content.width, 1);
    let page_area = Rect::new(
        content.x,
        content.y.saturating_add(2),
        content.width,
        content
            .y
            .saturating_add(content.height)
            .saturating_sub(content.y.saturating_add(2)),
    );
    let page_style = Style::default().bg(app.theme.surface_panel);
    frame.render_widget(Block::default().style(page_style), page_area);
    let horizontal_padding = (PAGE_PADDING_X as u16).min(page_area.width.saturating_sub(1) / 2);
    let vertical_padding = 2.min(page_area.height / 2);
    let document_area = Rect::new(
        page_area.x.saturating_add(horizontal_padding),
        page_area.y.saturating_add(vertical_padding),
        page_area
            .width
            .saturating_sub(horizontal_padding.saturating_mul(2)),
        page_area
            .height
            .saturating_sub(vertical_padding.saturating_mul(2)),
    );
    let unoccluded_document_height = compose
        .y
        .saturating_sub(1)
        .saturating_sub(document_area.y)
        .min(document_area.height);
    let image_base = match &app.document.as_ref().expect("document checked above").kind {
        crate::app::DocumentKind::File(path) => path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| app.storage.root.clone()),
        crate::app::DocumentKind::Daily(_) => app.storage.daily_dir.clone(),
    };
    let (rendered_links, rendered_tags, rendered_images, document_scroll) = {
        let document = app.document.as_mut().expect("document checked above");
        frame.render_widget(
            Paragraph::new(Span::styled(
                document.title.clone(),
                Style::default()
                    .fg(app.theme.ui_page_heading)
                    .add_modifier(Modifier::BOLD),
            )),
            header,
        );
        if let Some(target_line) = document.target_line.take() {
            document.scroll = crate::markdown::rendered_row_for_source_line(
                &document.source,
                target_line,
                document_area.width as usize,
                app.theme,
            )
            .min(u16::MAX as usize) as u16;
        }
        document.ensure_rendered(document_area.width as usize, app.theme);
        let rendered = &document
            .render_cache
            .as_ref()
            .expect("document render cache was initialized")
            .rendered;
        let rendered_links = rendered.links.clone();
        let rendered_tags = rendered.tags.clone();
        let rendered_images = rendered.images.clone();
        let lines = &rendered.lines;
        let max_scroll = lines
            .len()
            .saturating_sub(unoccluded_document_height as usize);
        document.scroll = (document.scroll as usize).min(max_scroll) as u16;
        let document_scroll = document.scroll as usize;
        let visible = visible_line_window(
            lines,
            document.scroll as usize,
            document_area.height as usize,
        );
        frame.render_widget(Paragraph::new(visible).style(page_style), document_area);
        (
            rendered_links,
            rendered_tags,
            rendered_images,
            document_scroll,
        )
    };
    app.images.render(
        frame,
        &rendered_images,
        document_area,
        document_scroll,
        &image_base,
        app.theme,
    );
    if interactive {
        let interactive_document_area = Rect::new(
            document_area.x,
            document_area.y,
            document_area.width,
            unoccluded_document_height,
        );
        register_link_hitboxes(
            &mut app.link_hitboxes,
            &rendered_links,
            interactive_document_area,
            document_scroll,
            &image_base,
        );
        register_tag_hitboxes(
            &mut app.tag_hitboxes,
            &rendered_tags,
            interactive_document_area,
            document_scroll,
        );
    }
    if compose.width > 0 && compose.height > 0 {
        clear_widget(frame, compose);
        draw_compose(frame, app, compose, interactive, cursor_position);
    }
}

fn draw_notification(frame: &mut Frame, root: Rect, message: &str, theme: Theme) {
    if root.width < 4 || root.height < 3 {
        return;
    }
    let width = root.width.saturating_sub(2).min(44);
    let text_width = width.saturating_sub(4).max(1) as usize;
    let rows = wrap_spans_to_width(&[Span::raw(message.to_string())], text_width)
        .len()
        .max(1);
    let height = (rows as u16).saturating_add(2).min(root.height.min(8));
    let area = Rect::new(
        root.x + root.width.saturating_sub(width).saturating_sub(1),
        root.y.saturating_add(1),
        width,
        height,
    );
    clear_widget(frame, area);
    frame.render_widget(
        Paragraph::new(message.to_string())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .padding(Padding::horizontal(1))
                    .title(" Notification ")
                    .style(Style::default().bg(theme.surface_panel))
                    .border_style(Style::default().fg(theme.ui_warning)),
            ),
        area,
    );
}

fn draw_search(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let content = inset_horizontal(area, 2);
    if content.width == 0 || content.height == 0 {
        return;
    }

    let input_width = if content.width > 4 {
        content.width.saturating_sub(4).min(72)
    } else {
        content.width
    };
    let input_height = 3.min(content.height);
    let input_box = Rect::new(
        content.x + content.width.saturating_sub(input_width) / 2,
        content.y,
        input_width,
        input_height,
    );
    let input_style = Style::default().bg(app.theme.surface_panel);
    if input_height >= 3 {
        clear_widget(frame, input_box);
        let title = match app.center_view {
            CenterView::DocumentSearch => "Search in Note",
            _ => "Searcher",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .title(format!(" {title} · {} ", app.search_results.len()))
            .style(input_style)
            .border_style(focus_border(app.focus == Focus::Center, app.theme));
        let input = block.inner(input_box);
        frame.render_widget(block, input_box);
        if let Some(position) = draw_single_line_input(
            frame,
            input,
            "/ ",
            &app.search_query,
            app.search_query.chars().count(),
            app.focus == Focus::Center && interactive,
            app.theme,
        ) {
            *cursor_position = Some(position);
        }
    } else {
        if let Some(position) = draw_single_line_input(
            frame,
            input_box,
            "/ ",
            &app.search_query,
            app.search_query.chars().count(),
            app.focus == Focus::Center && interactive,
            app.theme,
        ) {
            *cursor_position = Some(position);
        }
    }

    let results_y = input_box
        .y
        .saturating_add(input_box.height)
        .saturating_add(1);
    let results = Rect::new(
        content.x,
        results_y,
        content.width,
        content
            .y
            .saturating_add(content.height)
            .saturating_sub(results_y),
    );
    if results.height == 0 {
        return;
    }
    if app.search_results.is_empty() {
        if !app.search_query.is_empty() {
            frame.render_widget(
                Paragraph::new("No matches").alignment(Alignment::Center),
                results,
            );
        }
        return;
    }

    let visible = visible_selection_items(results.height, SELECT_OPTION_HEIGHT);
    let selected = app
        .search_index
        .min(app.search_results.len().saturating_sub(1));
    let start = selected
        .saturating_sub(visible.saturating_sub(1))
        .min(app.search_results.len().saturating_sub(visible));
    for (row, (index, hit)) in app
        .search_results
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let y = selection_item_y(results, row, SELECT_OPTION_HEIGHT);
        let item_height =
            SELECT_OPTION_HEIGHT.min(results.y.saturating_add(results.height).saturating_sub(y));
        let item_area = Rect::new(results.x, y, results.width, item_height);
        let is_selected = index == selected;
        let metadata_style = if is_selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(app.theme.text_muted)
        };
        let spans = match hit {
            SearchHit::FileLine {
                path,
                line_no,
                text,
                ..
            } => {
                let name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?");
                vec![
                    Span::styled(format!("{name}:{line_no} "), metadata_style),
                    Span::raw(text.clone()),
                ]
            }
            SearchHit::DocumentLine { line_no, text } => vec![
                Span::styled(format!("line {line_no} "), metadata_style),
                Span::raw(text.clone()),
            ],
        };
        let style = if is_selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .bg(app.theme.selection_background)
        } else {
            Style::default()
        };
        let selection_area = is_selected.then(|| shared_selection_area(results, y, item_height));
        if let Some(selection_area) = selection_area {
            frame.render_widget(Block::default().style(style), selection_area);
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(style),
            inset_horizontal(Rect::new(item_area.x, item_area.y, item_area.width, 1), 2),
        );
        if let Some(selection_area) = selection_area {
            draw_selection_indicator(frame, selection_area, app.theme);
        }
        if interactive {
            app.search_hitboxes.push(SearchHitbox {
                index,
                area: item_area,
            });
        }
    }
}

fn draw_single_line_input(
    frame: &mut Frame,
    area: Rect,
    prompt: &str,
    value: &str,
    cursor: usize,
    show_cursor: bool,
    theme: Theme,
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                prompt.to_string(),
                Style::default().fg(theme.ui_input_prompt),
            ),
            Span::raw(value.to_string()),
        ])),
        area,
    );
    show_cursor.then(|| {
        let cursor_byte = char_to_byte(value, cursor.min(value.chars().count()));
        let column = UnicodeWidthStr::width(prompt) + UnicodeWidthStr::width(&value[..cursor_byte]);
        let x = area.x + (column as u16).min(area.width.saturating_sub(1));
        Position::new(x, area.y)
    })
}

fn draw_multiline_input(
    frame: &mut Frame,
    area: Rect,
    value: &str,
    cursor: usize,
    placeholder: &str,
    show_cursor: bool,
    theme: Theme,
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let width = area.width as usize;
    let lines: Vec<Line> = if value.is_empty() {
        wrap_spans_to_width(
            &[Span::styled(
                placeholder.to_string(),
                Style::default().fg(theme.text_muted),
            )],
            width,
        )
        .into_iter()
        .map(Line::from)
        .collect()
    } else {
        value
            .split('\n')
            .flat_map(|line| {
                wrap_spans_to_width(&[Span::raw(line.to_string())], width)
                    .into_iter()
                    .map(Line::from)
            })
            .collect()
    };
    let logical_widths: Vec<usize> = value.split('\n').map(UnicodeWidthStr::width).collect();
    let total_rows = lines.len();
    let (cursor_line, cursor_column) = cursor_row_col(value, cursor);
    let cursor_line = cursor_line.min(logical_widths.len().saturating_sub(1));
    let rows_before: usize = logical_widths[..cursor_line]
        .iter()
        .map(|line_width| wrapped_row_count(*line_width, width))
        .sum();
    let wrapped_cursor_row = rows_before + cursor_column / width.max(1);
    let viewport_height = area.height as usize;
    let scroll = if total_rows <= viewport_height {
        0
    } else {
        wrapped_cursor_row
            .saturating_sub(viewport_height.saturating_sub(1))
            .min(total_rows.saturating_sub(viewport_height))
    };
    let visible = visible_line_window(&lines, scroll, viewport_height);
    frame.render_widget(Paragraph::new(visible), area);
    show_cursor.then(|| {
        let x = area.x + (cursor_column % width.max(1)) as u16;
        let visible_row = wrapped_cursor_row.saturating_sub(scroll);
        let y = area.y + (visible_row as u16).min(area.height.saturating_sub(1));
        Position::new(x.min(area.x + area.width - 1), y)
    })
}

fn visible_line_window<'a>(
    lines: &[Line<'a>],
    scroll: usize,
    viewport_height: usize,
) -> Vec<Line<'a>> {
    lines
        .iter()
        .skip(scroll.min(lines.len()))
        .take(viewport_height)
        .cloned()
        .collect()
}

fn split_last_row(area: Rect) -> (Rect, Rect) {
    if area.height < 2 {
        return (area, Rect::new(area.x, area.y + area.height, area.width, 0));
    }
    (
        Rect::new(area.x, area.y, area.width, area.height - 1),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    )
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let surface = match (app.focus, app.center_view, app.files_context) {
        (Focus::Files, _, FilesContext::Search) => "FILES/SEARCH",
        (Focus::Files, _, FilesContext::MoveTarget) => "FILES/MOVE",
        (Focus::Files, _, FilesContext::NewTarget) => "FILES/NEW",
        (Focus::Files, _, FilesContext::Rename) => "FILES/RENAME",
        (Focus::Files, _, _) => "FILES",
        (Focus::Todo, _, _) => "TODO",
        (Focus::Agent, _, _) => "AGENT",
        (Focus::Compose, _, _) => "COMPOSE",
        (_, CenterView::Document, _) => "DOCUMENT",
        (_, CenterView::Search, _) => "SEARCH",
        (_, CenterView::DocumentSearch, _) => "FIND",
        _ => "DAILY",
    };
    let surface_segment = format!(" {surface} ");
    let permission_segment = format!(" {} ", app.permission_mode.label());
    let mouse_status = Span::styled(
        " ",
        Style::default().bg(if app.mouse_captured {
            app.theme.ui_shortcut
        } else {
            app.theme.ui_warning
        }),
    );
    let surface_style = Style::default()
        .bg(app.theme.surface_status_context)
        .fg(app.theme.text_on_accent);
    let mode_line = if app.permission_mode == PermissionMode::Bypass {
        let mut spans = vec![
            mouse_status,
            Span::styled(surface_segment.clone(), surface_style),
        ];
        let bypass_style = Style::default().bg(app.theme.surface_overlay);
        spans.push(Span::styled(" ", bypass_style));
        spans.extend("BYPASS".chars().enumerate().map(|(index, character)| {
            Span::styled(
                character.to_string(),
                bypass_style
                    .fg(animated_color(index * 8, app.animation_tick, app.theme))
                    .add_modifier(Modifier::BOLD),
            )
        }));
        spans.push(Span::styled(" ", bypass_style));
        Line::from(spans)
    } else {
        Line::from(vec![
            mouse_status,
            Span::styled(surface_segment.clone(), surface_style),
            Span::styled(
                permission_segment.clone(),
                Style::default()
                    .bg(app.theme.surface_status_mode)
                    .fg(app.theme.text_on_accent),
            ),
        ])
    };
    let status_bar_style = Style::default().bg(app.theme.surface_status_bar);
    frame.render_widget(Paragraph::new(mode_line).style(status_bar_style), area);

    let hint = footer_hint(app, area.width);
    let mode_width = 1usize
        .saturating_add(surface_segment.width())
        .saturating_add(permission_segment.width()) as u16;
    let available_status = area
        .width
        .saturating_sub(mode_width)
        .saturating_sub(hint.width() as u16)
        .saturating_sub(u16::from(!hint.is_empty()));
    if !app.status.is_empty() && available_status > 2 {
        let status = Line::from(Span::styled(
            format!(" {}", app.status),
            Style::default().fg(app.theme.ui_warning),
        ));
        frame.render_widget(
            Paragraph::new(status).style(status_bar_style),
            Rect::new(area.x + mode_width, area.y, available_status, area.height),
        );
    }
    if !hint.is_empty() {
        let width = (hint.width() as u16).min(area.width);
        frame.render_widget(
            Paragraph::new(Span::styled(
                hint,
                Style::default().fg(app.theme.text_muted),
            ))
            .style(status_bar_style)
            .alignment(Alignment::Right),
            Rect::new(area.x + area.width - width, area.y, width, area.height),
        );
    }
}

fn footer_hint(app: &App, width: u16) -> &'static str {
    if width < 28 {
        return "";
    }
    if app.overlay == Some(Overlay::Terminal) {
        return "Ctrl+` close terminal";
    }
    if width < 55 {
        return match (app.focus, app.center_view) {
            (Focus::Compose, CenterView::Document) => "Esc document",
            (Focus::Compose, _) => "Esc daily",
            (Focus::Files, _) => "Esc back · Enter open",
            (Focus::Todo, _) => "Esc back · Enter toggle",
            (Focus::Agent, _) if app.ai_running => "c cancel · C clear · Esc back",
            (Focus::Agent, _) => "C clear · Esc back",
            (Focus::Center, CenterView::Document)
                if app.document.as_ref().is_some_and(|document| {
                    matches!(document.kind, crate::app::DocumentKind::File(_))
                }) =>
            {
                if app.current_note_archived() == Some(true) {
                    "e edit · u restore · r rename · d delete"
                } else {
                    "e edit · a archive · r rename · d delete"
                }
            }
            (Focus::Center, _) => "Ctrl+P commands",
        };
    }
    match (app.focus, app.center_view) {
        (Focus::Compose, CenterView::Daily) => {
            "Enter send · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline · Ctrl+P commands"
        }
        (Focus::Compose, CenterView::Document) => {
            "Enter append · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline · Ctrl+P commands"
        }
        (Focus::Files, _) => "↑↓ select · Enter open · a/u archive/restore · e edit · / filter",
        (Focus::Todo, _) => "↑↓ select · Enter toggle · Esc back",
        (Focus::Agent, _) if app.ai_running => "c cancel · C clear session · ↑↓ scroll · ← center",
        (Focus::Agent, _) => "C clear session · ↑↓ scroll · ← center",
        (_, CenterView::Daily) if width >= 95 => {
            "i compose · f files · t todo · / search · Ctrl+P commands · ? help"
        }
        (_, CenterView::Document)
            if app.document.as_ref().is_some_and(|document| {
                matches!(document.kind, crate::app::DocumentKind::File(_))
            }) =>
        {
            if app.current_note_archived() == Some(true) {
                if width >= 85 {
                    "↑↓ scroll · e edit · u restore · r rename · d delete · / find · Esc back"
                } else {
                    "e edit · u restore · r rename · d delete · / find"
                }
            } else if width >= 85 {
                "↑↓ scroll · e edit · a archive · r rename · d delete · / find · Esc back"
            } else {
                "e edit · a archive · r rename · d delete · / find"
            }
        }
        (_, CenterView::Document) => "↑↓ scroll · e edit DailyNote · / find · Esc back",
        (_, CenterView::Search) => "type query · ↑↓ select · Enter open · Esc back",
        (_, CenterView::DocumentSearch) => "type query · ↑↓ select · Enter jump · Esc article",
        _ => "f files · t todo · Ctrl+P commands · ? help",
    }
}

fn draw_left_right_line(frame: &mut Frame, area: Rect, left: &str, right: &str, color: Color) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(left.to_string(), Style::default().fg(color))),
        area,
    );
    if !right.is_empty() {
        let width = (right.width() as u16).min(area.width);
        frame.render_widget(
            Paragraph::new(Span::styled(right.to_string(), Style::default().fg(color)))
                .alignment(Alignment::Right),
            Rect::new(area.x + area.width - width, area.y, width, 1),
        );
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

fn draw_terminal(
    frame: &mut Frame,
    app: &mut App,
    root: Rect,
    cursor_position: &mut Option<Position>,
) -> Rect {
    let maximum_width = root.width.saturating_sub(4).max(root.width.min(1));
    let maximum_height = root.height.saturating_sub(2).max(root.height.min(1));
    let width = root
        .width
        .saturating_mul(4)
        .div_ceil(5)
        .max(root.width.min(40))
        .min(maximum_width);
    let height = root
        .height
        .saturating_mul(4)
        .div_ceil(5)
        .max(root.height.min(12))
        .min(maximum_height);
    let area = centered_rect(root, width, height);
    if area.width == 0 || area.height == 0 {
        return area;
    }

    clear_widget(frame, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Terminal ")
        .style(Style::default().bg(app.theme.surface_overlay))
        .border_style(focus_border(true, app.theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_animated_border(frame, area, app.animation_tick, app.theme);
    if inner.width == 0 || inner.height == 0 {
        return area;
    }

    if let Some(snapshot) = app.terminal_snapshot(inner.height, inner.width) {
        draw_terminal_snapshot(frame, inner, &snapshot, app.theme, cursor_position);
    }
    area
}

fn draw_terminal_snapshot(
    frame: &mut Frame,
    area: Rect,
    snapshot: &TerminalSnapshot,
    theme: Theme,
    cursor_position: &mut Option<Position>,
) {
    let (rows, cols) = snapshot.size();
    let rows = rows.min(area.height);
    let cols = cols.min(area.width);
    let buffer = frame.buffer_mut();
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = snapshot.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut foreground = terminal_color(cell.fgcolor(), theme.text_primary);
            let mut background = terminal_color(cell.bgcolor(), theme.surface_overlay);
            if cell.inverse() {
                std::mem::swap(&mut foreground, &mut background);
            }
            let mut style = Style::default().fg(foreground).bg(background);
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            let contents = cell.contents();
            buffer[(area.x + col, area.y + row)]
                .set_symbol(if contents.is_empty() { " " } else { &contents })
                .set_style(style);
        }
    }

    if !snapshot.hide_cursor() {
        let (row, col) = snapshot.cursor_position();
        if row < area.height && col < area.width {
            *cursor_position = Some(Position::new(area.x + col, area.y + row));
        }
    }
}

fn terminal_color(color: vt100::Color, default: Color) -> Color {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

fn approval_dialog_width(root_width: u16) -> u16 {
    if root_width >= APPROVAL_SIDE_BY_SIDE_MIN_WIDTH.saturating_add(8) {
        APPROVAL_SIDE_BY_SIDE_WIDTH
    } else {
        APPROVAL_UNIFIED_WIDTH
    }
}


/// Render every modal interaction through one fixed-width, bounded-height
/// command surface. The body changes by mode, but title, scrolling, option
/// selection, input and footer geometry remain identical.
fn draw_dialog(
    frame: &mut Frame,
    app: &mut App,
    root: Rect,
    cursor_position: &mut Option<Position>,
) -> Rect {
    let Some(dialog) = app.dialog.clone() else {
        return Rect::new(root.x, root.y, 0, 0);
    };
    let width = match dialog.mode {
        DialogMode::Approval => approval_dialog_width(root.width),
        DialogMode::Informational => 92,
        DialogMode::CommandPalette => DIALOG_WIDTH,
        _ => DIALOG_WIDTH,
    }
    .min(root.width.saturating_sub(4).max(root.width.min(1)));
    let text_width = width.saturating_sub(4).max(1) as usize;
    let message_rows = if dialog.purpose == DialogPurpose::Help {
        help_lines(app.theme).len() as u16
    } else {
        wrap_spans_to_width(&[Span::raw(dialog.message.clone())], text_width)
            .len()
            .max(1) as u16
    };
    let option_count = dialog.options.len() as u16;
    let desired_height = match dialog.mode {
        DialogMode::Confirm => 5,
        DialogMode::FreeText
            if matches!(
                dialog.purpose,
                DialogPurpose::NewFile | DialogPurpose::RenameFile
            ) =>
        {
            5
        }
        DialogMode::FreeText => 11,
        DialogMode::SelectOrInput => message_rows
            .min(8)
            .saturating_add(selection_list_height(
                option_count.saturating_add(1),
                SELECT_OPTION_HEIGHT,
            ))
            .saturating_add(4)
            .saturating_add(1)
            .saturating_add(2),
        DialogMode::SingleSelect | DialogMode::MultiSelect => message_rows
            .min(8)
            .saturating_add(selection_list_height(option_count, SELECT_OPTION_HEIGHT))
            .saturating_add(1)
            .saturating_add(2),
        DialogMode::Approval => root.height.saturating_sub(4).min(36),
        DialogMode::Informational => root.height.saturating_sub(2).min(30),
        DialogMode::CommandPalette => selection_list_height(option_count.min(8), 3)
            .saturating_add(6)
            .max(7),
    };
    let height = desired_height
        .max(3)
        .min(root.height.saturating_sub(2).max(root.height.min(1)));
    // Keep the command palette's query field anchored while its result list
    // shrinks. Centering each filtered height would make the input jump.
    let area = if dialog.mode == DialogMode::CommandPalette {
        let maximum_height = selection_list_height(8, 3).saturating_add(6);
        let anchor_height =
            maximum_height.min(root.height.saturating_sub(2).max(root.height.min(1)));
        let anchor = centered_rect(root, width, anchor_height);
        Rect::new(anchor.x, anchor.y, width, height)
    } else {
        centered_rect(root, width, height)
    };
    if area.width == 0 || area.height == 0 {
        return area;
    }
    clear_widget(frame, area);
    let destructive = matches!(
        dialog.purpose,
        DialogPurpose::DeleteDaily | DialogPurpose::DeleteFile
    );
    let border = match dialog.mode {
        _ if destructive => app.theme.ui_error,
        DialogMode::Approval => app.theme.ui_warning,
        DialogMode::FreeText => app.theme.ui_dialog_input,
        DialogMode::SelectOrInput
        | DialogMode::SingleSelect
        | DialogMode::MultiSelect
        | DialogMode::CommandPalette => app.theme.ui_dialog_choice,
        _ => app.theme.text_disabled,
    };
    let modal_background = if matches!(
        dialog.purpose,
        DialogPurpose::NewFile | DialogPurpose::RenameFile
    ) {
        app.theme.surface_panel
    } else {
        app.theme.surface_overlay
    };
    let border_style = if dialog.mode == DialogMode::CommandPalette {
        focus_border(true, app.theme)
    } else {
        Style::default().fg(border)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(format!(" {} ", dialog.title))
        .style(Style::default().bg(modal_background))
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return area;
    }

    app.dialog_hitboxes.clear();
    match dialog.mode {
        DialogMode::Confirm => {
            let body = Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(1),
            );
            frame.render_widget(
                Paragraph::new(dialog.message.clone())
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: false })
                    .style(if destructive {
                        Style::default()
                            .fg(app.theme.ui_error)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    }),
                body,
            );
            if destructive {
                draw_dialog_footer_line(
                    frame,
                    inner,
                    Line::from(vec![
                        Span::styled(
                            "Enter/Y confirm",
                            Style::default()
                                .fg(app.theme.ui_error)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" · ", Style::default().fg(app.theme.text_muted)),
                        Span::styled(
                            "N/Esc cancel",
                            Style::default().fg(app.theme.text_secondary),
                        ),
                    ]),
                );
            } else {
                draw_dialog_footer(frame, inner, "Enter/Y confirm · N/Esc cancel", app.theme);
            }
        }
        DialogMode::FreeText => {
            let (input, footer) = split_last_row(inner);
            if matches!(
                dialog.purpose,
                DialogPurpose::NewFile | DialogPurpose::RenameFile
            ) {
                let single = Rect::new(input.x, input.y, input.width, 1);
                if let Some(position) = draw_single_line_input(
                    frame,
                    single,
                    &dialog.message,
                    &dialog.input,
                    dialog.cursor,
                    true,
                    app.theme,
                ) {
                    *cursor_position = Some(position);
                }
                draw_dialog_footer(frame, footer, "Enter save · Esc cancel", app.theme);
            } else {
                if let Some(position) = draw_multiline_input(
                    frame,
                    input,
                    &dialog.input,
                    dialog.cursor,
                    "Optional prompt; empty formats this daily note",
                    true,
                    app.theme,
                ) {
                    *cursor_position = Some(position);
                }
                draw_dialog_footer(
                    frame,
                    footer,
                    "Enter submit · Shift/Ctrl/Alt+Enter newline · Esc cancel",
                    app.theme,
                );
            }
        }
        DialogMode::Approval => {
            let (content, footer) = split_last_row(inner);
            let lines = if content.width >= APPROVAL_SIDE_BY_SIDE_MIN_WIDTH {
                side_by_side_diff_lines(&dialog.message, content.width as usize, app.theme)
            } else {
                crate::markdown::to_lines_at_width(
                    &format!("```diff\n{}\n```", dialog.message),
                    content.width as usize,
                    app.theme,
                )
            };
            let maximum = lines.len().saturating_sub(content.height as usize);
            let scroll = dialog.scroll.min(maximum as u16);
            if let Some(state) = app.dialog.as_mut() {
                state.scroll = scroll;
            }
            app.approval_scroll = scroll;
            frame.render_widget(
                Paragraph::new(visible_line_window(
                    &lines,
                    scroll as usize,
                    content.height as usize,
                )),
                content,
            );
            draw_dialog_footer(
                frame,
                footer,
                "Enter/Y approve · N/Esc deny · ↑↓ scroll · Tab bypass",
                app.theme,
            );
        }
        DialogMode::Informational => {
            let lines = help_lines(app.theme);
            let maximum = lines.len().saturating_sub(inner.height as usize);
            let scroll = dialog.scroll.min(maximum as u16);
            if let Some(state) = app.dialog.as_mut() {
                state.scroll = scroll;
            }
            app.help_scroll = scroll;
            frame.render_widget(
                Paragraph::new(visible_line_window(
                    &lines,
                    scroll as usize,
                    inner.height as usize,
                )),
                inner,
            );
        }
        DialogMode::CommandPalette => {
            draw_command_palette(frame, app, &dialog, inner, cursor_position)
        }
        DialogMode::SingleSelect | DialogMode::MultiSelect | DialogMode::SelectOrInput => {
            draw_select_dialog(frame, app, &dialog, inner, cursor_position);
        }
    }
    area
}

fn draw_command_palette(
    frame: &mut Frame,
    app: &mut App,
    dialog: &DialogState,
    inner: Rect,
    cursor_position: &mut Option<Position>,
) {
    if inner.height == 0 {
        return;
    }
    let input = Rect::new(inner.x, inner.y, inner.width, 1);
    if let Some(position) = draw_single_line_input(
        frame,
        input,
        "/ ",
        &dialog.input,
        dialog.cursor,
        true,
        app.theme,
    ) {
        *cursor_position = Some(position);
    }
    if inner.height > 1 {
        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(app.theme.ui_border_subtle)),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }

    let footer_y = inner.y + inner.height.saturating_sub(1);
    let gap_y = footer_y.saturating_sub(1);
    let options = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        gap_y.saturating_sub(inner.y.saturating_add(2)),
    );
    let visible_items = visible_selection_items(options.height, 3);
    if dialog.options.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching commands")
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.theme.text_muted)),
            options,
        );
    } else if options.height > 0 {
        let list_start = dialog
            .selected
            .saturating_sub(visible_items.saturating_sub(1));
        let options_end = options.y.saturating_add(options.height);
        let mut y = options.y.saturating_add(1);
        for (index, option) in dialog
            .options
            .iter()
            .enumerate()
            .skip(list_start)
            .take(visible_items)
        {
            if y >= options_end {
                break;
            }
            let item_height = 3.min(options_end.saturating_sub(y));
            let item_area = Rect::new(options.x, y, options.width, item_height);
            let selected = index == dialog.selected;
            let selection_style = if selected {
                Style::default()
                    .fg(app.theme.selection_foreground)
                    .bg(app.theme.selection_background)
            } else {
                Style::default()
            };
            let selection_area = selected.then(|| shared_selection_area(options, y, item_height));
            if let Some(selection_area) = selection_area {
                frame.render_widget(Block::default().style(selection_style), selection_area);
            }
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        option.label.clone(),
                        Style::default()
                            .fg(if selected {
                                app.theme.selection_foreground
                            } else {
                                app.theme.text_secondary
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                ])),
                Rect::new(
                    options.x.saturating_add(1),
                    y,
                    options.width.saturating_sub(1),
                    1,
                ),
            );
            if item_height > 1 {
                let description = option.hint.as_deref().unwrap_or("");
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(
                            description,
                            if selected {
                                Style::default()
                                    .fg(app.theme.selection_foreground)
                                    .add_modifier(Modifier::DIM)
                            } else {
                                Style::default().fg(app.theme.text_muted)
                            },
                        ),
                    ])),
                    Rect::new(
                        options.x.saturating_add(1),
                        y + 1,
                        options.width.saturating_sub(1),
                        1,
                    ),
                );
            }
            if let Some(selection_area) = selection_area {
                draw_selection_indicator(frame, selection_area, app.theme);
            }
            app.dialog_hitboxes.push(crate::model::DialogOptionHitbox {
                index,
                area: item_area,
            });
            y = y.saturating_add(item_height);
        }
    }
    draw_dialog_footer(
        frame,
        Rect::new(inner.x, footer_y, inner.width, 1),
        "↑↓ select · Enter run · Esc close",
        app.theme,
    );
}

fn draw_dialog_footer(frame: &mut Frame, area: Rect, text: &str, theme: Theme) {
    draw_dialog_footer_line(
        frame,
        area,
        Line::styled(text.to_string(), Style::default().fg(theme.text_muted)),
    );
}

fn draw_dialog_footer_line(frame: &mut Frame, area: Rect, line: Line<'static>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let footer = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        1,
    );
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), footer);
}

fn draw_select_dialog(
    frame: &mut Frame,
    app: &mut App,
    dialog: &DialogState,
    inner: Rect,
    cursor_position: &mut Option<Position>,
) {
    if inner.height == 0 {
        return;
    }
    let has_input = dialog.mode == DialogMode::SelectOrInput;
    let footer_height = 1;
    let input_height = if has_input {
        4.min(inner.height.saturating_sub(footer_height))
    } else {
        0
    };
    let message_height = if dialog.message.is_empty() {
        0
    } else {
        wrap_spans_to_width(&[Span::raw(dialog.message.clone())], inner.width as usize)
            .len()
            .min(8) as u16
    };
    let available = inner
        .height
        .saturating_sub(footer_height)
        .saturating_sub(input_height);
    let option_capacity = available.saturating_sub(message_height);
    let option_items = dialog.options.len() + usize::from(has_input);
    let option_height = selection_list_height(
        u16::try_from(option_items).unwrap_or(u16::MAX),
        SELECT_OPTION_HEIGHT,
    )
    .min(option_capacity);
    let message = Rect::new(inner.x, inner.y, inner.width, message_height);
    let options = Rect::new(
        inner.x,
        message.y + message.height,
        inner.width,
        option_height,
    );
    let input = Rect::new(
        inner.x,
        options.y + options.height,
        inner.width,
        input_height,
    );
    let footer = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(footer_height),
        inner.width,
        footer_height,
    );
    if message.height > 0 {
        frame.render_widget(
            Paragraph::new(dialog.message.clone())
                .wrap(Wrap { trim: false })
                .style(Style::default().add_modifier(Modifier::BOLD)),
            message,
        );
    }
    let visible_items = visible_selection_items(options.height, SELECT_OPTION_HEIGHT);
    let list_start = dialog
        .selected
        .saturating_sub(visible_items.saturating_sub(1))
        .min(option_items.saturating_sub(visible_items));
    let options_end = options.y.saturating_add(options.height);
    for (index, option) in dialog
        .options
        .iter()
        .enumerate()
        .skip(list_start)
        .take(visible_items)
    {
        let row = index - list_start;
        let y = selection_item_y(options, row, SELECT_OPTION_HEIGHT);
        if y >= options_end {
            break;
        }
        let item_height = SELECT_OPTION_HEIGHT.min(options_end.saturating_sub(y));
        let item_area = Rect::new(options.x, y, options.width, item_height);
        let selected = dialog.selected == index;
        let style = if selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .bg(app.theme.selection_background)
        } else {
            Style::default().fg(app.theme.text_disabled)
        };
        let label = if dialog.mode == DialogMode::MultiSelect {
            let marker = if dialog.checked.get(index).copied().unwrap_or(false) {
                "[x]"
            } else {
                "[ ]"
            };
            format!("{marker} {}", option.label)
        } else {
            option.label.clone()
        };
        let mut spans = vec![Span::styled(label, style)];
        if let Some(hint) = &option.hint {
            spans.push(Span::styled(
                format!("  {hint}"),
                if selected {
                    style.add_modifier(Modifier::DIM)
                } else {
                    Style::default().fg(app.theme.text_muted)
                },
            ));
        }
        let selection_area = selected.then(|| shared_selection_area(options, y, item_height));
        if let Some(selection_area) = selection_area {
            frame.render_widget(
                Block::default().style(
                    Style::default()
                        .fg(app.theme.selection_foreground)
                        .bg(app.theme.selection_background),
                ),
                selection_area,
            );
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(
                item_area.x.saturating_add(2),
                item_area.y,
                item_area.width.saturating_sub(2),
                1,
            ),
        );
        if let Some(selection_area) = selection_area {
            draw_selection_indicator(frame, selection_area, app.theme);
        }
        app.dialog_hitboxes.push(crate::model::DialogOptionHitbox {
            index,
            area: item_area,
        });
        if dialog.purpose == DialogPurpose::WikiLinkChoice {
            app.wiki_link_hitboxes.push(crate::model::WikiLinkHitbox {
                index,
                area: item_area,
            });
        }
    }
    if has_input && input.height > 0 {
        let custom_selected = dialog.selected >= dialog.options.len();
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(" Your answer ")
            .border_style(focus_border(custom_selected, app.theme));
        let input_inner = input_block.inner(input);
        frame.render_widget(input_block, input);
        if let Some(position) = draw_multiline_input(
            frame,
            input_inner,
            &dialog.input,
            dialog.cursor,
            "Type a different response",
            custom_selected,
            app.theme,
        ) {
            *cursor_position = Some(position);
        }
        let other_index = dialog.options.len();
        if other_index >= list_start && other_index < list_start + visible_items {
            let row = other_index - list_start;
            let y = selection_item_y(options, row, SELECT_OPTION_HEIGHT);
            let item_height = SELECT_OPTION_HEIGHT.min(options_end.saturating_sub(y));
            let item_area = Rect::new(options.x, y, options.width, item_height);
            let selection_area =
                custom_selected.then(|| shared_selection_area(options, y, item_height));
            if let Some(selection_area) = selection_area {
                frame.render_widget(
                    Block::default().style(
                        Style::default()
                            .fg(app.theme.selection_foreground)
                            .bg(app.theme.selection_background),
                    ),
                    selection_area,
                );
            }
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "Other answer",
                    if custom_selected {
                        Style::default()
                            .fg(app.theme.selection_foreground)
                            .bg(app.theme.selection_background)
                    } else {
                        Style::default().fg(app.theme.text_disabled)
                    },
                ))),
                Rect::new(
                    item_area.x.saturating_add(2),
                    item_area.y,
                    item_area.width.saturating_sub(2),
                    1,
                ),
            );
            if let Some(selection_area) = selection_area {
                draw_selection_indicator(frame, selection_area, app.theme);
            }
            app.dialog_hitboxes.push(crate::model::DialogOptionHitbox {
                index: other_index,
                area: item_area,
            });
        }
    }
    let footer_text = match dialog.mode {
        DialogMode::MultiSelect => "↑↓ move · Space toggle · Enter submit · Esc cancel",
        DialogMode::SelectOrInput => "↑↓ choose · Enter submit · type custom · Esc cancel",
        DialogMode::SingleSelect if dialog.purpose == DialogPurpose::AskUser => {
            "↑↓ choose · Enter submit · Esc stop"
        }
        _ => "↑↓ choose · Enter open · Esc cancel",
    };
    draw_dialog_footer(frame, footer, footer_text, app.theme);
}

fn help_lines(theme: Theme) -> Vec<Line<'static>> {
    let heading = |text: &str| {
        Line::from(Span::styled(
            text.to_string(),
            Style::default()
                .fg(theme.ui_section_heading)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let key = |keys: &str, description: &str| {
        Line::from(vec![
            Span::styled(
                format!(" {keys:<16}"),
                Style::default()
                    .fg(theme.ui_shortcut)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(description.to_string()),
        ])
    };
    vec![
        heading("Workspace"),
        key("f / t", "focus Files / Todo"),
        key("← → / ↑ ↓", "move focus between panes"),
        key("Tab", "toggle approve / bypass mode"),
        key("Ctrl+P", "open command palette"),
        key("Ctrl+`", "toggle workspace terminal"),
        key("Esc", "return / cancel"),
        key("?", "open this help"),
        Line::default(),
        heading("Daily"),
        key("i / Enter", "focus Compose"),
        key("j k / ↑ ↓", "select DailyNote"),
        key("m a n", "move · archive · new file"),
        key("v e d / AI", "view · edit · delete · Agent"),
        key("/ / u", "search · undo"),
        Line::default(),
        heading("Compose / editor"),
        key("Enter", "send / save"),
        key("Ctrl+Enter", "send prompt directly to Agent"),
        key("Ctrl+U", "recall the last append into Compose"),
        key("Ctrl+J", "insert newline"),
        key("Esc", "leave / cancel"),
        Line::default(),
        heading("Files"),
        key("j k / ↑ ↓", "select"),
        key("Enter / e", "open / external editor"),
        key("/ r d", "filter · rename · delete"),
        Line::default(),
        heading("Todo / document"),
        key("Enter / Space", "toggle todo"),
        key("j k / PgUp/Dn", "scroll document"),
        key("i / Enter", "append while reading"),
        Line::default(),
        heading("Agent approval"),
        key("Enter / y", "approve displayed diff"),
        key("n / Esc", "deny displayed diff"),
        Line::default(),
        heading("Agent questions"),
        key("↑ ↓ / Enter", "choose and submit an option"),
        key("type / Esc", "custom answer / cancel question"),
        Line::default(),
        heading("Agent output"),
        key("c", "cancel running Agent while panel is focused"),
        key("C", "clear Agent conversation and start a new session"),
    ]
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn wrapped_row_count(line_width: usize, area_width: usize) -> usize {
    if line_width == 0 || area_width == 0 {
        1
    } else {
        line_width.div_ceil(area_width)
    }
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn cursor_row_col(input: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0;
    let mut column = 0;
    for (index, character) in input.chars().enumerate() {
        if index == cursor {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += character.width().unwrap_or(1);
        }
    }
    (line, column)
}

/// Greedy display-width wrapping that keeps span styles and explicit newlines.
fn wrap_spans_to_width(spans: &[Span<'_>], width: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut row_width = 0;
    for span in spans {
        for character in span.content.chars() {
            if character == '\n' {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
                continue;
            }
            let character_width = character.width().unwrap_or(1);
            if width > 0 && row_width + character_width > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
            }
            if let Some(last) = row.last_mut().filter(|last| last.style == span.style) {
                last.content.to_mut().push(character);
            } else {
                row.push(Span::styled(character.to_string(), span.style));
            }
            row_width += character_width;
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}
