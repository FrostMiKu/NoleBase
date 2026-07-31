use super::*;

pub(super) fn draw_tags(
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
            .title(format!(" Tags · {} ", app.tag_results.len()))
            .style(input_style)
            .border_style(focus_border(app.focus == Focus::Center, app.theme));
        let input = block.inner(input_box);
        frame.render_widget(block, input_box);
        if let Some(position) = draw_single_line_input(
            frame,
            input,
            "# ",
            &app.tag_query,
            app.tag_query.chars().count(),
            app.focus == Focus::Center && interactive,
            app.theme,
        ) {
            *cursor_position = Some(position);
        }
    } else if let Some(position) = draw_single_line_input(
        frame,
        input_box,
        "# ",
        &app.tag_query,
        app.tag_query.chars().count(),
        app.focus == Focus::Center && interactive,
        app.theme,
    ) {
        *cursor_position = Some(position);
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
    if app.tag_results.is_empty() {
        frame.render_widget(
            Paragraph::new(if app.tag_query.is_empty() {
                "No tags found"
            } else {
                "No matches"
            })
            .alignment(Alignment::Center),
            results,
        );
        return;
    }

    let visible = visible_selection_items(results.height, SELECT_OPTION_HEIGHT);
    let selected = app.tag_index.min(app.tag_results.len().saturating_sub(1));
    let start =
        selection_viewport_start(app.tag_list_start, selected, visible, app.tag_results.len());
    app.tag_list_start = start;
    for (row, (index, tag)) in app
        .tag_results
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
        let style = if is_selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .bg(app.theme.selection_background)
        } else {
            Style::default()
        };
        let metadata_style = if is_selected {
            style.add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(app.theme.text_muted)
        };
        let selection_area = is_selected.then(|| shared_selection_area(results, y, item_height));
        if let Some(selection_area) = selection_area {
            frame.render_widget(Block::default().style(style), selection_area);
        }

        let row_area = inset_horizontal(Rect::new(item_area.x, item_area.y, item_area.width, 1), 2);
        let metadata = if row_area.width >= 32 {
            format!("{} documents · {} mentions", tag.documents, tag.mentions)
        } else {
            format!("{}d · {}x", tag.documents, tag.mentions)
        };
        let metadata_width = UnicodeWidthStr::width(metadata.as_str()).min(row_area.width as usize);
        let name_width = (row_area.width as usize).saturating_sub(metadata_width.saturating_add(1));
        if name_width > 0 {
            frame.render_widget(
                Paragraph::new(Span::styled(format!("#{}", tag.name), style)),
                Rect::new(row_area.x, row_area.y, name_width as u16, 1),
            );
        }
        if metadata_width > 0 {
            frame.render_widget(
                Paragraph::new(Span::styled(metadata, metadata_style)).alignment(Alignment::Right),
                Rect::new(
                    row_area.x + row_area.width.saturating_sub(metadata_width as u16),
                    row_area.y,
                    metadata_width as u16,
                    1,
                ),
            );
        }
        if let Some(selection_area) = selection_area {
            draw_selection_indicator(frame, selection_area, app.theme);
        }
        if interactive {
            app.tag_hitboxes.push(TagHitbox {
                name: tag.name.clone(),
                area: item_area,
            });
        }
    }
}
