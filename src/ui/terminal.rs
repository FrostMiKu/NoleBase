use super::*;

pub(super) const AGENT_TERMINAL_MONITOR_ROWS: u16 = 7;
const AGENT_TERMINAL_CONTENT_ROWS: u16 = 5;

pub(super) fn draw_agent_terminal_monitor(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height < AGENT_TERMINAL_MONITOR_ROWS {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(app.theme.surface_panel));
    let inner = block.inner(area);
    let Some(snapshot) = app.agent_terminal.monitor_snapshot(inner.width) else {
        return;
    };
    let status = snapshot.status.label();
    let fixed_width = UnicodeWidthStr::width(format!(" PTY ·  · {status} ").as_str());
    let title_width = area.width.saturating_sub(2) as usize;
    let command = truncate_terminal_title(&snapshot.title, title_width.saturating_sub(fixed_width));
    let border_color = match snapshot.status {
        crate::agent::AgentTerminalStatus::Running => app.theme.ui_action_ai,
        crate::agent::AgentTerminalStatus::Exited(_) => app.theme.text_muted,
    };
    frame.render_widget(
        block
            .title(format!(" PTY · {command} · {status} "))
            .border_style(Style::default().fg(border_color)),
        area,
    );

    let (rows, _) = snapshot.terminal.size();
    let cursor_row = snapshot.terminal.cursor_position().0;
    let visible_rows = AGENT_TERMINAL_CONTENT_ROWS.min(inner.height);
    let start_row = cursor_row
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(rows.saturating_sub(visible_rows));
    draw_terminal_snapshot_rows(
        frame,
        Rect::new(inner.x, inner.y, inner.width, visible_rows),
        &snapshot.terminal,
        start_row,
        app.theme,
        app.theme.surface_panel,
        None,
    );
}

fn truncate_terminal_title(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let mut result = String::new();
    let target = width - 1;
    for character in value.chars() {
        let character_width = character.width().unwrap_or(1);
        if UnicodeWidthStr::width(result.as_str()) + character_width > target {
            break;
        }
        result.push(character);
    }
    result.push('…');
    result
}

pub(super) fn draw_terminal(
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

pub(super) fn draw_terminal_snapshot(
    frame: &mut Frame,
    area: Rect,
    snapshot: &TerminalSnapshot,
    theme: Theme,
    cursor_position: &mut Option<Position>,
) {
    draw_terminal_snapshot_rows(
        frame,
        area,
        snapshot,
        0,
        theme,
        theme.surface_overlay,
        Some(cursor_position),
    );
}

fn draw_terminal_snapshot_rows(
    frame: &mut Frame,
    area: Rect,
    snapshot: &TerminalSnapshot,
    start_row: u16,
    theme: Theme,
    default_background: Color,
    cursor_position: Option<&mut Option<Position>>,
) {
    let (rows, cols) = snapshot.size();
    let rows = rows.saturating_sub(start_row).min(area.height);
    let cols = cols.min(area.width);
    let buffer = frame.buffer_mut();
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = snapshot.cell(start_row + row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut foreground = terminal_color(cell.foreground(), theme.text_primary);
            let mut background = terminal_color(cell.background(), default_background);
            if cell.inverse() {
                std::mem::swap(&mut foreground, &mut background);
            }
            let mut style = Style::default().fg(foreground).bg(background);
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.dim() {
                style = style.add_modifier(Modifier::DIM);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            let contents = cell.contents();
            buffer[(area.x + col, area.y + row)]
                .set_symbol(if contents.is_empty() { " " } else { contents })
                .set_style(style);
        }
    }

    if !snapshot.hide_cursor() {
        let (row, col) = snapshot.cursor_position();
        if let Some(cursor_position) = cursor_position {
            if row >= start_row && row < start_row + area.height && col < area.width {
                *cursor_position = Some(Position::new(area.x + col, area.y + row - start_row));
            }
        }
    }
}

pub(super) fn terminal_color(color: TerminalColor, default: Color) -> Color {
    match color {
        TerminalColor::Default => default,
        TerminalColor::Indexed(index) => Color::Indexed(index),
        TerminalColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}
