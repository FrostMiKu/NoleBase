use super::*;

pub(super) fn draw_single_line_input(
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

pub(super) fn draw_multiline_input(
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

pub(super) fn visible_line_window<'a>(
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

pub(super) fn wrapped_row_count(line_width: usize, area_width: usize) -> usize {
    if line_width == 0 || area_width == 0 {
        1
    } else {
        line_width.div_ceil(area_width)
    }
}

pub(super) fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

pub(super) fn cursor_row_col(input: &str, cursor: usize) -> (usize, usize) {
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

