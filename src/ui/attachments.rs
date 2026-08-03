use super::*;

pub(super) fn draw_attachments(
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

    let results = draw_filter_header(
        frame,
        content,
        app,
        format!(" Attachments · {} ", app.attachment_entries.len()),
        "",
        &app.attachment_query,
        app.attachment_cursor,
        interactive,
        cursor_position,
    );
    if results.height == 0 {
        return;
    }
    if app.attachment_entries.is_empty() {
        frame.render_widget(
            Paragraph::new(if app.attachment_query.is_empty() {
                "No attachments"
            } else {
                "No matches"
            })
            .alignment(Alignment::Center),
            results,
        );
        return;
    }

    let visible = visible_selection_items(results.height, SELECT_OPTION_HEIGHT);
    let selected = app
        .attachment_index
        .min(app.attachment_entries.len().saturating_sub(1));
    let start = selection_viewport_start(
        app.attachment_list_start,
        selected,
        visible,
        app.attachment_entries.len(),
    );
    app.attachment_list_start = start;
    for (row, (index, entry)) in app
        .attachment_entries
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
        // Distinct managed notes, not occurrence counts; unknown while the
        // shared usage index has not delivered its first snapshot.
        let references = if !app.attachment_usage.is_ready() {
            "…".to_string()
        } else if entry.locations == 1 {
            "1 note".to_string()
        } else {
            format!("{} notes", entry.locations)
        };
        let metadata = format!(
            "{} · {} · {}",
            entry.kind,
            human_size(entry.size),
            references
        );
        let metadata_width = UnicodeWidthStr::width(metadata.as_str()).min(row_area.width as usize);
        let name_width = (row_area.width as usize).saturating_sub(metadata_width.saturating_add(1));
        if name_width > 0 {
            frame.render_widget(
                Paragraph::new(Span::styled(entry.name.clone(), style)),
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
            app.attachment_hitboxes.push(AttachmentHitbox {
                index,
                area: item_area,
            });
        }
    }
}
