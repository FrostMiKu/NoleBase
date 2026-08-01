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
    for (row, (index, hit)) in app
        .search_results
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
        let metadata_style = if is_selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(app.theme.text_muted)
        };
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
        let style = if is_selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .bg(app.theme.selection_background)
        } else {
            Style::default()
        };
        let selection_area = is_selected.then(|| shared_selection_area(results, y, item_height));
        if let Some(selection_area) = selection_area {
            frame.render_widget(Block::default().style(style), selection_area);
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(style),
            inset_horizontal(Rect::new(item_area.x, item_area.y, item_area.width, 1), 2),
        );
        if let Some(selection_area) = selection_area {
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
