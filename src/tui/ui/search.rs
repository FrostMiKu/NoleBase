use super::*;

pub(super) fn draw_search(
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

    let title = match app.center_view {
        CenterView::DocumentSearch => "Search in Note",
        _ => "Searcher",
    };
    let results = draw_filter_header(
        frame,
        content,
        app,
        format!(" {title} · {} ", app.search_results.len()),
        "/ ",
        &app.search_query,
        app.search_cursor,
        interactive,
        cursor_position,
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
    let start = selection_viewport_start(
        app.search_list_start,
        selected,
        visible,
        app.search_results.len(),
    );
    app.search_list_start = start;
    for row in selection_rows(
        results,
        SELECT_OPTION_HEIGHT,
        start,
        visible,
        app.search_results.len(),
        selected,
    ) {
        let index = row.index;
        let hit = &app.search_results[index];
        let item_area = row.item_area;
        let is_selected = row.selection_area.is_some();
        let (style, metadata_style) = selection_styles(is_selected, app.theme);
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
        render_selection_background(frame, row, style);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(style),
            inset_horizontal(Rect::new(item_area.x, item_area.y, item_area.width, 1), 2),
        );
        if let Some(selection_area) = row.selection_area {
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
