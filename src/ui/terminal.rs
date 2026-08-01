use super::*;

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
            let mut foreground = terminal_color(cell.foreground(), theme.text_primary);
            let mut background = terminal_color(cell.background(), theme.surface_overlay);
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
        if row < area.height && col < area.width {
            *cursor_position = Some(Position::new(area.x + col, area.y + row));
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
