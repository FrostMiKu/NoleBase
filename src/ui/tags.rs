use super::*;

pub(super) fn draw_tags(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if app.active_tag.is_some() {
        draw_tag_note_stream(frame, app, area, interactive);
    } else {
        draw_tag_picker(frame, app, area, interactive, cursor_position);
    }
}

fn draw_tag_picker(
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
        format!(" Tags · {} ", app.tag_results.len()),
        "# ",
        &app.tag_query,
        app.tag_cursor,
        interactive,
        cursor_position,
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

fn draw_tag_note_stream(frame: &mut Frame, app: &mut App, area: Rect, interactive: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let content = inset_horizontal(area, 2);
    if content.width == 0 || content.height == 0 {
        return;
    }
    let tag = app.active_tag.as_deref().unwrap_or_default();
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("#{tag} · {} notes", app.tag_notes.len()),
            Style::default()
                .fg(app.theme.ui_page_heading)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(content.x, content.y, content.width, 1),
    );
    let cards = Rect::new(
        content.x,
        content.y.saturating_add(2),
        content.width,
        content.height.saturating_sub(2),
    );
    if cards.height == 0 {
        return;
    }
    if app.tag_notes.is_empty() {
        frame.render_widget(
            Paragraph::new("No notes contain this tag").alignment(Alignment::Center),
            cards,
        );
        return;
    }

    let width = cards.width as usize;
    sync_tag_note_vlist(app, width);
    let render_height = cards.height as usize;
    let mut scroll =
        (app.tag_note_scroll as usize).min(app.tag_note_vlist.geometry.max_scroll(render_height));
    if app.reveal_selected_tag_note && app.tag_note_index < app.tag_notes.len() {
        ensure_tag_note_card_rendered(app, app.tag_note_index);
        let first = app.tag_note_vlist.geometry.item_top(app.tag_note_index);
        let last = first
            + app
                .tag_note_vlist
                .geometry
                .height(app.tag_note_index)
                .saturating_sub(1);
        scroll = stable_card_scroll(scroll, first, last, render_height);
        app.reveal_selected_tag_note = false;
    }
    scroll = measure_visible_tag_note_cards(app, scroll, render_height);
    scroll = scroll
        .min(app.tag_note_vlist.geometry.max_scroll(render_height))
        .min(u16::MAX as usize);
    app.tag_note_scroll = scroll as u16;

    let end = scroll.saturating_add(render_height);
    let visible_range = app
        .tag_note_vlist
        .geometry
        .visible_range(scroll, render_height);
    let mut visible_lines = Vec::with_capacity(render_height);
    let mut visible_cards = Vec::new();
    for index in visible_range.clone() {
        let first = app.tag_note_vlist.geometry.item_top(index);
        let cached = app.tag_note_vlist.items[index]
            .cache
            .as_ref()
            .expect("visible tag-note cards are measured before rendering");
        let from = scroll.saturating_sub(first);
        let to = cached.lines.len().min(end.saturating_sub(first));
        visible_lines.extend(cached.lines[from.min(to)..to].iter().cloned());
        visible_cards.push((index, first, cached.clone()));
    }
    frame.render_widget(Paragraph::new(visible_lines), cards);

    for (index, first, cached) in &visible_cards {
        let parent = cached.path.parent().unwrap_or(app.storage.root.as_path());
        let links = cached
            .links
            .iter()
            .filter_map(|link| {
                let global_row = first + link.row;
                (global_row >= scroll && global_row < end).then(|| {
                    let mut link = link.clone();
                    link.row = global_row;
                    link
                })
            })
            .collect::<Vec<_>>();
        let tags = cached
            .tags
            .iter()
            .filter_map(|tag| {
                let global_row = first + tag.row;
                (global_row >= scroll && global_row < end).then(|| {
                    let mut tag = tag.clone();
                    tag.row = global_row;
                    tag
                })
            })
            .collect::<Vec<_>>();
        let images = cached
            .images
            .iter()
            .filter_map(|image| {
                let mut image = image.clone();
                image.row += first;
                let image_end = image.row.saturating_add(image.height);
                (image_end > scroll && image.row < end).then_some(image)
            })
            .collect::<Vec<_>>();
        if interactive {
            register_link_hitboxes(&mut app.link_hitboxes, &links, cards, scroll, parent);
            register_tag_hitboxes(&mut app.tag_hitboxes, &tags, cards, scroll);
            let card_first = (*first).max(scroll);
            let card_last = first
                .saturating_add(cached.lines.len().saturating_sub(1))
                .min(end);
            if card_last > card_first {
                app.tag_note_hitboxes.push(crate::model::TagNoteHitbox {
                    index: *index,
                    area: Rect::new(
                        cards.x,
                        cards.y + (card_first - scroll) as u16,
                        cards.width,
                        (card_last - card_first) as u16,
                    ),
                });
            }
        }
        app.images
            .render(frame, &images, cards, scroll, parent, app.theme);
    }

    if let Some(cached) = app
        .tag_note_vlist
        .items
        .get(app.tag_note_index)
        .and_then(|item| item.cache.as_ref())
    {
        let first = app.tag_note_vlist.geometry.item_top(app.tag_note_index);
        let last = first + cached.lines.len().saturating_sub(2);
        draw_selected_card_border(
            frame,
            CardBorderGeometry {
                area: cards,
                scroll,
                first,
                last,
            },
            app.animation_tick,
            app.theme,
        );
    }
}

pub(super) fn sync_tag_note_vlist(app: &mut App, width: usize) {
    let width_changed = app.tag_note_vlist.width != width;
    let same_items = !width_changed
        && app.tag_note_vlist.items.len() == app.tag_notes.len()
        && app
            .tag_note_vlist
            .items
            .iter()
            .zip(&app.tag_notes)
            .all(|(item, note)| item.path == note.path && item.modified == note.modified);
    if !same_items {
        let old_items = if width_changed {
            Vec::new()
        } else {
            std::mem::take(&mut app.tag_note_vlist.items)
        };
        let mut by_path = old_items
            .into_iter()
            .map(|item| (item.path.clone(), item))
            .collect::<HashMap<_, _>>();
        app.tag_note_vlist.items = app
            .tag_notes
            .iter()
            .map(|note| {
                by_path
                    .remove(&note.path)
                    .filter(|item| item.modified == note.modified)
                    .unwrap_or(crate::app::TagNoteVirtualItem {
                        path: note.path.clone(),
                        modified: note.modified,
                        cache: None,
                    })
            })
            .collect();
        app.tag_note_vlist.geometry = crate::vlist::VList::new(12);
        app.tag_note_vlist.geometry.resize(app.tag_notes.len());
        for (index, item) in app.tag_note_vlist.items.iter().enumerate() {
            if let Some(cache) = &item.cache {
                app.tag_note_vlist
                    .geometry
                    .set_height(index, cache.lines.len());
            }
        }
        app.tag_note_vlist.width = width;
    } else {
        app.tag_note_vlist.geometry.resize(app.tag_notes.len());
    }

    for (index, (item, note)) in app
        .tag_note_vlist
        .items
        .iter_mut()
        .zip(&app.tag_notes)
        .enumerate()
    {
        if item.cache.as_ref().is_some_and(|cached| {
            cached.width != width || cached.title != note.title || cached.body != note.body
        }) {
            item.cache = None;
            app.tag_note_vlist.geometry.invalidate(index);
        }
    }
}

pub(super) fn ensure_tag_note_card_rendered(app: &mut App, index: usize) {
    if app.tag_note_vlist.items[index].cache.is_some() {
        return;
    }
    let note = &app.tag_notes[index];
    let cached = render_tag_note_card(note, app.tag_note_vlist.width, app.theme);
    let height = cached.lines.len();
    app.tag_note_vlist.items[index].cache = Some(cached);
    app.tag_note_vlist.geometry.set_height(index, height);
}

pub(super) fn measure_visible_tag_note_cards(
    app: &mut App,
    mut scroll: usize,
    view_height: usize,
) -> usize {
    loop {
        let range = app
            .tag_note_vlist
            .geometry
            .visible_range(scroll, view_height);
        if range.is_empty() {
            return 0;
        }
        let anchor = range.start;
        let anchor_offset = scroll.saturating_sub(app.tag_note_vlist.geometry.item_top(anchor));
        let missing = range
            .clone()
            .filter(|index| !app.tag_note_vlist.geometry.is_measured(*index))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return scroll.min(app.tag_note_vlist.geometry.max_scroll(view_height));
        }
        for index in missing {
            ensure_tag_note_card_rendered(app, index);
        }
        let height = app.tag_note_vlist.geometry.height(anchor);
        scroll = app.tag_note_vlist.geometry.item_top(anchor)
            + anchor_offset.min(height.saturating_sub(1));
        scroll = scroll.min(app.tag_note_vlist.geometry.max_scroll(view_height));
    }
}

pub(super) fn render_tag_note_card(
    note: &crate::model::TagNote,
    width: usize,
    theme: Theme,
) -> crate::app::TagNoteCardRenderCache {
    let card_style = Style::default().bg(theme.surface_panel);
    let horizontal_padding = DAILY_PADDING_X.min(width.saturating_sub(1) / 2);
    let body_start = horizontal_padding + DAILY_DATE_LABEL_WIDTH + 2;
    let (body_start, body_width) = centered_daily_body_axis(width, body_start);
    let mut lines = vec![
        line_with_background(Vec::new(), width, card_style),
        line_with_background(Vec::new(), width, card_style),
        line_with_background(
            vec![
                Span::raw(" ".repeat(body_start)),
                Span::styled(
                    note.title.clone(),
                    Style::default()
                        .fg(theme.text_subtle)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ],
            width,
            card_style,
        ),
        line_with_background(Vec::new(), width, card_style),
    ];

    let markdown = crate::markdown::render_at_width(&note.body, body_width, theme);
    let body_line_start = lines.len();
    let links = markdown
        .links
        .into_iter()
        .map(|mut link| {
            link.row += body_line_start;
            link.column += body_start;
            link
        })
        .collect();
    let tags = markdown
        .tags
        .into_iter()
        .map(|mut tag| {
            tag.row += body_line_start;
            tag.column += body_start;
            tag
        })
        .collect();
    let images = markdown
        .images
        .into_iter()
        .map(|mut image| {
            image.row += body_line_start;
            image.column += body_start;
            image
        })
        .collect();
    for markdown_line in markdown.lines {
        for body in wrap_spans_to_width(&markdown_line.spans, body_width) {
            let mut spans = Vec::with_capacity(body.len() + 1);
            spans.push(Span::raw(" ".repeat(body_start)));
            spans.extend(body);
            lines.push(line_with_background(spans, width, card_style));
        }
    }
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(Line::default());
    crate::app::TagNoteCardRenderCache {
        width,
        path: note.path.clone(),
        title: note.title.clone(),
        body: note.body.clone(),
        lines,
        links,
        tags,
        images,
    }
}
