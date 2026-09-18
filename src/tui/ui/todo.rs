use super::*;

pub(super) fn draw_todo(
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

    let header_width = content.width.saturating_sub(4).clamp(1, 72);
    let header_height = 3.min(content.height);
    let header = Rect::new(
        content.x + content.width.saturating_sub(header_width) / 2,
        content.y,
        header_width,
        header_height,
    );
    let done = app.todo_items.iter().filter(|item| item.checked).count();
    if header_height >= 3 {
        clear_widget(frame, header);
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .title(format!(" Todo · {done}/{} ", app.todo_items.len()))
            .style(Style::default().bg(app.theme.surface_panel))
            .border_style(focus_border(app.focus == Focus::Center, app.theme));
        let input = block.inner(header);
        frame.render_widget(block, header);
        if let Some(position) = draw_single_line_input(
            frame,
            input,
            "/ ",
            &app.todo_query,
            app.todo_cursor,
            app.focus == Focus::Center && interactive,
            app.theme,
        ) {
            *cursor_position = Some(position);
        }
    } else if let Some(position) = draw_single_line_input(
        frame,
        header,
        "/ ",
        &app.todo_query,
        app.todo_cursor,
        app.focus == Focus::Center && interactive,
        app.theme,
    ) {
        *cursor_position = Some(position);
    }

    let list_y = header.y.saturating_add(header.height).saturating_add(1);
    let list = Rect::new(
        content.x,
        list_y,
        content.width,
        content
            .y
            .saturating_add(content.height)
            .saturating_sub(list_y),
    );
    if list.height == 0 {
        return;
    }
    if app.todo_items.is_empty() {
        frame.render_widget(
            Paragraph::new("No todos yet").alignment(Alignment::Center),
            list,
        );
        return;
    }

    let visible_indices = app.visible_todo_indices();
    if visible_indices.is_empty() {
        frame.render_widget(
            Paragraph::new("No matches").alignment(Alignment::Center),
            list,
        );
        return;
    }
    let selected = app.todo_index.min(app.todo_items.len().saturating_sub(1));
    let selected_position = visible_indices
        .iter()
        .position(|index| *index == selected)
        .unwrap_or(0);
    let text_width = list.width.saturating_sub(6).max(1) as usize;
    let item_heights = visible_indices
        .iter()
        .filter_map(|index| app.todo_items.get(*index))
        .map(|item| {
            wrap_spans_to_width(&[Span::raw(item.text.replace('\n', " "))], text_width).len() + 1
        })
        .collect::<Vec<_>>();
    let viewport_height = list.height.saturating_sub(1) as usize;
    if viewport_height == 0 {
        return;
    }
    let start = variable_selection_viewport_start(
        app.todo_list_start,
        selected_position,
        &item_heights,
        viewport_height,
    );
    app.todo_list_start = start;

    let mut y = list.y.saturating_add(1);
    for index in visible_indices.iter().copied().skip(start) {
        if y >= list.y.saturating_add(list.height) {
            break;
        }
        let Some(item) = app.todo_items.get(index) else {
            continue;
        };
        let checked = if item.checked { "[x]" } else { "[ ]" };
        let item_selected = app.focus == Focus::Center && index == selected;
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
            .min(list.y.saturating_add(list.height).saturating_sub(y));
        let selection_area = shared_selection_area(list, y, layout_height);
        if item_selected {
            frame.render_widget(
                Block::default().style(
                    Style::default()
                        .fg(app.theme.selection_foreground)
                        .bg(app.theme.selection_background),
                ),
                selection_area,
            );
        }
        for (row, mut spans) in wrapped
            .into_iter()
            .take(content_height.min(layout_height) as usize)
            .enumerate()
        {
            let mut line = if row == 0 {
                vec![Span::styled(format!("  {checked} "), marker_style)]
            } else {
                vec![Span::raw("      ")]
            };
            line.append(&mut spans);
            frame.render_widget(
                Paragraph::new(Line::from(line)),
                Rect::new(list.x, y + row as u16, list.width, 1),
            );
        }
        let item_area = Rect::new(list.x, y, list.width, layout_height);
        if interactive {
            app.todo_hitboxes.push(TodoHitbox {
                index,
                area: item_area,
            });
        }
        if item_selected {
            draw_selection_indicator(frame, selection_area, app.theme);
        }
        y = y.saturating_add(layout_height);
    }
}
