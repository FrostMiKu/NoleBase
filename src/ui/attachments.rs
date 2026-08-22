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
    for row in selection_rows(
        results,
        SELECT_OPTION_HEIGHT,
        start,
        visible,
        app.attachment_entries.len(),
        selected,
    ) {
        let index = row.index;
        let entry = &app.attachment_entries[index];
        let item_area = row.item_area;
        let is_selected = row.selection_area.is_some();
        let (style, metadata_style) = selection_styles(is_selected, app.theme);
        render_selection_background(frame, row, style);
        let row_area = inset_horizontal(Rect::new(item_area.x, item_area.y, item_area.width, 1), 2);
        // Distinct managed notes rather than occurrence counts; the ellipsis
        // marks the interval before the shared usage index delivers its snapshot.
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
        render_label_metadata_row(
            frame,
            row_area,
            entry.name.clone(),
            metadata,
            style,
            metadata_style,
        );
        if let Some(selection_area) = row.selection_area {
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
