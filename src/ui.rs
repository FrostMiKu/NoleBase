//! Terminal rendering for the full-width workspace.

use std::collections::HashMap;
use std::path::Path;

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
use crate::model::{
    Action, ButtonHitbox, FileGroup, FileGroupHitbox, FileHitbox, FileListRow, LinkHitbox,
    SearchHit, SearchHitbox, TodoHitbox,
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
    register_link_hitboxes(&mut app.link_hitboxes, &rendered_links, inner, scroll);
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
    entry: &crate::app::AgentPanelEntry,
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
        crate::app::AgentPanelEntry::Prompt { text, muted } => {
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
        crate::app::AgentPanelEntry::Assistant { text, .. } => {
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
        crate::app::AgentPanelEntry::Tool { text, active } => {
            lines.extend(if *active && animate {
                animated_activity_lines(text, width, tick, theme)
            } else {
                activity_lines(text, width, theme)
            });
        }
        crate::app::AgentPanelEntry::Error(text) => lines.push(Line::from(Span::styled(
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
            crate::app::AgentPanelEntry::Tool { active: true, .. }
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
    if app.agent_usage.is_empty() {
        return None;
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
    let text = if width >= 44 {
        format!("↑{input} ↓{output} · {tps} t/s · Cache {cache_read} {cache_rate:.0}%")
    } else if width >= 30 {
        format!("↑{input} ↓{output} · {tps}t/s · C{cache_read} {cache_rate:.0}%")
    } else {
        format!("↑{input} ↓{output}")
    };
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
        FilesContext::Browse => " NoleBase ",
        FilesContext::Search => " NoleBase · search ",
        FilesContext::MoveTarget => " NoleBase · move to ",
        FilesContext::NewTarget => " NoleBase · new ",
        FilesContext::Rename => " NoleBase · rename ",
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
                .bg(if focused {
                    app.theme.surface_selection
                } else {
                    app.theme.surface_selection_inactive
                })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let selection_area = if selected && matches!(row, FileListRow::File(_)) {
            let selection_y = y.saturating_sub(1).max(list_area.y);
            let selection_end = y
                .saturating_add(3)
                .min(list_area.y.saturating_add(list_area.height));
            Some(Rect::new(
                list_area.x,
                selection_y,
                list_area.width,
                selection_end.saturating_sub(selection_y),
            ))
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
                        Span::styled(marker, Style::default().fg(app.theme.ui_group_marker)),
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
                        Style::default().fg(app.theme.text_muted),
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
                    let prefix = if file.archived {
                        "  Archived · "
                    } else {
                        "  "
                    };
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            format!("{prefix}{}", modified.format("%y/%m/%d %H:%M")),
                            Style::default().fg(if selected {
                                app.theme.text_secondary
                            } else {
                                app.theme.text_muted
                            }),
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
                    for rail_y in selection_area.y..selection_area.y + selection_area.height {
                        frame.render_widget(
                            Paragraph::new(Span::styled(
                                "▌",
                                Style::default().fg(app.theme.ui_selection_indicator),
                            )),
                            Rect::new(selection_area.x, rail_y, 1, 1),
                        );
                    }
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
        let marker_style = if item.checked {
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
        if focused && index == selected {
            text_style = text_style.bg(app.theme.surface_selection);
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
        if focused && index == selected {
            let selection_y = y.saturating_sub(1).max(inner.y);
            let selection_end = y
                .saturating_add(layout_height)
                .min(inner.y.saturating_add(inner.height));
            frame.render_widget(
                Block::default().style(Style::default().bg(app.theme.surface_selection)),
                Rect::new(
                    inner.x,
                    selection_y,
                    inner.width,
                    selection_end.saturating_sub(selection_y),
                ),
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
        hitboxes.push(LinkHitbox {
            target: link.target.clone(),
            area: Rect::new(
                viewport.x.saturating_add(column as u16),
                viewport.y.saturating_add((link.row - scroll) as u16),
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
        let hint = if focused && toolbar.width >= 62 {
            match app.center_view {
                CenterView::Document => "Enter append · Ctrl+Enter Agent · Ctrl+J newline",
                _ => "Enter send · Ctrl+Enter Agent · Ctrl+J newline",
            }
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
    let (rendered_links, rendered_images, document_scroll) = {
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
        (rendered_links, rendered_images, document_scroll)
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
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .title(format!(" Searcher · {} ", app.search_results.len()))
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

    let visible = results.height as usize;
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
        let spans = match hit {
            SearchHit::Daily { text, .. } => vec![
                Span::styled("• ", Style::default().fg(app.theme.ui_search_marker)),
                Span::raw(text.clone()),
            ],
            SearchHit::FileLine {
                path,
                line_no,
                text,
            } => {
                let name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?");
                vec![
                    Span::styled(
                        format!("{name}:{line_no} "),
                        Style::default().fg(app.theme.text_muted),
                    ),
                    Span::raw(text.clone()),
                ]
            }
            SearchHit::DocumentLine { line_no, text } => vec![
                Span::styled(
                    format!("line {line_no} "),
                    Style::default().fg(app.theme.text_muted),
                ),
                Span::raw(text.clone()),
            ],
        };
        let style = if index == selected {
            Style::default().bg(app.theme.surface_selection)
        } else {
            Style::default()
        };
        let row_area = Rect::new(results.x, results.y + row as u16, results.width, 1);
        frame.render_widget(Paragraph::new(Line::from(spans)).style(style), row_area);
        if interactive {
            app.search_hitboxes.push(SearchHitbox {
                index,
                area: row_area,
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
    let surface_style = Style::default()
        .bg(app.theme.surface_status_context)
        .fg(app.theme.text_on_accent);
    let mode_line = if app.permission_mode == PermissionMode::Bypass {
        let mut spans = vec![Span::styled(surface_segment.clone(), surface_style)];
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
    let mode_width = surface_segment
        .width()
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
                "e edit · a archive · r rename · d delete"
            }
            (Focus::Center, _) => "Ctrl+P commands",
        };
    }
    match (app.focus, app.center_view) {
        (Focus::Compose, CenterView::Daily) => {
            "Enter send · Ctrl+Enter Agent · Ctrl+J newline · Ctrl+P commands"
        }
        (Focus::Compose, CenterView::Document) => {
            "Enter append · Ctrl+Enter Agent · Ctrl+J newline · Ctrl+P commands"
        }
        (Focus::Files, _) => "↑↓ select · Enter open · a/u archive/restore · e edit · / filter",
        (Focus::Todo, _) => "↑↓ select · Enter toggle · Esc back",
        (Focus::Agent, _) if app.ai_running => "c cancel · C clear session · ↑↓ scroll · ← center",
        (Focus::Agent, _) => "C clear session · ↑↓ scroll · ← center",
        (_, CenterView::Daily) if width >= 95 => {
            "i compose · f files · T todo · / search · Ctrl+P commands · ? help"
        }
        (_, CenterView::Document)
            if app.document.as_ref().is_some_and(|document| {
                matches!(document.kind, crate::app::DocumentKind::File(_))
            }) =>
        {
            if width >= 85 {
                "↑↓ scroll · e edit · a archive · r rename · d delete · / find · Esc back"
            } else {
                "e edit · a archive · r rename · d delete · / find"
            }
        }
        (_, CenterView::Document) => "↑↓ scroll · e edit DailyNote · / find · Esc back",
        (_, CenterView::Search) => "type query · ↑↓ select · Enter open · Esc back",
        (_, CenterView::DocumentSearch) => "type query · ↑↓ select · Enter jump · Esc article",
        _ => "f files · T todo · Ctrl+P commands · ? help",
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
    let _ = overlay;
    draw_dialog(frame, app, root, cursor_position)
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
        DialogMode::Approval => 110,
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
            .saturating_add(option_count.saturating_add(1))
            .saturating_add(4)
            .saturating_add(1)
            .saturating_add(2),
        DialogMode::SingleSelect | DialogMode::MultiSelect => message_rows
            .min(8)
            .saturating_add(option_count)
            .saturating_add(1)
            .saturating_add(2),
        DialogMode::Approval => root.height.saturating_sub(4).min(36),
        DialogMode::Informational => root.height.saturating_sub(2).min(30),
        DialogMode::CommandPalette => option_count
            .min(8)
            .saturating_mul(3)
            .saturating_add(6)
            .max(7),
    };
    let height = desired_height
        .max(3)
        .min(root.height.saturating_sub(2).max(root.height.min(1)));
    let area = centered_rect(root, width, height);
    if area.width == 0 || area.height == 0 {
        return area;
    }
    clear_widget(frame, area);
    let border = match dialog.mode {
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
                    .wrap(Wrap { trim: false }),
                body,
            );
            draw_dialog_footer(frame, inner, "Enter/Y confirm · N/Esc cancel", app.theme);
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
                    "Optional prompt; empty uses the card content",
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
            let lines = crate::markdown::to_lines_at_width(
                &format!("```diff\n{}\n```", dialog.message),
                content.width as usize,
                app.theme,
            );
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
    let visible_items = (options.height as usize).div_ceil(3).max(1);
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
        let mut y = options.y;
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
                Style::default().bg(app.theme.surface_selection)
            } else {
                Style::default()
            };
            if selected {
                let selection_y = y.saturating_sub(1).max(options.y);
                let selection_end = y.saturating_add(3).min(options_end);
                let selection_area = Rect::new(
                    options.x,
                    selection_y,
                    options.width,
                    selection_end.saturating_sub(selection_y),
                );
                frame.render_widget(Block::default().style(selection_style), selection_area);
                for rail_y in selection_y..selection_end {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            "▌",
                            Style::default().fg(app.theme.ui_selection_indicator),
                        )),
                        Rect::new(options.x, rail_y, 1, 1),
                    );
                }
            }
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        option.label.clone(),
                        Style::default()
                            .fg(app.theme.text_secondary)
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
                        Span::styled(description, Style::default().fg(app.theme.text_muted)),
                    ])),
                    Rect::new(
                        options.x.saturating_add(1),
                        y + 1,
                        options.width.saturating_sub(1),
                        1,
                    ),
                );
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
    if area.width == 0 || area.height == 0 {
        return;
    }
    let footer = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.text_muted)),
        footer,
    );
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
    let option_extra = u16::from(has_input);
    let option_capacity = available.saturating_sub(message_height) as usize;
    let option_height = dialog
        .options
        .len()
        .saturating_add(option_extra as usize)
        .min(option_capacity) as u16;
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
    let total_rows = dialog.options.len() + usize::from(has_input);
    let visible_rows = options.height as usize;
    let list_start = dialog
        .selected
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(total_rows.saturating_sub(visible_rows));
    for (index, option) in dialog
        .options
        .iter()
        .enumerate()
        .skip(list_start)
        .take(options.height as usize)
    {
        let row = index - list_start;
        let selected = dialog.selected == index;
        let style = if selected {
            Style::default()
                .fg(app.theme.text_on_accent)
                .bg(app.theme.ui_action)
        } else {
            Style::default().fg(app.theme.text_disabled)
        };
        let marker = if dialog.mode == DialogMode::MultiSelect {
            if dialog.checked.get(index).copied().unwrap_or(false) {
                "[x]"
            } else {
                "[ ]"
            }
        } else if selected {
            ">"
        } else {
            " "
        };
        let mut spans = vec![Span::styled(format!("{marker} {}", option.label), style)];
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
        let row_area = Rect::new(options.x, options.y + row as u16, options.width, 1);
        frame.render_widget(Paragraph::new(Line::from(spans)), row_area);
        app.dialog_hitboxes.push(crate::model::DialogOptionHitbox {
            index,
            area: row_area,
        });
        if dialog.purpose == DialogPurpose::WikiLinkChoice {
            app.wiki_link_hitboxes.push(crate::model::WikiLinkHitbox {
                index,
                area: row_area,
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
        if other_index >= list_start && other_index < list_start + visible_rows {
            let row = other_index - list_start;
            let row_area = Rect::new(options.x, options.y + row as u16, options.width, 1);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "> Other answer",
                    if custom_selected {
                        Style::default()
                            .fg(app.theme.text_on_accent)
                            .bg(app.theme.ui_action)
                    } else {
                        Style::default().fg(app.theme.text_disabled)
                    },
                ))),
                row_area,
            );
            app.dialog_hitboxes.push(crate::model::DialogOptionHitbox {
                index: other_index,
                area: row_area,
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
        key("f / T", "focus Files / Todo"),
        key("← → / ↑ ↓", "move focus between panes"),
        key("Tab", "toggle approve / bypass mode"),
        key("Ctrl+P", "open command palette"),
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

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn clearing_a_floating_widget_sanitizes_wide_characters_at_its_edges() {
        let backend = TestBackend::new(12, 5);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("界"), Rect::new(4, 2, 2, 1));
                frame.render_widget(Paragraph::new("界"), Rect::new(8, 2, 2, 1));

                clear_widget(frame, Rect::new(5, 1, 4, 3));

                let left_outside = &frame.buffer_mut()[(4, 2)];
                assert_eq!(left_outside.symbol(), " ");
                assert_eq!(left_outside.diff_option, CellDiffOption::None);
                assert_eq!(frame.buffer_mut()[(5, 2)].symbol(), " ");

                assert_eq!(frame.buffer_mut()[(8, 2)].symbol(), " ");
                let right_outside = &frame.buffer_mut()[(9, 2)];
                assert_eq!(right_outside.symbol(), " ");
                assert_eq!(right_outside.diff_option, CellDiffOption::None);

                assert_eq!(frame.buffer_mut()[(6, 0)].diff_option, CellDiffOption::None);
                assert_eq!(frame.buffer_mut()[(6, 4)].diff_option, CellDiffOption::None);
            })
            .unwrap();
    }

    #[test]
    fn vs16_continuation_cells_are_not_emitted_by_the_buffer_diff() {
        let area = Rect::new(0, 0, 4, 1);
        let previous = Buffer::with_lines(["abcd"]);
        let mut next = Buffer::empty(area);
        next.set_string(0, 0, "☀️xy", Style::default().bg(Color::Blue));

        skip_vs16_continuation_cells(&mut next);

        assert_eq!(next[(1, 0)].diff_option, CellDiffOption::Skip);
        let updates = previous.diff(&next);
        assert!(updates
            .iter()
            .any(|(x, _, cell)| *x == 0 && cell.symbol() == "☀️"));
        assert!(!updates.iter().any(|(x, _, _)| *x == 1));
        assert!(updates
            .iter()
            .any(|(x, _, cell)| *x == 2 && cell.symbol() == "x"));
    }

    use crate::theme::catppuccin as ctp;

    fn animated_activity_lines(text: &str, width: usize, tick: u64) -> Vec<Line<'static>> {
        super::animated_activity_lines(text, width, tick, Theme::default())
    }

    fn activity_lines(text: &str, width: usize) -> Vec<Line<'static>> {
        super::activity_lines(text, width, Theme::default())
    }

    fn render_agent_entry(
        entry: &crate::app::AgentPanelEntry,
        width: usize,
        tick: u64,
        animate: bool,
    ) -> (
        Vec<Line<'static>>,
        Vec<crate::markdown::RenderedLink>,
        Vec<mbtui::ImagePlacement>,
    ) {
        super::render_agent_entry(entry, width, tick, animate, Theme::default())
    }

    fn render_daily_note(
        note: &crate::model::DailyNote,
        date_label: String,
        width: usize,
    ) -> crate::app::DailyCardRenderCache {
        super::render_daily_note(note, date_label, width, Theme::default())
    }

    fn animated_color(position: usize, tick: u64) -> Color {
        super::animated_color(position, tick, Theme::default())
    }
    use crate::agent::ApprovalRequest;
    use crate::agent::AskUserKind;
    use crate::app::{AgentPanelEntry, Document, DocumentKind, DocumentReturn};
    use crate::model::{LinkTarget, TodoItem, WikiLinkCandidate};
    use crate::storage::Storage;

    fn make_app() -> (App, tempfile::TempDir) {
        let directory = tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        (App::new(storage).unwrap(), directory)
    }

    fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let _ = draw(frame, app);
            })
            .unwrap();
        terminal
    }

    fn buffer_string(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        let mut output = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        output
    }

    #[test]
    fn rendering_uses_colors_loaded_from_the_app_theme() {
        let (mut app, _directory) = make_app();
        let custom = crate::theme::DEFAULT_THEME_TOML
            .replace("canvas = \"terminal\"", "canvas = \"#0a0b0c\"")
            .replace("panel = \"#181825\"", "panel = \"#010203\"")
            .replace("compose = \"#313244\"", "compose = \"#0d0e0f\"")
            .replace("status_bar = \"terminal\"", "status_bar = \"#101112\"")
            .replace(
                "status_context = \"#89b4fa\"",
                "status_context = \"#040506\"",
            )
            .replace("heading_1 = \"#b4befe\"", "heading_1 = \"#070809\"");
        app.theme = Theme::from_toml(&custom).unwrap();

        let terminal = render(&mut app, 220, 24);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 1)].bg, Color::Rgb(1, 2, 3));
        let center = app.layout.center.unwrap();
        assert_eq!(buffer[(center.x, 0)].bg, Color::Rgb(10, 11, 12));
        let compose = app.layout.compose.unwrap();
        assert_eq!(
            buffer[(compose.x + 1, compose.y + 1)].bg,
            Color::Rgb(13, 14, 15)
        );
        assert_eq!(buffer[(0, 23)].bg, Color::Rgb(4, 5, 6));
        assert_eq!(buffer[(219, 23)].bg, Color::Rgb(16, 17, 18));

        let markdown = crate::markdown::render_at_width("# Heading", 40, app.theme);
        assert!(markdown
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.fg == Some(Color::Rgb(7, 8, 9))));
    }

    #[test]
    fn agent_header_shows_rounds_session_usage_tps_and_cache() {
        assert_eq!(human_token_count(999), "999");
        assert_eq!(human_token_count(1_000), "1k");
        assert_eq!(human_token_count(12_400), "12.4k");
        assert_eq!(human_token_count(1_250_000), "1.2m");

        let (mut app, _directory) = make_app();
        app.agent_round = 3;
        app.agent_round_limit = 25;
        app.agent_usage = crate::agent::TokenUsage {
            input_tokens: 500,
            output_tokens: 1_234,
            cache_creation_input_tokens: 1_000,
            cache_read_input_tokens: 2_000,
        };
        app.agent_timed_output_tokens = 1_234;
        app.agent_response_duration = std::time::Duration::from_secs(2);
        let terminal = render(&mut app, 170, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Agent · ↻3/25"));
        assert!(screen.contains("↑3.5k ↓1.2k · 617.0 t/s · Cache 2k 57%"));
    }

    #[test]
    fn command_palette_is_fixed_width_and_renders_query_and_commands() {
        let (mut app, _directory) = make_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.handle_paste("agent");

        let terminal = render(&mut app, 120, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Command Palette · Ctrl+P"));
        assert!(screen.contains("/ agent"));
        assert!(screen.contains("Agent: Interrupt task"));
        assert!(screen.contains("Stop the active Agent task"));
        assert!(screen.contains("Agent: Clear session"));
        let palette = app.layout.overlay.unwrap();
        assert_eq!(palette.width, 80);
        assert_eq!(
            terminal.backend().buffer()[(palette.x, palette.y)].fg,
            ctp::GREEN
        );
        assert!(app.dialog_hitboxes.len() >= 4);
        let selected = &app.dialog_hitboxes[0].area;
        assert_eq!(selected.height, 3);
        assert_eq!(
            terminal.backend().buffer()[(selected.x, selected.y)].symbol(),
            "▌"
        );
        assert_eq!(
            terminal.backend().buffer()[(selected.x, selected.y + 1)].symbol(),
            "▌"
        );
        assert_eq!(
            terminal.backend().buffer()[(selected.x, selected.y + 2)].symbol(),
            "▌",
            "selection rail should fill the shared blank row"
        );
        assert_eq!(
            terminal.backend().buffer()[(selected.x, selected.y + 2)].bg,
            ctp::SURFACE_1,
            "selection should include the shared blank row"
        );
        assert!(terminal.backend().buffer()[(selected.x + 2, selected.y)]
            .modifier
            .contains(Modifier::BOLD));
        assert_eq!(
            terminal.backend().buffer()[(selected.x + 2, selected.y + 1)].fg,
            ctp::OVERLAY_0
        );
        let last = &app.dialog_hitboxes.last().unwrap().area;
        let gap_y = last.y + last.height;
        assert_eq!(
            gap_y,
            palette.y + palette.height - 3,
            "one blank row should separate commands from the footer"
        );
    }

    fn contains(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x.saturating_add(inner.width) <= outer.x.saturating_add(outer.width)
            && inner.y.saturating_add(inner.height) <= outer.y.saturating_add(outer.height)
    }

    #[test]
    fn narrow_center_surface_fills_body_while_content_axis_is_capped() {
        for width in [60, 80, 120, 169] {
            let (mut app, _directory) = make_app();
            app.focus = Focus::Center;
            let terminal = render(&mut app, width, 24);
            let center = app.layout.center.expect("center surface");
            assert_eq!(center, Rect::new(0, 0, width, 23), "width {width}");
            let content = center_content_axis(center);
            assert_eq!(content.width, width.min(CENTER_MAX_WIDTH), "width {width}");
            assert_eq!(
                content.x,
                width.saturating_sub(content.width) / 2,
                "width {width}"
            );
            assert!(app.layout.files.is_none());
            assert!(app.layout.todo.is_none());
            assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), " ");
            assert!(buffer_string(&terminal).contains("Daily"));
        }
    }

    #[test]
    fn wide_layout_uses_terminal_edges_and_center_content_axis() {
        for width in [170, 171, 220] {
            let (mut app, _directory) = make_app();
            app.focus = Focus::Center;
            render(&mut app, width, 24);
            let files = app.layout.files.unwrap();
            let center = app.layout.center.unwrap();
            let todo = app.layout.todo.unwrap();
            let agent = app.layout.agent.unwrap();
            assert_eq!(files, Rect::new(0, 0, FILES_WIDTH, 23), "width {width}");
            assert_eq!(todo.width, RIGHT_SIDEBAR_WIDTH, "width {width}");
            assert_eq!(todo.x + todo.width, width, "width {width}");
            assert_eq!(todo.height, 23u16.div_ceil(3), "width {width}");
            assert_eq!(agent.y, todo.y + todo.height, "width {width}");
            assert_eq!(agent.height, 23 - todo.height, "width {width}");
            let region_width = width - FILES_WIDTH - RIGHT_SIDEBAR_WIDTH;
            assert_eq!(center, Rect::new(FILES_WIDTH, 0, region_width, 23));
            let content = center_content_axis(center);
            assert_eq!(content.width, region_width.min(CENTER_MAX_WIDTH));
            assert_eq!(
                content.x,
                FILES_WIDTH + region_width.saturating_sub(content.width) / 2,
                "width {width}"
            );
        }
    }

    #[test]
    fn footer_uses_full_terminal_width() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.status = "saved-at-left".to_string();
        let terminal = render(&mut app, 220, 12);
        let buffer = terminal.backend().buffer();
        let footer: String = (0..220)
            .map(|x| buffer[(x, 11)].symbol().to_string())
            .collect();
        assert!(footer.starts_with(" DAILY "));
        assert!(footer.contains("saved-at-left"));
        assert!(footer.trim_end().ends_with("? help"));
    }

    #[test]
    fn running_agent_animates_its_border_and_current_activity_only() {
        let (mut app, _directory) = make_app();
        app.ai_running = true;
        app.agent_panel = vec![
            AgentPanelEntry::Prompt {
                text: "Analyze this".to_string(),
                muted: false,
            },
            AgentPanelEntry::Tool {
                text: "Completed Read File.".to_string(),
                active: false,
            },
            AgentPanelEntry::Tool {
                text: "Fetching Web...".to_string(),
                active: true,
            },
            AgentPanelEntry::Assistant {
                text: "I will compare **multiple sources**.".to_string(),
                streaming: false,
                final_output: false,
            },
            AgentPanelEntry::Prompt {
                text: "Consumed follow-up".to_string(),
                muted: false,
            },
            AgentPanelEntry::Prompt {
                text: "Queued follow-up".to_string(),
                muted: true,
            },
        ];
        app.status = "AI is working".to_string();
        app.animation_tick = 0;
        let first = render(&mut app, 170, 40);
        let agent = app.layout.agent.unwrap();
        let first_corner = first.backend().buffer()[(agent.x, agent.y)].fg;
        let top_colors = (agent.x..agent.x + agent.width)
            .map(|x| first.backend().buffer()[(x, agent.y)].fg)
            .collect::<Vec<_>>();
        assert!(top_colors
            .iter()
            .all(|color| matches!(color, Color::Rgb(..))));
        assert!(top_colors.windows(2).any(|colors| colors[0] != colors[1]));
        let first_footer = buffer_string(&first).lines().last().unwrap().to_string();
        let first_footer_colors = (0..170)
            .map(|x| first.backend().buffer()[(x, 39)].fg)
            .collect::<Vec<_>>();
        let first_activity_colors = (agent.y + 1..agent.y + agent.height - 1)
            .flat_map(|y| (agent.x + 1..agent.x + agent.width - 1).map(move |x| (x, y)))
            .map(|(x, y)| first.backend().buffer()[(x, y)].fg)
            .collect::<Vec<_>>();
        let first_screen = buffer_string(&first);
        let completed = first_screen.find("• Completed Read File.").unwrap();
        let active = first_screen.find("• Fetching Web...").unwrap();
        let intermediate = first_screen
            .find("I will compare multiple sources.")
            .unwrap();
        let consumed = first_screen.find("Consumed follow-up").unwrap();
        let queued = first_screen.find("Queued follow-up").unwrap();
        assert!(completed < active && active < intermediate && intermediate < consumed);
        assert!(consumed < queued);
        let screen_lines = first_screen.lines().collect::<Vec<_>>();
        let consumed_y = screen_lines
            .iter()
            .position(|line| line.contains("Consumed follow-up"))
            .unwrap() as u16;
        let queued_y = screen_lines
            .iter()
            .position(|line| line.contains("Queued follow-up"))
            .unwrap() as u16;
        let consumed_byte = screen_lines[consumed_y as usize]
            .find("Consumed follow-up")
            .unwrap();
        let queued_byte = screen_lines[queued_y as usize]
            .find("Queued follow-up")
            .unwrap();
        let consumed_x = screen_lines[consumed_y as usize][..consumed_byte].width() as u16;
        let queued_x = screen_lines[queued_y as usize][..queued_byte].width() as u16;
        assert_ne!(
            first.backend().buffer()[(consumed_x, consumed_y)].fg,
            ctp::OVERLAY_0
        );
        assert_eq!(
            first.backend().buffer()[(queued_x, queued_y)].fg,
            ctp::OVERLAY_0
        );

        app.animation_tick = 1;
        let second = render(&mut app, 170, 40);
        let second_corner = second.backend().buffer()[(agent.x, agent.y)].fg;
        let second_footer = buffer_string(&second).lines().last().unwrap().to_string();
        let second_footer_colors = (0..170)
            .map(|x| second.backend().buffer()[(x, 39)].fg)
            .collect::<Vec<_>>();
        let second_activity_colors = (agent.y + 1..agent.y + agent.height - 1)
            .flat_map(|y| (agent.x + 1..agent.x + agent.width - 1).map(move |x| (x, y)))
            .map(|(x, y)| second.backend().buffer()[(x, y)].fg)
            .collect::<Vec<_>>();
        assert_ne!(first_corner, second_corner);
        assert_eq!(first_footer, second_footer);
        assert_eq!(first_footer_colors, second_footer_colors);
        assert_ne!(first_activity_colors, second_activity_colors);

        app.ai_running = false;
        app.agent_panel.push(AgentPanelEntry::Assistant {
            text: "Final response".to_string(),
            streaming: false,
            final_output: true,
        });
        app.agent_scroll = u16::MAX;
        let final_frame = render(&mut app, 170, 40);
        let final_screen = buffer_string(&final_frame);
        assert!(final_screen.contains("User"));
        assert!(!final_screen.contains("Prompt"));
        assert!(!final_screen.contains("Response"));
        assert!(final_screen.contains("Final response"));
        for retained in [
            "Completed Read File.",
            "Fetching Web...",
            "multiple sources",
            "Consumed follow-up",
            "Queued follow-up",
        ] {
            assert!(final_screen.contains(retained));
        }
    }

    #[test]
    fn animated_activity_respects_terminal_cell_width() {
        let lines = animated_activity_lines("正在调用工具", 19, 4);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), " • 正在调用工具");
        assert_eq!(lines[0].width(), 15);
        assert_eq!(
            lines[0]
                .spans
                .iter()
                .skip(1)
                .filter(|span| span.style.fg != Some(ctp::OVERLAY_0))
                .count(),
            6
        );
        assert!(lines[0]
            .spans
            .iter()
            .filter(|span| span.style.fg != Some(ctp::OVERLAY_0))
            .all(|span| span.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn tool_activity_places_detail_on_a_connected_second_line() {
        let text = "Calling Read File...\ndata/a very long project filename.md";
        for lines in [
            activity_lines(text, 24),
            animated_activity_lines(text, 24, 4),
        ] {
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0].to_string(), " • Calling Read File...");
            assert_eq!(lines[1].width(), 24);
            assert!(lines[1].to_string().starts_with("   └─ "));
            assert!(lines[1].to_string().ends_with('…'));
        }

        assert_eq!(activity_lines("tool", 1)[0].width(), 1);
        assert_eq!(activity_lines("tool", 2)[0].width(), 2);
        assert_eq!(activity_lines("tool\ndetail", 1)[1].to_string(), "└");
        assert_eq!(activity_lines("tool\ndetail", 2)[1].to_string(), "└─");
    }

    #[test]
    fn bypass_mode_animates_in_the_footer_and_daily_advertises_commands() {
        let (mut app, _directory) = make_app();
        let approve = render(&mut app, 170, 24);
        let approve_screen = buffer_string(&approve);
        let approve_footer = approve_screen.lines().last().unwrap();
        assert!(!approve_footer.contains("DAILY · APPROVE"));
        assert!(approve_footer.contains("DAILY  APPROVE"));
        let surface_x = approve_footer.find("DAILY").unwrap() as u16;
        let approve_byte = approve_footer.find("APPROVE").unwrap();
        let approve_x = approve_footer[..approve_byte].width() as u16;
        assert_eq!(approve.backend().buffer()[(surface_x, 23)].bg, ctp::BLUE);
        assert_eq!(
            approve.backend().buffer()[(approve_x, 23)].bg,
            ctp::SAPPHIRE
        );

        app.permission_mode = PermissionMode::Bypass;
        app.animation_tick = 0;
        let first = render(&mut app, 170, 24);
        let footer_y = 23;
        let first_screen = buffer_string(&first);
        let first_footer = first_screen.lines().last().unwrap();
        let bypass_byte = first_footer.find("BYPASS").unwrap();
        let bypass_x = first_footer[..bypass_byte].width() as u16;
        let first_colors = (bypass_x..bypass_x + "BYPASS".width() as u16)
            .map(|x| first.backend().buffer()[(x, footer_y)].fg)
            .collect::<Vec<_>>();
        assert!(first_footer.contains("Ctrl+P commands"));
        assert!(first_colors
            .iter()
            .all(|color| matches!(color, Color::Rgb(..))));
        assert!((bypass_x..bypass_x + "BYPASS".width() as u16)
            .all(|x| first.backend().buffer()[(x, footer_y)].bg == ctp::CRUST));

        app.animation_tick = 1;
        let second = render(&mut app, 170, 24);
        let second_colors = (bypass_x..bypass_x + "BYPASS".width() as u16)
            .map(|x| second.backend().buffer()[(x, footer_y)].fg)
            .collect::<Vec<_>>();
        assert_ne!(first_colors, second_colors);
    }

    #[test]
    fn virtual_line_window_clones_only_visible_rows() {
        let lines = (0..10_000)
            .map(|index| Line::from(format!("row {index}")))
            .collect::<Vec<_>>();
        let visible = visible_line_window(&lines, 9_990, 5);

        assert_eq!(visible.len(), 5);
        assert_eq!(visible.first().unwrap().to_string(), "row 9990");
        assert_eq!(visible.last().unwrap().to_string(), "row 9994");
        assert!(visible_line_window(&lines, lines.len(), 5).is_empty());
    }

    #[test]
    fn narrow_files_and_todo_each_use_the_full_body_without_duplicates() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
        app.reload_files();
        app.focus = Focus::Files;
        let terminal = render(&mut app, 80, 18);
        assert_eq!(app.layout.files, Some(Rect::new(0, 0, 80, 17)));
        assert!(app.layout.center.is_none());
        assert!(app.layout.todo.is_none());
        assert_eq!(buffer_string(&terminal).matches("NoleBase").count(), 1);
        assert!(!app.file_hitboxes.is_empty());
        assert!(app
            .file_hitboxes
            .iter()
            .all(|hitbox| contains(app.layout.files.unwrap(), hitbox.area)));

        app.focus = Focus::Todo;
        app.todo_items = vec![TodoItem {
            checked: false,
            text: "buy milk".to_string(),
        }];
        let terminal = render(&mut app, 60, 18);
        assert_eq!(app.layout.todo, Some(Rect::new(0, 0, 60, 17)));
        assert!(app.layout.files.is_none());
        assert!(app.layout.center.is_none());
        let screen = buffer_string(&terminal);
        assert_eq!(screen.matches("Todo").count(), 1);
        assert!(screen.contains("buy milk"));
        assert_eq!(app.todo_hitboxes.len(), 1);
    }

    #[test]
    fn sidebars_use_mantle_background_with_square_ui_borders() {
        let (mut app, _directory) = make_app();
        let terminal = render(&mut app, 170, 24);
        let buffer = terminal.backend().buffer();
        let files = app.layout.files.expect("files panel");
        let todo = app.layout.todo.expect("todo panel");
        let agent = app.layout.agent.expect("agent panel");
        let center = app.layout.center.expect("center region");

        for area in [files, todo, agent] {
            assert_eq!(buffer[(area.x, area.y)].symbol(), "┌");
            assert_eq!(
                buffer[(area.x + 2, area.y + area.height - 2)].bg,
                ctp::MANTLE
            );
        }
        assert_eq!(buffer[(center.x, center.y)].bg, Color::Reset);
    }

    #[test]
    fn selected_file_background_covers_name_and_modified_time_rows() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
        app.reload_files();
        app.focus = Focus::Files;
        let modified: DateTime<Local> = app.note_files[app.file_index].modified.into();
        let expected_timestamp = modified.format("%y/%m/%d %H:%M").to_string();

        let terminal = render(&mut app, 170, 18);
        assert!(buffer_string(&terminal).contains(&expected_timestamp));
        let selected_path = app.note_files[app.file_index].path.clone();
        let selected_area = app
            .file_hitboxes
            .iter()
            .find(|hitbox| hitbox.path == selected_path)
            .expect("selected file hitbox")
            .area;
        assert_eq!(selected_area.height, 2);
        let buffer = terminal.backend().buffer();
        for y in selected_area.y..selected_area.y + selected_area.height {
            for x in selected_area.x..selected_area.x + selected_area.width {
                assert_eq!(buffer[(x, y)].bg, ctp::SURFACE_1);
            }
        }
        assert_eq!(
            buffer[(selected_area.x + 1, selected_area.y + 1)].fg,
            ctp::SUBTEXT_1,
            "modified time must remain legible on the selected background"
        );
    }

    #[test]
    fn file_selection_includes_both_shared_spacing_lines() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("First.md"), "first").unwrap();
        fs::write(app.storage.data_dir.join("Second.md"), "second").unwrap();
        app.reload_files();
        app.focus = Focus::Files;

        let terminal = render(&mut app, 170, 22);
        let mut files = app.file_hitboxes.clone();
        files.sort_by_key(|hitbox| hitbox.area.y);
        let notes = app
            .file_group_hitboxes
            .iter()
            .find(|hitbox| hitbox.group == FileGroup::Notes)
            .expect("Notes group")
            .area;
        let archives = app
            .file_group_hitboxes
            .iter()
            .find(|hitbox| hitbox.group == FileGroup::Archives)
            .expect("Archives group")
            .area;

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].area.y, notes.y + 2);
        assert_eq!(files[1].area.y, files[0].area.y + 3);
        assert_eq!(archives.y, files[1].area.y + 3);
        assert_eq!(files[0].area.height, 2);
        assert_eq!(files[1].area.height, 2);
        let buffer = terminal.backend().buffer();
        let selected = files
            .iter()
            .find(|hitbox| Some(&hitbox.path) == app.selected_file.as_ref())
            .expect("selected file");
        assert_eq!(
            buffer[(selected.area.x, selected.area.y.saturating_sub(1))].bg,
            ctp::SURFACE_1,
            "selected background must include the upper shared spacing"
        );
        assert_eq!(
            buffer[(selected.area.x, selected.area.y + 2)].bg,
            ctp::SURFACE_1,
            "selected background must include the lower shared spacing"
        );
        for rail_y in selected.area.y.saturating_sub(1)..=selected.area.y + 2 {
            assert_eq!(buffer[(selected.area.x, rail_y)].symbol(), "▌");
            assert_eq!(buffer[(selected.area.x, rail_y)].fg, ctp::MAUVE);
        }
        assert_ne!(buffer[(notes.x, notes.y)].symbol(), "▌");
        assert_ne!(buffer[(archives.x, archives.y)].symbol(), "▌");
    }

    #[test]
    fn file_groups_use_teal_markers_and_muted_counts() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
        app.reload_files();
        app.focus = Focus::Files;

        let terminal = render(&mut app, 170, 18);
        let notes = app
            .file_group_hitboxes
            .iter()
            .find(|hitbox| hitbox.group == FileGroup::Notes)
            .expect("Notes group hitbox")
            .area;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(notes.x, notes.y)].fg, ctp::TEAL);
        assert_eq!(
            buffer[(notes.x + notes.width - 1, notes.y)].fg,
            ctp::OVERLAY_0
        );
    }

    #[test]
    fn file_name_inputs_render_as_modals_while_search_stays_inline() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.files_context = FilesContext::NewTarget;
        app.new_file_input = "Project".to_string();
        app.new_file_cursor = app.new_file_input.chars().count();
        let terminal = render(&mut app, 80, 16);
        assert_eq!(app.layout.files, Some(Rect::new(0, 0, 80, 15)));
        assert!(app.layout.center.is_none());
        let screen = buffer_string(&terminal);
        assert!(screen.contains("New file · Enter create"));
        assert!(screen.contains("Name  Project"));
        assert!(app.layout.overlay.is_some());
        assert!(app.file_hitboxes.is_empty());
        let modal = app.layout.overlay.unwrap();
        assert_eq!(
            terminal.backend().buffer()[(modal.x + 1, modal.y + 1)].bg,
            ctp::MANTLE,
            "modal padding should have an opaque background"
        );

        app.files_context = FilesContext::Rename;
        app.rename_input = "Renamed".to_string();
        app.rename_cursor = app.rename_input.chars().count();
        let terminal = render(&mut app, 80, 16);
        assert!(buffer_string(&terminal).contains("Name  Renamed"));
        assert!(app.layout.overlay.is_some());

        app.files_context = FilesContext::Search;
        app.file_query = "work".to_string();
        let terminal = render(&mut app, 80, 16);
        assert!(buffer_string(&terminal).contains("/ work"));
        let files = app.layout.files.unwrap();
        let underline = &terminal.backend().buffer()[(files.x + 2, files.y + 2)];
        assert_eq!(underline.symbol(), "─");
        assert_eq!(underline.fg, ctp::OVERLAY_0);
        assert!(app.layout.overlay.is_none());
    }

    #[test]
    fn narrow_center_renders_each_center_view_in_place() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.center_view = CenterView::Document;
        app.document = Some(Document {
            kind: DocumentKind::Daily(NaiveDate::from_ymd_opt(2026, 7, 27).unwrap()),
            title: "Preview".to_string(),
            source: "# Heading".to_string(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });
        let terminal = render(&mut app, 80, 18);
        assert!(buffer_string(&terminal).contains("Heading"));

        app.center_view = CenterView::Search;
        app.search_query = "needle".to_string();
        app.search_results = vec![SearchHit::Daily {
            date: NaiveDate::from_ymd_opt(2026, 7, 27).unwrap(),
            text: "needle result".to_string(),
        }];
        let terminal = render(&mut app, 80, 18);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Searcher · 1"));
        assert!(screen.contains("needle result"));
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), " ");
        assert_ne!(
            terminal.backend().buffer()[(4, 0)].symbol(),
            " ",
            "only the centered searcher should have a border"
        );
        assert_eq!(app.search_hitboxes.len(), 1);
    }

    #[test]
    fn chat_compose_and_button_hitboxes_stay_inside_visible_center_viewport() {
        for width in [60, 80, 120, 169, 170, 171, 220] {
            let (mut app, _directory) = make_app();
            for index in 0..30 {
                app.storage
                    .append_to_today(&format!("message {index}"))
                    .unwrap();
            }
            app.reload();
            app.selected = app.daily_notes.len() - 1;
            app.focus = Focus::Center;
            app.scroll = u16::MAX;
            render(&mut app, width, 24);
            let center = app.layout.center.unwrap();
            let compose = app.layout.compose.unwrap();
            assert!(compose.width <= CENTER_MAX_WIDTH, "width {width}");
            assert!(contains(center, compose), "width {width}");
            assert!(!app.hitboxes.is_empty(), "width {width}");
            for hitbox in &app.hitboxes {
                assert!(contains(center, hitbox.area), "width {width}");
                assert!(
                    hitbox.area.y < compose.y.saturating_sub(1),
                    "button behind compose at width {width}: {:?}",
                    hitbox.area
                );
            }
        }
    }

    #[test]
    fn overlay_records_geometry_and_disables_all_background_hitboxes() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
        app.reload_files();
        app.storage.append_to_today("hello").unwrap();
        app.reload();
        app.todo_items = vec![TodoItem {
            checked: false,
            text: "task".to_string(),
        }];
        app.focus = Focus::Center;
        app.set_overlay(Overlay::Help);
        render(&mut app, 220, 24);
        assert!(app.layout.overlay.is_some());
        assert!(app.hitboxes.is_empty());
        assert!(app.link_hitboxes.is_empty());
        assert!(app.file_hitboxes.is_empty());
        assert!(app.todo_hitboxes.is_empty());
        assert!(app.search_hitboxes.is_empty());
    }

    #[test]
    fn links_are_clickable_in_daily_documents_and_agent_output() {
        let (mut app, _directory) = make_app();
        app.storage
            .append_to_today("Open [site](https://example.test)")
            .unwrap();
        app.reload();
        render(&mut app, 170, 24);
        assert!(app.link_hitboxes.iter().any(|hitbox| {
            hitbox.target == LinkTarget::External("https://example.test".to_string())
        }));

        app.document = Some(Document {
            kind: DocumentKind::Daily(NaiveDate::from_ymd_opt(2026, 7, 27).unwrap()),
            title: "Preview".to_string(),
            source: "See [[Project]]".to_string(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });
        app.center_view = CenterView::Document;
        render(&mut app, 170, 24);
        assert!(app
            .link_hitboxes
            .iter()
            .any(|hitbox| { hitbox.target == LinkTarget::WikiLink("Project".to_string()) }));

        app.agent_panel = vec![AgentPanelEntry::Assistant {
            text: "[result](https://agent.example)".to_string(),
            streaming: false,
            final_output: true,
        }];
        render(&mut app, 170, 24);
        assert!(app.link_hitboxes.iter().any(|hitbox| {
            hitbox.target == LinkTarget::External("https://agent.example".to_string())
        }));
    }

    #[test]
    fn wikilink_choice_marks_archive_and_file_format_as_muted_metadata() {
        let (mut app, directory) = make_app();
        app.wiki_link_target = Some("Project".to_string());
        app.wiki_link_candidates = vec![
            WikiLinkCandidate {
                path: directory.path().join("data/Project.md"),
                archived: false,
            },
            WikiLinkCandidate {
                path: directory.path().join("archives/Project.mb"),
                archived: true,
            },
        ];
        app.set_overlay(Overlay::WikiLinkChoice);
        let terminal = render(&mut app, 100, 18);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Project.md"));
        assert!(screen.contains("MD"));
        assert!(screen.contains("Archived"));
        assert!(screen.contains("MB"));
        assert_eq!(app.wiki_link_hitboxes.len(), 2);
    }

    #[test]
    fn agent_prompt_and_diff_approval_render_as_opaque_overlays() {
        let (mut app, _directory) = make_app();
        app.ai_prompt_input = "summarize this".to_string();
        app.ai_prompt_cursor = app.ai_prompt_input.chars().count();
        app.set_overlay(Overlay::AiPrompt);
        let terminal = render(&mut app, 100, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Agent prompt"));
        assert!(screen.contains("summarize this"));

        app.approval_request = Some(ApprovalRequest {
            title: "Update data/note.md".to_string(),
            diff: "--- old\n+++ new\n@@ -1 +1 @@\n-old value\n+new value\n".to_string(),
        });
        app.set_overlay(Overlay::Approval);
        let terminal = render(&mut app, 100, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Update data/note.md"));
        assert!(screen.contains("-old value"));
        assert!(screen.contains("+new value"));
        assert!(screen.contains("Tab bypass"));
    }

    #[test]
    fn ask_user_overlay_renders_choices_and_free_text_input() {
        let (mut app, _directory) = make_app();
        app.ask_user_request = Some(crate::agent::AskUserRequest {
            kind: AskUserKind::Tool,
            question: "Which output format should be used?".to_string(),
            options: vec!["Markdown".to_string(), "MBDown".to_string()],
        });
        app.ask_user_option = 1;
        app.set_overlay(Overlay::AskUser);
        let terminal = render(&mut app, 100, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Agent question"));
        assert!(screen.contains("Which output format should be used?"));
        assert!(screen.contains("Markdown"));
        assert!(screen.contains("MBDown"));
        assert!(screen.contains("Other answer"));
        assert!(screen.contains("Your answer"));
        let overlay = app.layout.overlay.expect("ask-user overlay");
        assert_eq!(overlay.width, DIALOG_WIDTH);
        assert_eq!(overlay.height, 11);
        assert!(app.hitboxes.is_empty());
    }

    #[test]
    fn round_limit_dialog_only_offers_continue_or_stop() {
        let (mut app, _directory) = make_app();
        app.ask_user_request = Some(crate::agent::AskUserRequest {
            kind: AskUserKind::RoundLimit,
            question: "Continue for up to 25 more rounds?".to_string(),
            options: vec!["Continue".to_string(), "Stop".to_string()],
        });
        app.set_overlay(Overlay::AskUser);

        let terminal = render(&mut app, 100, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Agent round limit"));
        assert!(screen.contains("Continue"));
        assert!(screen.contains("Stop"));
        assert!(!screen.contains("Other answer"));
        assert!(!screen.contains("Your answer"));
        assert!(screen.contains("Esc stop"));
    }

    #[test]
    fn agent_panel_shows_user_before_agent_with_source_backgrounds() {
        let (mut app, _directory) = make_app();
        app.agent_panel = vec![
            AgentPanelEntry::Prompt {
                text: "Explain the selected note".to_string(),
                muted: false,
            },
            AgentPanelEntry::Assistant {
                text: "Here is the explanation".to_string(),
                streaming: false,
                final_output: true,
            },
        ];
        app.focus = Focus::Agent;

        let terminal = render(&mut app, 170, 24);
        let screen = buffer_string(&terminal);
        let prompt = screen.find("Explain the selected note").unwrap();
        let response = screen.find("Here is the explanation").unwrap();
        assert!(prompt < response);

        let (user_lines, _, _) = render_agent_entry(&app.agent_panel[0], 40, 0, false);
        let (agent_lines, _, _) = render_agent_entry(&app.agent_panel[1], 40, 0, false);
        assert_eq!(user_lines[1].to_string().trim_end(), "User");
        assert_eq!(agent_lines[1].to_string().trim_end(), "Agent");
        assert!(user_lines.first().unwrap().to_string().trim().is_empty());
        assert!(user_lines.last().unwrap().to_string().trim().is_empty());
        assert!(agent_lines.first().unwrap().to_string().trim().is_empty());
        assert!(agent_lines.last().unwrap().to_string().trim().is_empty());
        assert!(user_lines.iter().all(|line| {
            UnicodeWidthStr::width(line.to_string().as_str()) == 40
                && line
                    .spans
                    .iter()
                    .all(|span| span.style.bg == Some(ctp::SURFACE_0))
        }));
        assert!(agent_lines.iter().all(|line| {
            UnicodeWidthStr::width(line.to_string().as_str()) == 40
                && line
                    .spans
                    .iter()
                    .all(|span| span.style.bg == Some(ctp::BASE))
        }));

        let agent_area = app.layout.agent.unwrap();
        let rows = screen.lines().collect::<Vec<_>>();
        let user_text_row = rows
            .iter()
            .position(|line| line.contains("Explain the selected note"))
            .unwrap();
        let agent_text_row = rows
            .iter()
            .position(|line| line.contains("Here is the explanation"))
            .unwrap();
        assert_eq!(agent_text_row - user_text_row, 4);
        for (needle, background) in [
            ("Explain the selected note", ctp::SURFACE_0),
            ("Here is the explanation", ctp::BASE),
        ] {
            let y = rows.iter().position(|line| line.contains(needle)).unwrap() as u16;
            for x in agent_area.x + 1..agent_area.x + agent_area.width - 1 {
                assert_eq!(terminal.backend().buffer()[(x, y)].bg, background);
            }
        }
    }

    #[test]
    fn daily_and_agent_render_caches_offset_markdown_images() {
        let date = NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();
        let daily = crate::model::DailyNote {
            date,
            body: "![Daily diagram](../data/diagram.png)".to_string(),
        };
        let cached = render_daily_note(&daily, "2026-07-27".to_string(), 100);
        assert_eq!(cached.images.len(), 1);
        assert!(cached.images[0].row >= 4);
        assert!(cached.images[0].column > 0);
        assert_eq!(cached.images[0].height, 12);

        let entry = AgentPanelEntry::Assistant {
            text: "![Agent diagram](https://example.com/diagram.png)".to_string(),
            streaming: false,
            final_output: true,
        };
        let (lines, _, images) = render_agent_entry(&entry, 40, 0, false);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].row, 2);
        assert_eq!(images[0].width, 40);
        assert_eq!(lines.len(), 15);
    }

    #[test]
    fn document_keeps_compose_visible_and_notification_uses_top_right() {
        let (mut app, _directory) = make_app();
        app.center_view = CenterView::Document;
        app.focus = Focus::Center;
        app.document = Some(Document {
            kind: DocumentKind::File(app.storage.archives_dir.join("2026-07-27.md")),
            title: "Article".to_string(),
            source: "# Reading\n\nUseful paragraph".to_string(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });
        app.notifications.notify("Recorded in Daily");
        let terminal = render(&mut app, 120, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Reading"));
        assert!(screen.contains("Compose"));
        assert!(screen.contains("Notification"));
        assert!(screen.contains("Recorded in Daily"));
        let compose = app.layout.compose.expect("document compose");
        assert!(compose.y > 12);
    }

    #[test]
    fn focused_compose_floats_over_the_document_with_an_animated_border() {
        let (mut app, _directory) = make_app();
        app.center_view = CenterView::Document;
        app.focus = Focus::Compose;
        app.animation_tick = 3;
        app.document = Some(Document {
            kind: DocumentKind::File(app.storage.data_dir.join("Article.md")),
            title: "Article".to_string(),
            source: (0..40)
                .map(|line| format!("paragraph {line}"))
                .collect::<Vec<_>>()
                .join("\n\n"),
            scroll: u16::MAX,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });

        let terminal = render(&mut app, 120, 30);
        let compose = app.layout.compose.expect("document compose");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(compose.x, compose.y)].symbol(), "┌");
        assert_eq!(
            buffer[(compose.x, compose.y)].fg,
            animated_color(0, app.animation_tick)
        );
        assert_eq!(
            buffer[(compose.x + 1, compose.y)].fg,
            animated_color(1, app.animation_tick)
        );

        let center = app.layout.center.expect("center area");
        let content = inset_horizontal(center_content_axis(center), 2);
        assert!(compose.x > content.x);
        assert_eq!(buffer[(content.x, compose.y + 1)].bg, ctp::MANTLE);
        assert_eq!(buffer[(compose.x + 1, compose.y + 1)].bg, ctp::SURFACE_0);
        let last_paragraph_y = (0..buffer.area().height)
            .find(|y| {
                (0..buffer.area().width)
                    .map(|x| buffer[(x, *y)].symbol())
                    .collect::<String>()
                    .contains("paragraph 39")
            })
            .expect("the final paragraph should remain visible above Compose");
        assert!(last_paragraph_y < compose.y.saturating_sub(1));
    }

    #[test]
    fn tiny_terminals_and_requested_widths_do_not_panic() {
        for (width, height) in [
            (1, 1),
            (2, 2),
            (5, 3),
            (20, 4),
            (60, 8),
            (80, 8),
            (120, 8),
            (169, 8),
            (170, 8),
            (171, 8),
            (220, 8),
        ] {
            let (mut app, _directory) = make_app();
            app.input = "wide 字\nsecond line".to_string();
            app.input_cursor = app.input.chars().count();
            render(&mut app, width, height);
        }
    }

    #[test]
    fn multiline_chat_and_compose_content_render() {
        let (mut app, _directory) = make_app();
        app.storage.append_to_today("alpha\nbeta **bold**").unwrap();
        app.reload();
        app.focus = Focus::Compose;
        app.input = "first\nsecond".to_string();
        app.input_cursor = app.input.chars().count();
        let terminal = render(&mut app, 120, 24);
        let screen = buffer_string(&terminal);
        for expected in ["alpha", "beta bold", "first", "second"] {
            assert!(screen.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn todo_items_wrap_and_keep_the_whole_item_clickable() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Todo;
        app.todo_items = vec![TodoItem {
            checked: false,
            text: "a todo item whose content is deliberately longer than the panel".to_string(),
        }];
        let terminal = render(&mut app, 170, 18);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("a todo item whose content"));
        assert!(screen.contains("longer"));
        assert!(screen.contains("panel"));
        assert_eq!(app.todo_hitboxes.len(), 1);
        assert!(app.todo_hitboxes[0].area.height > 1);
    }

    #[test]
    fn todo_items_share_a_blank_row_included_in_selection_and_hitbox() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Todo;
        app.todo_items = vec![
            TodoItem {
                checked: false,
                text: "first task".to_string(),
            },
            TodoItem {
                checked: true,
                text: "second task".to_string(),
            },
        ];
        app.todo_index = 1;

        let terminal = render(&mut app, 170, 24);
        let screen = buffer_string(&terminal);
        let lines = screen.lines().collect::<Vec<_>>();
        let first_row = lines
            .iter()
            .position(|line| line.contains("first task"))
            .unwrap();
        let second_row = lines
            .iter()
            .position(|line| line.contains("second task"))
            .unwrap();
        assert_eq!(second_row, first_row + 2);
        assert_eq!(app.todo_hitboxes.len(), 2);
        assert_eq!(app.todo_hitboxes[0].area.height, 2);
        let todo = app.layout.todo.unwrap();
        let first = app.todo_hitboxes[0].area;
        let last = app.todo_hitboxes[1].area;
        assert_eq!(first.y, todo.y + 2, "the first item needs a top margin");
        assert_eq!(
            terminal.backend().buffer()[(first.x, first.y + first.height - 1)].bg,
            ctp::SURFACE_1,
            "the selected background should include the shared blank row"
        );
        let last_margin = &terminal.backend().buffer()[(last.x, last.y + last.height - 1)];
        assert_eq!(last_margin.symbol(), " ");
        assert_eq!(last_margin.bg, ctp::SURFACE_1);
        assert!(!last_margin.modifier.contains(Modifier::CROSSED_OUT));
        assert!(!terminal.backend().buffer()[(last.x, last.y - 1)]
            .modifier
            .contains(Modifier::CROSSED_OUT));
        assert!((last.x..last.x + last.width).any(|x| {
            let cell = &terminal.backend().buffer()[(x, last.y)];
            cell.modifier.contains(Modifier::CROSSED_OUT) && cell.symbol() != " "
        }));
    }

    #[test]
    fn todo_display_groups_open_items_before_completed_items() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Todo;
        app.todo_items = vec![
            TodoItem {
                checked: true,
                text: "finished task".to_string(),
            },
            TodoItem {
                checked: false,
                text: "open task".to_string(),
            },
        ];
        app.todo_index = 1;
        let terminal = render(&mut app, 170, 18);
        let screen = buffer_string(&terminal);
        assert!(screen.find("open task") < screen.find("finished task"));
        assert_eq!(
            app.todo_hitboxes
                .iter()
                .map(|hitbox| hitbox.index)
                .collect::<Vec<_>>(),
            vec![1, 0],
            "hitboxes must retain daily task source indices"
        );
    }

    #[test]
    fn chat_renders_block_markdown_on_colored_cards() {
        let (mut app, _directory) = make_app();
        app.storage
            .append_to_today(concat!(
                "# Heading\n\n- first\n- second\n\n`code`\n\n",
                "[columns gap=2]\n",
                "[column]Left[/column]\n",
                "[column]Right[/column]\n",
                "[/columns]\n\n",
                "[bg=196]colored[/bg]"
            ))
            .unwrap();
        app.reload();
        let date = app.daily_notes[0].date.format("%Y-%m-%d").to_string();
        let terminal = render(&mut app, 170, 40);
        let screen = buffer_string(&terminal);
        let screen_lines = screen.lines().collect::<Vec<_>>();
        let date_row = screen_lines
            .iter()
            .position(|line| line.contains(&date))
            .expect("missing DailyNote date");
        let heading_row = screen_lines
            .iter()
            .position(|line| line.contains("Heading"))
            .expect("missing body heading");
        assert_eq!(
            heading_row,
            date_row + 2,
            "date and body need one blank row"
        );
        assert!(
            screen_lines[heading_row].find("Heading").unwrap()
                >= screen_lines[date_row].find(&date).unwrap(),
            "date and body should use the same centered content axis"
        );
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| {
            cell.symbol() == date.chars().next().unwrap().to_string()
                && cell.modifier.contains(Modifier::BOLD)
                && cell.modifier.contains(Modifier::UNDERLINED)
        }));
        for expected in ["Heading", "• first", "• second", "code"] {
            assert!(screen.contains(expected), "missing {expected}");
        }
        assert!(screen
            .lines()
            .any(|line| line.contains("Left") && line.contains("Right")));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.symbol() == "c" && cell.bg == Color::Indexed(196)));
        assert!(!screen.contains("[view]"));
        assert!(
            app.hitboxes
                .iter()
                .all(|hitbox| hitbox.action != Action::View),
            "DailyNotes no longer need a preview button"
        );
        assert!(
            buffer.content().iter().any(|cell| {
                cell.symbol() == "H"
                    && cell.modifier.contains(Modifier::BOLD)
                    && cell.bg == ctp::MANTLE
            }),
            "selection should not alter the Markdown body background"
        );
        let ai = app
            .hitboxes
            .iter()
            .find(|hitbox| hitbox.action == Action::Ai)
            .expect("AI button");
        let center = app.layout.center.expect("center");
        let daily_area = inset_horizontal(center_content_axis(center), 2);
        assert_eq!(
            ai.area.x + ai.area.width,
            daily_area.x + daily_area.width - PAGE_PADDING_X as u16,
            "AI button should share the body content axis"
        );
    }

    #[test]
    fn selected_daily_card_uses_an_animated_gradient_border() {
        let (mut app, _directory) = make_app();
        app.storage.append_to_today("A daily note").unwrap();
        app.reload();
        app.animation_tick = 0;

        let first = render(&mut app, 170, 30);
        let center = app.layout.center.unwrap();
        let daily = inset_horizontal(center_content_axis(center), 2);
        let top = daily.y + 2;
        let first_buffer = first.backend().buffer();
        assert_eq!(first_buffer[(daily.x, top)].symbol(), "┌");
        assert_eq!(first_buffer[(daily.x, top)].fg, animated_color(0, 0));
        assert_eq!(first_buffer[(daily.x + 1, top)].fg, animated_color(1, 0));
        assert_ne!(
            first_buffer[(daily.x, top)].fg,
            first_buffer[(daily.x + 1, top)].fg
        );

        app.animation_tick = 1;
        let second = render(&mut app, 170, 30);
        assert_eq!(
            second.backend().buffer()[(daily.x, top)].fg,
            animated_color(0, 1)
        );
        assert_ne!(
            second.backend().buffer()[(daily.x, top)].fg,
            first_buffer[(daily.x, top)].fg
        );
    }

    #[test]
    fn daily_body_axis_has_symmetric_gutters() {
        let width = 100;
        let metadata_and_gap = DAILY_PADDING_X + UnicodeWidthStr::width("2026-07-27") + 2;
        let (start, body_width) = centered_daily_body_axis(width, metadata_and_gap);
        let trailing = width - start - body_width;

        assert_eq!(start, metadata_and_gap);
        assert_eq!(trailing, start);
        assert_eq!(body_width, 74);
        assert_eq!(PAGE_PADDING_X, metadata_and_gap);
    }

    #[test]
    fn oversized_selected_card_keeps_a_stable_scroll_position() {
        let (mut app, _directory) = make_app();
        let body = (0..80)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.storage.append_to_today(&body).unwrap();
        app.reload();
        app.selected = app.daily_notes.len() - 1;
        app.scroll = u16::MAX;
        app.focus = Focus::Center;

        let first = render(&mut app, 80, 24);
        let first_scroll = app.scroll;
        let first_screen = buffer_string(&first);
        assert!(first_scroll > 0);

        let second = render(&mut app, 80, 24);
        assert_eq!(app.scroll, first_scroll);
        assert_eq!(buffer_string(&second), first_screen);
    }

    #[test]
    fn oversized_card_scroll_can_rest_anywhere_inside_the_card() {
        assert_eq!(stable_card_scroll(10, 10, 100, 20), 10);
        assert_eq!(stable_card_scroll(50, 10, 100, 20), 50);
        assert_eq!(stable_card_scroll(100, 10, 100, 20), 81);
        assert_eq!(stable_card_scroll(81, 10, 100, 20), 81);
    }

    #[test]
    fn manual_scroll_can_cross_an_oversized_selected_card_boundary() {
        let (mut app, _directory) = make_app();
        app.storage
            .append_daily("2026-07-26", "previous day")
            .unwrap();
        let long_body = (0..80)
            .map(|line| format!("long line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.storage.append_daily("2026-07-27", &long_body).unwrap();
        app.reload();
        app.selected = 1;
        app.scroll = u16::MAX;
        app.reveal_selected_daily = true;
        render(&mut app, 80, 24);
        assert!(app.scroll > 0);

        app.scroll = 0;
        app.reveal_selected_daily = false;
        let terminal = render(&mut app, 80, 24);
        assert_eq!(app.scroll, 0);
        assert!(buffer_string(&terminal).contains("previous day"));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn single_line_daily_card_spaces_date_body_and_buttons() {
        let (mut app, _directory) = make_app();
        app.storage.append_to_today("one line").unwrap();
        app.reload();
        let date = app.daily_notes[0].date.format(DATE_FMT).to_string();

        let terminal = render(&mut app, 170, 30);
        let screen = buffer_string(&terminal);
        let rows = screen.lines().collect::<Vec<_>>();
        let date_row = rows
            .iter()
            .position(|line| line.contains(&date))
            .expect("date row");
        assert!(rows[date_row + 2].contains("one line"));
        let button_row = app
            .hitboxes
            .iter()
            .find(|hitbox| hitbox.action == Action::Delete)
            .expect("delete button")
            .area
            .y as usize;
        assert_eq!(button_row, date_row + 4);
        let buffer = terminal.backend().buffer();
        let center = app.layout.center.expect("center area");
        let sample_x = center.x + center.width / 2;
        assert!(date_row >= 2);
        assert_eq!(buffer[(sample_x, date_row as u16 - 1)].bg, ctp::MANTLE);
        assert_eq!(buffer[(sample_x, date_row as u16 - 2)].bg, ctp::MANTLE);
        assert_eq!(buffer[(sample_x, button_row as u16 + 1)].bg, ctp::MANTLE);
        assert_eq!(buffer[(sample_x, button_row as u16 + 2)].bg, ctp::MANTLE);
        let card_left = inset_horizontal(center_content_axis(center), 2).x;
        assert_eq!(buffer[(card_left, date_row as u16 - 2)].symbol(), "┌");
        assert_eq!(buffer[(card_left, date_row as u16)].symbol(), "│");
        assert_eq!(buffer[(card_left, button_row as u16 + 2)].symbol(), "└");
    }

    #[test]
    fn document_view_uses_a_padded_page_background_without_an_outer_border() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.center_view = CenterView::Document;
        app.document = Some(Document {
            kind: DocumentKind::File(app.storage.archives_dir.join("2026-07-27.md")),
            title: "Archive".to_string(),
            source: "# Heading\n\nintro\n\nneedle".to_string(),
            scroll: 0,
            target_line: Some(5),
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });
        let terminal = render(&mut app, 80, 30);
        let buffer = terminal.backend().buffer();
        let header: String = (0..80)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        assert!(header.contains("Archive"));
        assert!(!header.contains("Esc back"));
        assert_eq!(buffer[(0, 0)].symbol(), " ");
        assert!(buffer_string(&terminal).contains("Compose"));
        assert!(buffer_string(&terminal).contains("  Archive"));
        assert_eq!(app.document.as_ref().unwrap().scroll, 0);
        assert_eq!(app.document.as_ref().unwrap().target_line, None);
        let first_document_row: String = (0..80)
            .map(|x| buffer[(x, 4)].symbol().to_string())
            .collect();
        assert!(first_document_row.contains("Heading"));
        assert_eq!(buffer[(2, 2)].bg, ctp::MANTLE);
        assert_eq!(buffer[(2, 3)].bg, ctp::MANTLE);
        let heading_x = first_document_row.find("Heading").unwrap() as u16;
        assert!(heading_x >= 2 + PAGE_PADDING_X as u16);
        assert!(buffer_string(&terminal).contains("needle"));
    }

    #[test]
    fn document_scroll_overwrites_box_borders_from_the_previous_frame() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.center_view = CenterView::Document;
        let boxed = (0..40)
            .map(|line| format!("boxed {line} ☀️"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let plain = (0..40)
            .map(|line| format!("plain {line}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        app.document = Some(Document {
            kind: DocumentKind::File(app.storage.data_dir.join("scroll.md")),
            title: "Scroll".to_string(),
            source: format!(
                "{plain}\n\n[box width=full border=single border-color=#df7f3f bg=16]\n{boxed}\n[/box]"
            ),
            scroll: u16::MAX,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });

        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        {
            let completed = terminal
                .draw(|frame| {
                    let _ = draw(frame, &mut app);
                })
                .unwrap();
            let box_buffer = completed.buffer;
            let mut saw_vs16 = false;
            for y in 0..box_buffer.area.height {
                for x in 0..box_buffer.area.width.saturating_sub(1) {
                    if box_buffer[(x, y)].symbol().contains('\u{fe0f}') {
                        saw_vs16 = true;
                        assert_eq!(box_buffer[(x + 1, y)].diff_option, CellDiffOption::Skip);
                    }
                }
            }
            assert!(saw_vs16);
        }
        assert!(terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() == "│"));

        app.document.as_mut().unwrap().scroll = 0;
        terminal
            .draw(|frame| {
                let _ = draw(frame, &mut app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let page = app.layout.center.unwrap();
        let border = Color::Rgb(223, 127, 63);
        assert!(buffer_string(&terminal).contains("plain 0"));
        assert!((page.y..page.y + page.height).all(|y| {
            (page.x..page.x + page.width)
                .all(|x| buffer[(x, y)].symbol() != "│" || buffer[(x, y)].fg != border)
        }));
    }

    #[test]
    fn document_code_block_background_has_no_wrapped_gaps() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.center_view = CenterView::Document;
        app.document = Some(Document {
            kind: DocumentKind::File(app.storage.archives_dir.join("2026-07-27.md")),
            title: "Code".to_string(),
            source: "```rust\nfn main() {\n    println!(\"hello\");\n}\n```".to_string(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });

        let terminal = render(&mut app, 80, 30);
        let buffer = terminal.backend().buffer();
        let background = mbtui::Theme::default()
            .code_block
            .bg
            .expect("the default code block theme has a background");
        let compose = app.layout.compose.expect("document compose");
        let rows = (0..compose.y)
            .filter(|y| (0..buffer.area().width).any(|x| buffer[(x, *y)].bg == background))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 7);
        assert!(rows.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }

    #[test]
    fn daily_vlist_only_renders_visible_cards_and_invalidates_changed_content() {
        let (mut app, _directory) = make_app();
        let first_date = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        app.daily_notes = (0..40)
            .map(|day| crate::model::DailyNote {
                date: first_date + chrono::Days::new(day),
                body: format!("# DailyNote {day}\n\nA DailyNote that may be off screen."),
            })
            .collect();

        sync_daily_vlist(&mut app, 80);
        assert!(app
            .daily_vlist
            .items
            .iter()
            .all(|item| item.cache.is_none()));
        let scroll = measure_visible_daily_cards(&mut app, 0, 8, 8, false);
        assert_eq!(scroll, 0);
        let rendered = app
            .daily_vlist
            .items
            .iter()
            .filter(|item| item.cache.is_some())
            .count();
        assert!(rendered > 0 && rendered < app.daily_notes.len());
        assert!(app.daily_vlist.items[20].cache.is_none());
        let original = app.daily_vlist.items[0].cache.clone();

        measure_visible_daily_cards(&mut app, 0, 8, 8, false);
        assert_eq!(app.daily_vlist.items[0].cache, original);

        app.daily_notes[0].body.push_str("\n\nChanged");
        sync_daily_vlist(&mut app, 80);
        assert!(app.daily_vlist.items[0].cache.is_none());
        assert!(!app.daily_vlist.geometry.is_measured(0));

        sync_daily_vlist(&mut app, 72);
        assert!(app
            .daily_vlist
            .items
            .iter()
            .all(|item| item.cache.is_none()));
        assert_eq!(app.daily_vlist.width, 72);
    }

    #[test]
    fn agent_vlist_only_renders_visible_entries_and_keeps_animation_out_of_cache() {
        let (mut app, _directory) = make_app();
        app.agent_panel.push(AgentPanelEntry::Tool {
            text: "Fetching Web...".to_string(),
            active: true,
        });
        app.agent_panel
            .extend((1..40).map(|index| AgentPanelEntry::Prompt {
                text: format!("Prompt {index}"),
                muted: false,
            }));

        sync_agent_vlist(&mut app, 40);
        assert!(app.agent_vlist.caches.iter().all(Option::is_none));
        let scroll = measure_visible_agent_entries(&mut app, 0, 6, false);
        assert_eq!(scroll, 0);
        let rendered = app
            .agent_vlist
            .caches
            .iter()
            .filter(|cache| cache.is_some())
            .count();
        assert!(rendered > 0 && rendered < app.agent_panel.len());
        assert!(app.agent_vlist.caches[20].is_none());
        let original = app.agent_vlist.caches[0].clone();
        let (visible, _, _) = visible_agent_lines(&mut app, 0, 6);
        assert_eq!(visible.len(), 6);

        app.animation_tick = 10;
        sync_agent_vlist(&mut app, 40);
        assert_eq!(app.agent_vlist.caches[0], original);

        if let AgentPanelEntry::Tool { text, .. } = &mut app.agent_panel[0] {
            text.push_str(" now");
        }
        sync_agent_vlist(&mut app, 40);
        assert!(app.agent_vlist.caches[0].is_none());
        assert!(!app.agent_vlist.geometry.is_measured(0));
    }
}
