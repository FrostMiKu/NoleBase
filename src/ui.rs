//! Terminal rendering for the full-width workspace.

use chrono::{DateTime, Local};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{block::Padding, Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, CenterView, FilesContext, Focus, LayoutSnapshot, Overlay};
use crate::model::{Action, ButtonHitbox, FileHitbox, SearchHit, SearchHitbox, TodoHitbox};

const DATE_FMT: &str = "%Y-%m-%d";
const TIME_FMT: &str = "%H:%M";
const WIDE_BREAKPOINT: u16 = 170;
const FILES_WIDTH: u16 = 30;
const TODO_WIDTH: u16 = 36;
const CENTER_MAX_WIDTH: u16 = 120;
const PANEL_PADDING: u16 = 1;
const MESSAGE_PADDING_X: usize = 1;
const MESSAGE_BUTTON_RIGHT_MARGIN: usize = 1;
const ASK_USER_WIDTH: u16 = 80;
const ASK_USER_MAX_HEIGHT: u16 = 32;

/// Render one frame and rebuild all geometry consumed by mouse handling.
pub fn draw(frame: &mut Frame, app: &mut App) {
    app.layout = LayoutSnapshot::default();
    clear_hitboxes(app);

    let root = frame.area();
    let (body, footer) = body_and_footer(root);
    let file_input_modal = matches!(
        app.files_context,
        FilesContext::NewTarget | FilesContext::Rename
    );
    let interactive = app.overlay.is_none() && !file_input_modal;

    if root.width >= WIDE_BREAKPOINT {
        draw_wide_workspace(frame, app, body, interactive);
    } else {
        draw_narrow_workspace(frame, app, body, interactive);
    }
    draw_footer(frame, app, footer);
    if let Some(message) = app.notifications.visible() {
        draw_notification(frame, root, &message);
    }

    if let Some(overlay) = app.overlay {
        // Background widgets may still be visible, but an overlay owns all input.
        // Keeping no base hitboxes makes that ownership explicit to mouse code.
        clear_hitboxes(app);
        let area = draw_overlay(frame, app, root, overlay);
        app.layout.overlay = non_empty(area);
    } else if file_input_modal {
        clear_hitboxes(app);
        let area = draw_file_input_modal(frame, app, root);
        app.layout.overlay = non_empty(area);
    }
}

fn clear_hitboxes(app: &mut App) {
    app.hitboxes.clear();
    app.file_hitboxes.clear();
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

fn draw_wide_workspace(frame: &mut Frame, app: &mut App, body: Rect, interactive: bool) {
    let files = Rect::new(body.x, body.y, FILES_WIDTH.min(body.width), body.height);
    let todo_width = TODO_WIDTH.min(body.width.saturating_sub(files.width));
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
    draw_files(frame, app, files, interactive);
    draw_center(frame, app, center_region, interactive);
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
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(" Agent ")
        .border_style(focus_border(app.focus == Focus::Agent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if app.ai_running {
        draw_animated_border(frame, area, app.animation_tick);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if app.agent_prompt.is_empty() && app.agent_output.is_empty() {
        return;
    }
    let mut lines = Vec::new();
    if !app.agent_prompt.is_empty() {
        lines.push(Line::from(Span::styled(
            "Prompt",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.extend(crate::markdown::to_lines_at_width(
            &app.agent_prompt,
            inner.width as usize,
        ));
    }
    if !app.agent_output.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            "Response",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
        lines.extend(crate::markdown::to_lines_at_width(
            &app.agent_output.join("\n\n"),
            inner.width as usize,
        ));
    }
    let max_scroll = lines.len().saturating_sub(inner.height as usize) as u16;
    app.agent_scroll = app.agent_scroll.min(max_scroll);
    let visible = visible_line_window(&lines, app.agent_scroll as usize, inner.height as usize);
    frame.render_widget(Paragraph::new(visible), inner);
}

fn draw_narrow_workspace(frame: &mut Frame, app: &mut App, body: Rect, interactive: bool) {
    if app.focus == Focus::Files || app.files_context != FilesContext::Browse {
        app.layout.files = non_empty(body);
        draw_files(frame, app, body, interactive);
    } else if app.focus == Focus::Todo {
        app.layout.todo = non_empty(body);
        draw_todo(frame, app, body, interactive);
    } else if app.focus == Focus::Agent {
        app.layout.agent = non_empty(body);
        draw_agent_output(frame, app, body);
    } else {
        app.layout.center = non_empty(body);
        draw_center(frame, app, body, interactive);
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

fn draw_files(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let focused = app.focus == Focus::Files;
    let title = match app.files_context {
        FilesContext::Browse => " Files ",
        FilesContext::Search => " Files · search ",
        FilesContext::MoveTarget => " Files · move to ",
        FilesContext::NewTarget => " Files · new ",
        FilesContext::Rename => " Files · rename ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(title)
        .border_style(focus_border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (input_area, list_area) = match app.files_context {
        FilesContext::Search if inner.height > 0 => (
            Some(Rect::new(inner.x, inner.y, inner.width, 1)),
            Rect::new(
                inner.x,
                inner.y.saturating_add(1),
                inner.width,
                inner.height.saturating_sub(1),
            ),
        ),
        _ => (None, inner),
    };

    if let Some(input_area) = input_area {
        let (prompt, value) = match app.files_context {
            FilesContext::Search => ("/ ", app.file_query.as_str()),
            _ => ("", ""),
        };
        draw_single_line_input(
            frame,
            input_area,
            prompt,
            value,
            value.chars().count(),
            focused && interactive,
        );
    }

    if list_area.width == 0 || list_area.height == 0 {
        return;
    }

    let visible_indices = app.visible_file_indices();
    if visible_indices.is_empty() {
        let message = if app.files_context == FilesContext::Search && !app.file_query.is_empty() {
            "No matching files"
        } else {
            "No files yet"
        };
        frame.render_widget(
            Paragraph::new(message).alignment(Alignment::Center),
            list_area,
        );
        return;
    }

    // File order comes from App/Storage and is recent-first. Each two-line row
    // reads like a compact conversation: name first, timestamp beneath it.
    let slots = usize::from(list_area.height.div_ceil(2));
    let selected_position = visible_indices
        .iter()
        .position(|index| *index == app.file_index)
        .unwrap_or(0);
    let start = selected_position
        .saturating_sub(slots.saturating_sub(1))
        .min(visible_indices.len().saturating_sub(slots));

    for (slot, absolute_index) in visible_indices
        .iter()
        .copied()
        .skip(start)
        .take(slots)
        .enumerate()
    {
        let Some(file) = app.note_files.get(absolute_index) else {
            continue;
        };
        let y = list_area.y.saturating_add((slot as u16).saturating_mul(2));
        if y >= list_area.y.saturating_add(list_area.height) {
            break;
        }
        let row_height = 2.min(list_area.y + list_area.height - y);
        let selected = absolute_index == app.file_index;
        let row_style = if selected {
            Style::default()
                .bg(if focused {
                    Color::DarkGray
                } else {
                    Color::Indexed(235)
                })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let name = file
            .path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("?");
        frame.render_widget(
            Paragraph::new(Line::from(format!(" {name}"))).style(row_style),
            Rect::new(list_area.x, y, list_area.width, 1),
        );
        if row_height > 1 {
            let modified: DateTime<Local> = file.modified.into();
            let modified_style = Style::default().fg(if selected {
                Color::Gray
            } else {
                Color::DarkGray
            });
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {}", modified.format("%m-%d %H:%M")),
                    modified_style,
                )))
                .style(row_style),
                Rect::new(list_area.x, y + 1, list_area.width, 1),
            );
        }
        if interactive {
            app.file_hitboxes.push(FileHitbox {
                path: file.path.clone(),
                area: Rect::new(list_area.x, y, list_area.width, row_height),
            });
        }
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
        .border_style(focus_border(focused));
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
            wrap_spans_to_width(&[Span::raw(item.text.replace('\n', " "))], text_width).len()
        })
        .collect();
    let mut start = selected_position;
    let mut used = item_heights[selected_position];
    while start > 0 && used + item_heights[start - 1] <= inner.height as usize {
        start -= 1;
        used += item_heights[start];
    }

    let mut y = inner.y;
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
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan)
        };
        let mut text_style = if item.checked {
            Style::default().add_modifier(Modifier::CROSSED_OUT)
        } else {
            Style::default()
        };
        if focused && index == selected {
            text_style = text_style.bg(Color::DarkGray);
        }
        let wrapped = wrap_spans_to_width(
            &[Span::styled(item.text.replace('\n', " "), text_style)],
            text_width,
        );
        let visible_height =
            (wrapped.len() as u16).min(inner.y.saturating_add(inner.height).saturating_sub(y));
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
                Paragraph::new(Line::from(line)).style(text_style),
                Rect::new(inner.x, y + row as u16, inner.width, 1),
            );
        }
        let item_area = Rect::new(inner.x, y, inner.width, visible_height);
        if interactive {
            app.todo_hitboxes.push(TodoHitbox {
                index,
                area: item_area,
            });
        }
        y = y.saturating_add(visible_height);
    }
}

fn focus_border(focused: bool) -> Style {
    Style::default().fg(if focused {
        Color::Green
    } else {
        Color::DarkGray
    })
}

fn animated_color(position: usize, tick: u64) -> Color {
    const STOPS: [(u8, u8, u8); 6] = [
        (50, 220, 255),
        (70, 120, 255),
        (210, 80, 255),
        (255, 80, 150),
        (255, 210, 70),
        (80, 240, 160),
    ];
    const STEPS: usize = 24;
    let phase = (position + tick as usize * 3) % (STOPS.len() * STEPS);
    let stop = phase / STEPS;
    let amount = phase % STEPS;
    let from = STOPS[stop];
    let to = STOPS[(stop + 1) % STOPS.len()];
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

fn draw_animated_border(frame: &mut Frame, area: Rect, tick: u64) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mut position = 0usize;
    for x in area.x..area.x.saturating_add(area.width) {
        frame.buffer_mut()[(x, area.y)].set_fg(animated_color(position, tick));
        position += 1;
    }
    for y in area.y.saturating_add(1)..area.y.saturating_add(area.height) {
        let x = area.x.saturating_add(area.width.saturating_sub(1));
        frame.buffer_mut()[(x, y)].set_fg(animated_color(position, tick));
        position += 1;
    }
    if area.height > 1 {
        let y = area.y.saturating_add(area.height - 1);
        for x in (area.x..area.x.saturating_add(area.width.saturating_sub(1))).rev() {
            frame.buffer_mut()[(x, y)].set_fg(animated_color(position, tick));
            position += 1;
        }
    }
    if area.width > 1 {
        for y in
            (area.y.saturating_add(1)..area.y.saturating_add(area.height.saturating_sub(1))).rev()
        {
            frame.buffer_mut()[(area.x, y)].set_fg(animated_color(position, tick));
            position += 1;
        }
    }
}

fn draw_center(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
    let content = center_content_axis(area);
    match app.center_view {
        CenterView::Chat => draw_chat(frame, app, area, content, interactive),
        CenterView::Document => draw_document(frame, app, content, interactive),
        CenterView::Search => draw_search(frame, app, content, interactive),
    }
}

fn draw_chat(frame: &mut Frame, app: &mut App, surface: Rect, content: Rect, interactive: bool) {
    if surface.width == 0 || surface.height == 0 {
        return;
    }
    let content = inset_horizontal(content, 2);
    if content.width == 0 || content.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Chat",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(content.x, content.y, content.width, 1),
    );

    let compose = compose_rect(content);
    app.layout.compose = non_empty(compose);

    let message_top = content.y.saturating_add(2).min(content.y + content.height);
    let message_bottom = compose.y.saturating_sub(1).min(content.y + content.height);
    let message_view = Rect::new(
        content.x,
        message_top,
        content.width,
        message_bottom.saturating_sub(message_top),
    );
    draw_messages(frame, app, message_view, interactive);

    if compose.width > 0 && compose.height > 0 {
        frame.render_widget(Clear, compose);
        draw_compose(frame, app, compose, interactive);
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

fn draw_messages(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if app.messages.is_empty() {
        frame.render_widget(
            Paragraph::new("No notes yet").alignment(Alignment::Center),
            area,
        );
        return;
    }

    let width = area.width as usize;
    let mut lines = Vec::new();
    let mut card_first = Vec::with_capacity(app.messages.len());
    let mut button_lines = Vec::with_capacity(app.messages.len());
    let mut button_starts = Vec::with_capacity(app.messages.len());

    for (index, message) in app.messages.iter().enumerate() {
        card_first.push(lines.len());
        let selected = index == app.selected;
        let card_style = Style::default().bg(if selected {
            Color::Indexed(238)
        } else {
            Color::Indexed(235)
        });
        let horizontal_padding = MESSAGE_PADDING_X.min(width.saturating_sub(1) / 2);
        lines.push(line_with_background(Vec::new(), width, card_style));
        let date = message.created_at.format(DATE_FMT).to_string();
        let time = message.created_at.format(TIME_FMT).to_string();
        let metadata_width = UnicodeWidthStr::width(date.as_str());
        let column_gap = 2;
        let body_start = horizontal_padding + metadata_width + column_gap;
        let (body_start, body_width) = centered_message_body_axis(width, body_start);
        let markdown_lines = crate::markdown::to_lines_at_width(&message.body, body_width);
        let mut body_rows = Vec::new();
        for markdown_line in markdown_lines {
            body_rows.extend(wrap_spans_to_width(&markdown_line.spans, body_width));
        }
        for row in 0..body_rows.len().max(2) {
            let metadata = match row {
                0 => format!("{date:>metadata_width$}"),
                1 => format!("{time:>metadata_width$}"),
                _ => " ".repeat(metadata_width),
            };
            let mut spans = Vec::with_capacity(body_rows.get(row).map_or(3, |body| body.len() + 3));
            spans.push(Span::raw(" ".repeat(horizontal_padding)));
            spans.push(Span::styled(metadata, Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw(" ".repeat(
                body_start.saturating_sub(horizontal_padding + metadata_width),
            )));
            if let Some(body) = body_rows.get(row) {
                spans.extend(body.iter().cloned());
            }
            lines.push(line_with_background(spans, width, card_style));
        }
        if body_rows.len() >= 2 {
            lines.push(line_with_background(Vec::new(), width, card_style));
        }
        let button_width = action_buttons_width();
        let button_start = width
            .saturating_sub(MESSAGE_BUTTON_RIGHT_MARGIN)
            .saturating_sub(button_width);
        button_lines.push(lines.len());
        button_starts.push(button_start);
        let mut button_spans = vec![Span::raw(" ".repeat(button_start))];
        button_spans.extend(render_button_line(selected).spans);
        lines.push(line_with_background(button_spans, width, card_style));
        lines.push(line_with_background(Vec::new(), width, card_style));
        lines.push(Line::default());
    }

    let total = lines.len();
    let view_height = area.height as usize;
    let max_scroll = total.saturating_sub(view_height);
    let mut scroll = (app.scroll as usize).min(max_scroll).min(u16::MAX as usize);
    if let (Some(first), Some(button)) = (
        card_first.get(app.selected).copied(),
        button_lines.get(app.selected).copied(),
    ) {
        scroll = stable_card_scroll(scroll, first, button, view_height);
    }
    scroll = scroll.min(max_scroll).min(u16::MAX as usize);
    app.scroll = scroll as u16;

    if interactive {
        for (index, message) in app.messages.iter().enumerate() {
            let Some(button_line) = button_lines.get(index).copied() else {
                continue;
            };
            let Some(button_start) = button_starts.get(index).copied() else {
                continue;
            };
            if button_line < scroll || button_line >= scroll.saturating_add(view_height) {
                continue;
            }
            let y = area.y + (button_line - scroll) as u16;
            register_buttons_clipped(
                &mut app.hitboxes,
                &message.id,
                area.x.saturating_add(button_start as u16),
                y,
                area,
            );
        }
    }

    let visible = visible_line_window(&lines, scroll, view_height);
    frame.render_widget(Paragraph::new(visible), area);
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

fn centered_message_body_axis(width: usize, desired_start: usize) -> (usize, usize) {
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

fn render_button_line(selected: bool) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, action) in Action::all().iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if *action == Action::Ai {
            Style::default()
                .fg(Color::Black)
                .bg(Color::LightMagenta)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan)
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
    message_id: &str,
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
                message_id: message_id.to_string(),
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

fn draw_compose(frame: &mut Frame, app: &App, area: Rect, interactive: bool) {
    let focused = app.focus == Focus::Compose;
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(if focused {
            " Compose "
        } else {
            " Compose · i "
        })
        .border_style(focus_border(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let (text_area, toolbar) = split_last_row(inner);
    draw_multiline_input(
        frame,
        text_area,
        &app.input,
        app.input_cursor,
        "Write something…",
        focused && interactive,
    );

    if toolbar.height > 0 {
        let lines = if app.input.is_empty() {
            0
        } else {
            app.input.lines().count().max(1)
        };
        let count = format!("{lines}l · {}c", app.input.chars().count());
        let hint = if focused && toolbar.width >= 62 {
            match app.center_view {
                CenterView::Document => "Enter record · Ctrl+Enter Agent · Ctrl+J newline",
                _ => "Enter send · Ctrl+Enter Agent · Ctrl+J newline",
            }
        } else if focused && toolbar.width >= 25 {
            "Ctrl+Enter Agent"
        } else {
            ""
        };
        draw_left_right_line(frame, toolbar, &count, hint, Color::DarkGray);
    }
}

fn draw_document(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
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
    let document_bottom = compose.y.saturating_sub(1);
    let header = Rect::new(content.x, content.y, content.width, 1);
    let document_area = Rect::new(
        content.x,
        content.y.saturating_add(2),
        content.width,
        document_bottom.saturating_sub(content.y.saturating_add(2)),
    );
    {
        let document = app.document.as_mut().expect("document checked above");
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    document.title.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  Esc back", Style::default().fg(Color::DarkGray)),
            ])),
            header,
        );
        let lines =
            crate::markdown::to_lines_at_width(&document.source, document_area.width as usize);
        if let Some(target_line) = document.target_line.take() {
            document.scroll = crate::markdown::rendered_row_for_source_line(
                &document.source,
                target_line,
                document_area.width as usize,
            )
            .min(u16::MAX as usize) as u16;
        }
        let max_scroll = lines.len().saturating_sub(document_area.height as usize);
        document.scroll = (document.scroll as usize).min(max_scroll) as u16;
        let visible = visible_line_window(
            &lines,
            document.scroll as usize,
            document_area.height as usize,
        );
        frame.render_widget(Paragraph::new(visible), document_area);
    }
    if compose.width > 0 && compose.height > 0 {
        frame.render_widget(Clear, compose);
        draw_compose(frame, app, compose, interactive);
    }
}

fn draw_notification(frame: &mut Frame, root: Rect, message: &str) {
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
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(message.to_string())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .padding(Padding::horizontal(1))
                    .title(" Notification ")
                    .style(Style::default().bg(Color::Indexed(235)))
                    .border_style(Style::default().fg(Color::LightYellow)),
            ),
        area,
    );
}

fn draw_search(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
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
    let input_style = Style::default().bg(Color::Indexed(235));
    if input_height >= 3 {
        frame.render_widget(Clear, input_box);
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .title(format!(" Searcher · {} ", app.search_results.len()))
            .style(input_style)
            .border_style(focus_border(app.focus == Focus::Center));
        let input = block.inner(input_box);
        frame.render_widget(block, input_box);
        draw_single_line_input(
            frame,
            input,
            "/ ",
            &app.search_query,
            app.search_query.chars().count(),
            app.focus == Focus::Center && interactive,
        );
    } else {
        draw_single_line_input(
            frame,
            input_box,
            "/ ",
            &app.search_query,
            app.search_query.chars().count(),
            app.focus == Focus::Center && interactive,
        );
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
            SearchHit::Message { text, .. } => vec![
                Span::styled("• ", Style::default().fg(Color::Cyan)),
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
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(text.clone()),
                ]
            }
        };
        let style = if index == selected {
            Style::default().bg(Color::DarkGray)
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
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prompt.to_string(), Style::default().fg(Color::Green)),
            Span::raw(value.to_string()),
        ])),
        area,
    );
    if show_cursor {
        let cursor_byte = char_to_byte(value, cursor.min(value.chars().count()));
        let column = UnicodeWidthStr::width(prompt) + UnicodeWidthStr::width(&value[..cursor_byte]);
        let x = area.x + (column as u16).min(area.width.saturating_sub(1));
        frame.set_cursor_position((x, area.y));
    }
}

fn draw_multiline_input(
    frame: &mut Frame,
    area: Rect,
    value: &str,
    cursor: usize,
    placeholder: &str,
    show_cursor: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width as usize;
    let lines: Vec<Line> = if value.is_empty() {
        wrap_spans_to_width(
            &[Span::styled(
                placeholder.to_string(),
                Style::default().fg(Color::DarkGray),
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
    if show_cursor {
        let x = area.x + (cursor_column % width.max(1)) as u16;
        let visible_row = wrapped_cursor_row.saturating_sub(scroll);
        let y = area.y + (visible_row as u16).min(area.height.saturating_sub(1));
        frame.set_cursor_position((x.min(area.x + area.width - 1), y));
    }
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
        _ => "CHAT",
    };
    let mode = format!(" {surface} · {} ", app.permission_mode.label());
    let mode_style = Style::default().bg(Color::Blue).fg(Color::Black);
    frame.render_widget(Paragraph::new(Span::styled(mode.clone(), mode_style)), area);

    let hint = footer_hint(app, area.width);
    let mode_width = mode.width() as u16;
    let available_status = area
        .width
        .saturating_sub(mode_width)
        .saturating_sub(hint.width() as u16)
        .saturating_sub(u16::from(!hint.is_empty()));
    if !app.status.is_empty() && available_status > 2 {
        let status = if app.ai_running {
            animated_status_line(&app.status, available_status as usize, app.animation_tick)
        } else {
            Line::from(Span::styled(
                format!(" {}", app.status),
                Style::default().fg(Color::Yellow),
            ))
        };
        frame.render_widget(
            Paragraph::new(status),
            Rect::new(area.x + mode_width, area.y, available_status, area.height),
        );
    }
    if !hint.is_empty() {
        let width = (hint.width() as u16).min(area.width);
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray)))
                .alignment(Alignment::Right),
            Rect::new(area.x + area.width - width, area.y, width, area.height),
        );
    }
}

fn animated_status_line(status: &str, width: usize, tick: u64) -> Line<'static> {
    if width == 0 || status.is_empty() {
        return Line::default();
    }
    let mut visible = Vec::new();
    let mut used = 1usize.min(width);
    for character in status.chars() {
        let character = if character == '\n' || character == '\r' {
            ' '
        } else {
            character
        };
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if character_width == 0 {
            continue;
        }
        if used + character_width > width {
            break;
        }
        visible.push((character, character_width));
        used += character_width;
    }
    if visible.is_empty() {
        return Line::from(" ".repeat(width.min(1)));
    }
    let active = tick as usize % visible.len();
    let mut spans = vec![Span::raw(" ")];
    for (index, (character, _)) in visible.into_iter().enumerate() {
        let style = if index == active {
            Style::default()
                .fg(animated_color(index * 8, tick))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(character.to_string(), style));
    }
    Line::from(spans)
}

fn footer_hint(app: &App, width: u16) -> &'static str {
    if width < 28 {
        return "";
    }
    if width < 55 {
        return match (app.focus, app.center_view) {
            (Focus::Compose, CenterView::Document) => "Esc document",
            (Focus::Compose, _) => "Esc chat",
            (Focus::Files, _) => "Esc back · Enter open",
            (Focus::Todo, _) => "Esc back · Enter toggle",
            (Focus::Agent, _) => "Enter add · Esc back",
            (Focus::Center, _) => "? help",
        };
    }
    match (app.focus, app.center_view) {
        (Focus::Compose, CenterView::Chat) => "Enter send · Ctrl+Enter Agent · Ctrl+J newline",
        (Focus::Compose, CenterView::Document) => {
            "Enter record · Ctrl+Enter Agent · Ctrl+J newline"
        }
        (Focus::Files, _) => "↑↓ select · Enter open · / filter · Esc back",
        (Focus::Todo, _) => "↑↓ select · Enter toggle · Esc back",
        (Focus::Agent, _) => "Enter add to Chat · ↑↓ scroll · ← center",
        (_, CenterView::Chat) if width >= 95 => "i compose · f files · T todo · / search · ? help",
        (_, CenterView::Document)
            if app.document.as_ref().is_some_and(|document| {
                matches!(document.kind, crate::app::DocumentKind::File(_))
            }) =>
        {
            "↑↓ scroll · e editor · Esc back"
        }
        (_, CenterView::Document) => "↑↓ scroll · e edit message · Esc back",
        (_, CenterView::Search) => "type query · ↑↓ select · Enter open · Esc back",
        _ => "f files · T todo · ? help",
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

fn draw_file_input_modal(frame: &mut Frame, app: &App, root: Rect) -> Rect {
    let area = centered_rect(root, 56.min(root.width), 5.min(root.height));
    if area.width == 0 || area.height == 0 {
        return area;
    }
    let (title, prompt, value, cursor) = match app.files_context {
        FilesContext::NewTarget => (
            " New file · Enter create · Esc cancel ",
            "Name  ",
            app.new_file_input.as_str(),
            app.new_file_cursor,
        ),
        FilesContext::Rename => (
            " Rename file · Enter save · Esc cancel ",
            "Name  ",
            app.rename_input.as_str(),
            app.rename_cursor,
        ),
        _ => return Rect::new(area.x, area.y, 0, 0),
    };
    frame.render_widget(Clear, area);
    let modal_style = Style::default().bg(Color::Indexed(235));
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::uniform(1))
        .title(title)
        .style(modal_style)
        .border_style(focus_border(true));
    let input = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new("").style(modal_style), input);
    draw_single_line_input(frame, input, prompt, value, cursor, true);
    area
}

fn draw_overlay(frame: &mut Frame, app: &mut App, root: Rect, overlay: Overlay) -> Rect {
    match overlay {
        Overlay::Help => draw_help(frame, app, root),
        Overlay::AiPrompt => draw_ai_prompt(frame, app, root),
        Overlay::Approval => draw_approval(frame, app, root),
        Overlay::AskUser => draw_ask_user(frame, app, root),
        Overlay::ConfirmDeleteMessage => draw_confirmation(
            frame,
            root,
            " Delete message ",
            "Delete this message? [y/N]",
        ),
        Overlay::ConfirmDeleteFile => {
            let name = app
                .pending_file
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "this file".to_string());
            draw_confirmation(
                frame,
                root,
                " Delete file ",
                &format!("Delete {name}? [y/N]"),
            )
        }
    }
}

fn draw_ai_prompt(frame: &mut Frame, app: &App, root: Rect) -> Rect {
    let area = centered_rect(root, root.width.min(80), root.height.min(11));
    if area.width == 0 || area.height == 0 {
        return area;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::uniform(1))
        .title(" Agent prompt · Enter run · Esc cancel ")
        .style(Style::default().bg(Color::Indexed(235)))
        .border_style(Style::default().fg(Color::LightMagenta));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_multiline_input(
        frame,
        inner,
        &app.ai_prompt_input,
        app.ai_prompt_cursor,
        "Optional prompt; empty uses the card content",
        true,
    );
    area
}

fn draw_approval(frame: &mut Frame, app: &mut App, root: Rect) -> Rect {
    let area = centered_rect(
        root,
        root.width.saturating_sub(4).min(110),
        root.height.saturating_sub(4).min(36),
    );
    if area.width == 0 || area.height == 0 {
        return area;
    }
    frame.render_widget(Clear, area);
    let title = app
        .approval_request
        .as_ref()
        .map(|request| format!(" {} ", request.title))
        .unwrap_or_else(|| " Approve change ".to_string());
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(title)
        .style(Style::default().bg(Color::Indexed(234)))
        .border_style(Style::default().fg(Color::LightYellow));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return area;
    }
    let content = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let footer = Rect::new(
        inner.x,
        inner.y.saturating_add(inner.height.saturating_sub(1)),
        inner.width,
        1.min(inner.height),
    );
    if let Some(request) = &app.approval_request {
        let source = format!("```diff\n{}\n```", request.diff);
        let lines = crate::markdown::to_lines_at_width(&source, content.width as usize);
        let max_scroll = lines.len().saturating_sub(content.height as usize) as u16;
        app.approval_scroll = app.approval_scroll.min(max_scroll);
        let visible = visible_line_window(
            &lines,
            app.approval_scroll as usize,
            content.height as usize,
        );
        frame.render_widget(Paragraph::new(visible), content);
    }
    frame.render_widget(
        Paragraph::new("Enter/Y approve · N/Esc deny · ↑↓ scroll · Tab bypass")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray)),
        footer,
    );
    area
}

fn draw_ask_user(frame: &mut Frame, app: &App, root: Rect) -> Rect {
    let width = ASK_USER_WIDTH.min(root.width.saturating_sub(4));
    let text_width = width.saturating_sub(4).max(1) as usize;
    let question_rows = app
        .ask_user_request
        .as_ref()
        .map(|request| {
            wrap_spans_to_width(&[Span::raw(request.question.clone())], text_width)
                .len()
                .clamp(1, 8) as u16
        })
        .unwrap_or(1);
    let option_rows = app
        .ask_user_request
        .as_ref()
        .map_or(0, |request| request.options.len() as u16 + 1);
    let desired_height = question_rows
        .saturating_add(option_rows)
        .saturating_add(4) // Free-text input.
        .saturating_add(1) // Keyboard hint.
        .saturating_add(2); // Borders.
    let height = desired_height
        .min(ASK_USER_MAX_HEIGHT)
        .min(root.height.saturating_sub(2));
    let area = centered_rect(root, width, height);
    if area.width == 0 || area.height == 0 {
        return area;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(" Agent question ")
        .style(Style::default().bg(Color::Indexed(234)))
        .border_style(Style::default().fg(Color::LightCyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(request) = &app.ask_user_request else {
        return area;
    };
    if inner.height == 0 {
        return area;
    }

    let footer_height = u16::from(inner.height > 0);
    let input_height = inner.height.saturating_sub(footer_height).min(4);
    let available = inner
        .height
        .saturating_sub(footer_height)
        .saturating_sub(input_height);
    let question_height = question_rows.min(available);
    let option_rows = if request.options.is_empty() {
        0
    } else {
        (request.options.len() as u16 + 1).min(available.saturating_sub(question_height))
    };
    let question = Rect::new(inner.x, inner.y, inner.width, question_height);
    let options = Rect::new(
        inner.x,
        question.y.saturating_add(question.height),
        inner.width,
        option_rows,
    );
    let input = Rect::new(
        inner.x,
        options.y.saturating_add(options.height),
        inner.width,
        input_height,
    );
    let footer = Rect::new(
        inner.x,
        input.y.saturating_add(input.height),
        inner.width,
        footer_height,
    );

    frame.render_widget(
        Paragraph::new(request.question.clone())
            .wrap(Wrap { trim: false })
            .style(Style::default().add_modifier(Modifier::BOLD)),
        question,
    );
    for (row, label) in request
        .options
        .iter()
        .map(|option| option.as_str())
        .chain(std::iter::once("Other answer"))
        .take(options.height as usize)
        .enumerate()
    {
        let selected = app.ask_user_option == row;
        let style = if selected {
            Style::default().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        frame.render_widget(
            Paragraph::new(format!("{} {label}", if selected { ">" } else { " " })).style(style),
            Rect::new(options.x, options.y + row as u16, options.width, 1),
        );
    }

    if input.width > 0 && input.height > 0 {
        let custom_selected = app.ask_user_option >= request.options.len();
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(" Your answer ")
            .border_style(focus_border(custom_selected));
        let input_inner = input_block.inner(input);
        frame.render_widget(input_block, input);
        draw_multiline_input(
            frame,
            input_inner,
            &app.ask_user_input,
            app.ask_user_cursor,
            "Type a different response",
            custom_selected,
        );
    }
    frame.render_widget(
        Paragraph::new("↑↓ choose · Enter submit · type for custom answer · Esc cancel")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray)),
        footer,
    );
    area
}

fn draw_confirmation(frame: &mut Frame, root: Rect, title: &str, message: &str) -> Rect {
    let area = centered_rect(root, 56.min(root.width), 3.min(root.height));
    if area.width > 0 && area.height > 0 {
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(message.to_string())
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .padding(Padding::horizontal(PANEL_PADDING))
                        .title(title.to_string()),
                ),
            area,
        );
    }
    area
}

fn draw_help(frame: &mut Frame, app: &mut App, root: Rect) -> Rect {
    let width = root.width.saturating_sub(2).min(92).max(root.width.min(1));
    let height = root
        .height
        .saturating_sub(2)
        .min(30)
        .max(root.height.min(1));
    let area = centered_rect(root, width, height);
    if area.width == 0 || area.height == 0 {
        return area;
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(" Help · ↑↓ scroll · Esc close ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines = help_lines();
    let maximum = lines.len().saturating_sub(inner.height as usize);
    app.help_scroll = (app.help_scroll as usize).min(maximum) as u16;
    let visible = visible_line_window(&lines, app.help_scroll as usize, inner.height as usize);
    frame.render_widget(Paragraph::new(visible), inner);
    area
}

fn help_lines() -> Vec<Line<'static>> {
    let heading = |text: &str| {
        Line::from(Span::styled(
            text.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let key = |keys: &str, description: &str| {
        Line::from(vec![
            Span::styled(
                format!(" {keys:<16}"),
                Style::default()
                    .fg(Color::Green)
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
        key("Esc", "return / cancel"),
        key("?", "open this help"),
        Line::default(),
        heading("Chat"),
        key("i / Enter", "focus Compose"),
        key("j k / ↑ ↓", "select message"),
        key("t m a n", "todo · move · archive · new file"),
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
        key("i / Enter", "capture while reading"),
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
        key("Enter", "add final output to Chat"),
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

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tempfile::tempdir;

    use super::*;
    use crate::agent::ApprovalRequest;
    use crate::app::{Document, DocumentKind, DocumentReturn};
    use crate::model::TodoItem;
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
        terminal.draw(|frame| draw(frame, app)).unwrap();
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
            assert!(buffer_string(&terminal).contains("Chat"));
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
            assert_eq!(files, Rect::new(0, 0, 30, 23), "width {width}");
            assert_eq!(todo.width, 36, "width {width}");
            assert_eq!(todo.x + todo.width, width, "width {width}");
            assert_eq!(todo.height, 23u16.div_ceil(3), "width {width}");
            assert_eq!(agent.y, todo.y + todo.height, "width {width}");
            assert_eq!(agent.height, 23 - todo.height, "width {width}");
            let region_width = width - FILES_WIDTH - TODO_WIDTH;
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
        assert!(footer.starts_with(" CHAT "));
        assert!(footer.contains("saved-at-left"));
        assert!(footer.trim_end().ends_with("? help"));
    }

    #[test]
    fn running_agent_animates_its_border_and_footer_status() {
        let (mut app, _directory) = make_app();
        app.ai_running = true;
        app.agent_prompt = "Analyze this".to_string();
        app.status = "AI is working".to_string();
        app.animation_tick = 0;
        let first = render(&mut app, 170, 24);
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
        let first_highlights = (0..170)
            .filter(|x| matches!(first.backend().buffer()[(*x, 23)].fg, Color::Rgb(..)))
            .collect::<Vec<_>>();
        assert_eq!(first_highlights.len(), 1);

        app.animation_tick = 1;
        let second = render(&mut app, 170, 24);
        let second_corner = second.backend().buffer()[(agent.x, agent.y)].fg;
        let second_footer = buffer_string(&second).lines().last().unwrap().to_string();
        let second_highlights = (0..170)
            .filter(|x| matches!(second.backend().buffer()[(*x, 23)].fg, Color::Rgb(..)))
            .collect::<Vec<_>>();
        assert_ne!(first_corner, second_corner);
        assert_eq!(first_footer, second_footer);
        assert_eq!(second_highlights.len(), 1);
        assert_ne!(first_highlights, second_highlights);
    }

    #[test]
    fn animated_status_line_respects_terminal_cell_width() {
        let line = animated_status_line("正在调用工具", 19, 4);
        assert_eq!(line.to_string(), " 正在调用工具");
        assert_eq!(line.width(), 13);
        assert_eq!(
            line.spans
                .iter()
                .filter(|span| matches!(span.style.fg, Some(Color::Rgb(..))))
                .count(),
            1
        );
        assert!(line
            .spans
            .iter()
            .filter(|span| matches!(span.style.fg, Some(Color::Rgb(..))))
            .all(|span| span.style.add_modifier.contains(Modifier::BOLD)));
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
        assert_eq!(buffer_string(&terminal).matches("Files").count(), 1);
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
    fn selected_file_background_covers_name_and_modified_time_rows() {
        let (mut app, _directory) = make_app();
        fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
        app.reload_files();
        app.focus = Focus::Files;

        let terminal = render(&mut app, 170, 18);
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
                assert_eq!(buffer[(x, y)].bg, Color::DarkGray);
            }
        }
        assert_eq!(
            buffer[(selected_area.x + 1, selected_area.y + 1)].fg,
            Color::Gray,
            "modified time must remain legible on the selected background"
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
            Color::Indexed(235),
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
        assert!(app.layout.overlay.is_none());
    }

    #[test]
    fn narrow_center_renders_each_center_view_in_place() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.center_view = CenterView::Document;
        app.document = Some(Document {
            kind: DocumentKind::Message("id".to_string()),
            title: "Preview".to_string(),
            source: "# Heading".to_string(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Chat,
        });
        let terminal = render(&mut app, 80, 18);
        assert!(buffer_string(&terminal).contains("Heading"));

        app.center_view = CenterView::Search;
        app.search_query = "needle".to_string();
        app.search_results = vec![SearchHit::Message {
            id: "id".to_string(),
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
                    .append_chat_message(&format!("message {index}"))
                    .unwrap();
            }
            app.reload();
            app.selected = app.messages.len() - 1;
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
        app.storage.append_chat_message("hello").unwrap();
        app.reload();
        app.todo_items = vec![TodoItem {
            checked: false,
            text: "task".to_string(),
        }];
        app.focus = Focus::Center;
        app.overlay = Some(Overlay::Help);
        render(&mut app, 220, 24);
        assert!(app.layout.overlay.is_some());
        assert!(app.hitboxes.is_empty());
        assert!(app.file_hitboxes.is_empty());
        assert!(app.todo_hitboxes.is_empty());
        assert!(app.search_hitboxes.is_empty());
    }

    #[test]
    fn agent_prompt_and_diff_approval_render_as_opaque_overlays() {
        let (mut app, _directory) = make_app();
        app.overlay = Some(Overlay::AiPrompt);
        app.ai_prompt_input = "summarize this".to_string();
        app.ai_prompt_cursor = app.ai_prompt_input.chars().count();
        let terminal = render(&mut app, 100, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Agent prompt"));
        assert!(screen.contains("summarize this"));

        app.overlay = Some(Overlay::Approval);
        app.approval_request = Some(ApprovalRequest {
            title: "Update data/note.md".to_string(),
            diff: "--- old\n+++ new\n@@ -1 +1 @@\n-old value\n+new value\n".to_string(),
        });
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
        app.overlay = Some(Overlay::AskUser);
        app.ask_user_request = Some(crate::agent::AskUserRequest {
            question: "Which output format should be used?".to_string(),
            options: vec!["Markdown".to_string(), "MBDown".to_string()],
        });
        app.ask_user_option = 1;
        let terminal = render(&mut app, 100, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Agent question"));
        assert!(screen.contains("Which output format should be used?"));
        assert!(screen.contains("Markdown"));
        assert!(screen.contains("MBDown"));
        assert!(screen.contains("Other answer"));
        assert!(screen.contains("Your answer"));
        let overlay = app.layout.overlay.expect("ask-user overlay");
        assert_eq!(overlay.width, ASK_USER_WIDTH);
        assert_eq!(overlay.height, 11);
        assert!(app.hitboxes.is_empty());
    }

    #[test]
    fn agent_panel_shows_the_user_prompt_above_the_response() {
        let (mut app, _directory) = make_app();
        app.agent_prompt = "Explain the selected note".to_string();
        app.agent_output = vec!["Here is the explanation".to_string()];
        app.focus = Focus::Agent;

        let terminal = render(&mut app, 170, 24);
        let screen = buffer_string(&terminal);
        let prompt = screen.find("Explain the selected note").unwrap();
        let response = screen.find("Here is the explanation").unwrap();
        assert!(screen.contains("Prompt"));
        assert!(screen.contains("Response"));
        assert!(prompt < response);
    }

    #[test]
    fn document_keeps_compose_visible_and_notification_uses_top_right() {
        let (mut app, _directory) = make_app();
        app.center_view = CenterView::Document;
        app.focus = Focus::Center;
        app.document = Some(Document {
            kind: DocumentKind::File(app.storage.archive_path.clone()),
            title: "Article".to_string(),
            source: "# Reading\n\nUseful paragraph".to_string(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Chat,
        });
        app.notifications.notify("Recorded in Chat");
        let terminal = render(&mut app, 120, 24);
        let screen = buffer_string(&terminal);
        assert!(screen.contains("Reading"));
        assert!(screen.contains("Compose"));
        assert!(screen.contains("Notification"));
        assert!(screen.contains("Recorded in Chat"));
        let compose = app.layout.compose.expect("document compose");
        assert!(compose.y > 12);
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
        app.storage
            .append_chat_message("alpha\nbeta **bold**")
            .unwrap();
        app.reload();
        app.focus = Focus::Compose;
        app.input = "first\nsecond".to_string();
        app.input_cursor = app.input.chars().count();
        let terminal = render(&mut app, 120, 24);
        let screen = buffer_string(&terminal);
        for expected in ["alpha", "beta bold", "first", "second", "[todo]"] {
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
            "hitboxes must retain TODO.md source indices"
        );
    }

    #[test]
    fn chat_renders_block_markdown_on_colored_cards() {
        let (mut app, _directory) = make_app();
        app.storage
            .append_chat_message(concat!(
                "# Heading\n\n- first\n- second\n\n`code`\n\n",
                "[columns gap=2]\n",
                "[column]Left[/column]\n",
                "[column]Right[/column]\n",
                "[/columns]\n\n",
                "[bg=196]colored[/bg]"
            ))
            .unwrap();
        app.reload();
        let date = app.messages[0].created_at.format("%Y-%m-%d").to_string();
        let time = app.messages[0].created_at.format("%H:%M").to_string();
        let terminal = render(&mut app, 170, 40);
        let screen = buffer_string(&terminal);
        let screen_lines = screen.lines().collect::<Vec<_>>();
        let date_row = screen_lines
            .iter()
            .position(|line| line.contains(&date))
            .expect("missing message date");
        let date_line = screen_lines[date_row];
        let time_line = screen_lines
            .get(date_row + 1)
            .filter(|line| line.contains(&time))
            .expect("message time should be directly below its date");
        assert_eq!(
            date_line.find(&date).unwrap() + date.len(),
            time_line.rfind(&time).unwrap() + time.len(),
            "date and time should share a right edge"
        );
        assert!(
            date_line.find("Heading").unwrap() > date_line.find(&date).unwrap() + date.len(),
            "message body should be left-aligned in the column after the timestamp"
        );
        let buffer = terminal.backend().buffer();
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
            "Markdown messages no longer need a preview button"
        );
        assert!(
            buffer.content().iter().any(|cell| {
                cell.symbol() == "H"
                    && cell.modifier.contains(Modifier::BOLD)
                    && cell.bg == Color::Indexed(238)
            }),
            "heading should retain Markdown emphasis on the selected card background"
        );
        let ai = app
            .hitboxes
            .iter()
            .find(|hitbox| hitbox.action == Action::Ai)
            .expect("AI button");
        let center = app.layout.center.expect("center");
        assert_eq!(
            ai.area.x + ai.area.width,
            center.x + center.width - 3,
            "AI button should sit against the card's right padding"
        );
    }

    #[test]
    fn message_body_axis_has_symmetric_gutters() {
        let width = 100;
        let metadata_and_gap = MESSAGE_PADDING_X + UnicodeWidthStr::width("2026-07-27") + 2;
        let (start, body_width) = centered_message_body_axis(width, metadata_and_gap);
        let trailing = width - start - body_width;

        assert_eq!(start, metadata_and_gap);
        assert_eq!(trailing, start);
        assert_eq!(body_width, 74);
        assert_eq!(MESSAGE_BUTTON_RIGHT_MARGIN, 1);
    }

    #[test]
    fn oversized_selected_card_keeps_a_stable_scroll_position() {
        let (mut app, _directory) = make_app();
        let body = (0..80)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.storage.append_chat_message(&body).unwrap();
        app.reload();
        app.selected = app.messages.len() - 1;
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
    fn single_line_message_uses_the_time_row_as_button_spacing() {
        let (mut app, _directory) = make_app();
        app.storage.append_chat_message("one line").unwrap();
        app.reload();
        let date = app.messages[0].created_at.format(DATE_FMT).to_string();
        let time = app.messages[0].created_at.format(TIME_FMT).to_string();

        let terminal = render(&mut app, 170, 20);
        let screen = buffer_string(&terminal);
        let rows = screen.lines().collect::<Vec<_>>();
        let date_row = rows
            .iter()
            .position(|line| line.contains(&date) && line.contains("one line"))
            .expect("date and body row");
        assert!(rows[date_row + 1].contains(&time));
        let button_row = app
            .hitboxes
            .iter()
            .find(|hitbox| hitbox.action == Action::Delete)
            .expect("delete button")
            .area
            .y as usize;
        assert_eq!(button_row, date_row + 2);
    }

    #[test]
    fn document_view_has_padding_without_an_outer_border() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.center_view = CenterView::Document;
        app.document = Some(Document {
            kind: DocumentKind::File(app.storage.archive_path.clone()),
            title: "Archive".to_string(),
            source: "# Heading\n\nintro\n\nneedle".to_string(),
            scroll: 0,
            target_line: Some(5),
            return_to: DocumentReturn::Chat,
        });
        let terminal = render(&mut app, 80, 18);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), " ");
        assert!(buffer_string(&terminal).contains("Compose"));
        assert!(buffer_string(&terminal).contains("  Archive"));
        assert_eq!(app.document.as_ref().unwrap().scroll, 0);
        assert_eq!(app.document.as_ref().unwrap().target_line, None);
        let first_document_row: String = (0..80)
            .map(|x| buffer[(x, 2)].symbol().to_string())
            .collect();
        assert!(first_document_row.contains("Heading"));
        assert!(buffer_string(&terminal).contains("needle"));
    }

    #[test]
    fn document_code_block_background_has_no_wrapped_gaps() {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        app.center_view = CenterView::Document;
        app.document = Some(Document {
            kind: DocumentKind::File(app.storage.archive_path.clone()),
            title: "Code".to_string(),
            source: "```rust\nfn main() {\n    println!(\"hello\");\n}\n```".to_string(),
            scroll: 0,
            target_line: None,
            return_to: DocumentReturn::Chat,
        });

        let terminal = render(&mut app, 80, 20);
        let buffer = terminal.backend().buffer();
        let background = Color::Rgb(32, 36, 43);
        let rows = (0..buffer.area().height)
            .filter(|y| (0..buffer.area().width).any(|x| buffer[(x, *y)].bg == background))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 7);
        assert!(rows.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }
}
