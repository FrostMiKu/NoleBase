use super::*;

pub(super) fn draw_daily(
    frame: &mut Frame,
    app: &mut App,
    surface: Rect,
    content: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if surface.width == 0 || surface.height == 0 {
        return;
    }
    let content = inset_horizontal(content, 2);
    if content.width == 0 || content.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Daily",
            Style::default()
                .fg(app.theme.ui_page_heading)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(content.x, content.y, content.width, 1),
    );

    let compose = compose_rect(content);
    app.layout.compose = non_empty(compose);

    let daily_top = content.y.saturating_add(2).min(content.y + content.height);
    let daily_view = Rect::new(
        content.x,
        daily_top,
        content.width,
        content
            .y
            .saturating_add(content.height)
            .saturating_sub(daily_top),
    );
    let unoccluded_height = compose
        .y
        .saturating_sub(1)
        .saturating_sub(daily_view.y)
        .min(daily_view.height);
    draw_daily_notes(frame, app, daily_view, unoccluded_height, interactive);

    if compose.width > 0 && compose.height > 0 {
        clear_widget(frame, compose);
        draw_compose(frame, app, compose, interactive, cursor_position);
    }
}

pub(super) fn draw_daily_notes(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    unoccluded_height: u16,
    interactive: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width as usize;
    sync_daily_vlist(app, width);
    if app.daily_notes.is_empty() {
        frame.render_widget(
            Paragraph::new("No notes yet").alignment(Alignment::Center),
            area,
        );
        return;
    }

    let render_height = area.height as usize;
    let view_height = unoccluded_height.min(area.height) as usize;
    let tail_pinned = app.scroll == u16::MAX;
    let mut scroll = (app.scroll as usize)
        .min(app.daily_vlist.geometry.max_scroll(view_height))
        .min(u16::MAX as usize);
    if app.reveal_selected_daily {
        if app.selected < app.daily_notes.len() {
            ensure_daily_card_rendered(app, app.selected);
            let first = app.daily_vlist.geometry.item_top(app.selected);
            let button = first
                + app.daily_vlist.items[app.selected]
                    .cache
                    .as_ref()
                    .expect("selected DailyNote was rendered")
                    .button_line;
            scroll = stable_card_scroll(scroll, first, button, view_height);
        }
        app.reveal_selected_daily = false;
    }
    scroll = measure_visible_daily_cards(app, scroll, render_height, view_height, tail_pinned);
    scroll = scroll
        .min(app.daily_vlist.geometry.max_scroll(view_height))
        .min(u16::MAX as usize);
    app.scroll = scroll as u16;

    let end = scroll.saturating_add(render_height);
    let mut visible = Vec::with_capacity(render_height);
    let mut rendered_links = Vec::new();
    let mut rendered_tags = Vec::new();
    let mut rendered_images = Vec::new();
    let visible_range = app
        .daily_vlist
        .geometry
        .visible_range(scroll, render_height);
    for index in visible_range.clone() {
        let first = app.daily_vlist.geometry.item_top(index);
        let cached = app.daily_vlist.items[index]
            .cache
            .as_ref()
            .expect("visible DailyNotes are measured before rendering");
        let from = scroll.saturating_sub(first);
        let to = cached.lines.len().min(end.saturating_sub(first));
        for (local_row, line) in cached.lines[from.min(to)..to].iter().enumerate() {
            let actual_row = from + local_row;
            if actual_row == cached.button_line {
                visible.push(daily_button_line(
                    width,
                    cached.button_start,
                    index == app.selected,
                    app.theme,
                ));
            } else {
                visible.push(line.clone());
            }
        }
        rendered_links.extend(cached.links.iter().filter_map(|link| {
            let global_row = first + link.row;
            (global_row >= scroll && global_row < end).then(|| {
                let mut link = link.clone();
                link.row = global_row;
                link
            })
        }));
        rendered_tags.extend(cached.tags.iter().filter_map(|tag| {
            let global_row = first + tag.row;
            (global_row >= scroll && global_row < end).then(|| {
                let mut tag = tag.clone();
                tag.row = global_row;
                tag
            })
        }));
        rendered_images.extend(cached.images.iter().filter_map(|image| {
            let mut image = image.clone();
            image.row += first;
            let image_end = image.row.saturating_add(image.height);
            (image_end > scroll && image.row < end).then_some(image)
        }));
    }

    if interactive {
        let interactive_area = Rect::new(area.x, area.y, area.width, unoccluded_height);
        register_link_hitboxes(
            &mut app.link_hitboxes,
            &rendered_links,
            interactive_area,
            scroll,
            &app.storage.daily_dir,
        );
        register_tag_hitboxes(
            &mut app.tag_hitboxes,
            &rendered_tags,
            interactive_area,
            scroll,
        );
        for index in visible_range {
            let first = app.daily_vlist.geometry.item_top(index);
            let cached = app.daily_vlist.items[index]
                .cache
                .as_ref()
                .expect("visible DailyNotes are rendered");
            let button_line = first + cached.button_line;
            if button_line < scroll || button_line >= scroll.saturating_add(view_height) {
                continue;
            }
            let y = area.y + (button_line - scroll) as u16;
            register_buttons_clipped(
                &mut app.hitboxes,
                app.daily_notes[index].date,
                area.x.saturating_add(cached.button_start as u16),
                y,
                area,
            );
        }
    }

    frame.render_widget(Paragraph::new(visible), area);
    let image_base = app.storage.daily_dir.clone();
    app.images.render(
        frame,
        &rendered_images,
        area,
        scroll,
        &image_base,
        app.theme,
    );
    if let Some(cached) = app
        .daily_vlist
        .items
        .get(app.selected)
        .and_then(|item| item.cache.as_ref())
    {
        let first = app.daily_vlist.geometry.item_top(app.selected);
        let last = first + cached.lines.len().saturating_sub(2);
        draw_selected_card_border(
            frame,
            area,
            scroll,
            first,
            last,
            app.animation_tick,
            app.theme,
        );
    }
}

pub(super) fn sync_daily_vlist(app: &mut App, width: usize) {
    let width_changed = app.daily_vlist.width != width;
    let same_items = !width_changed
        && app.daily_vlist.items.len() == app.daily_notes.len()
        && app
            .daily_vlist
            .items
            .iter()
            .zip(&app.daily_notes)
            .all(|(item, note)| item.date == note.date);
    if !same_items {
        let old_items = if width_changed {
            Vec::new()
        } else {
            std::mem::take(&mut app.daily_vlist.items)
        };
        let mut by_date = old_items
            .into_iter()
            .map(|item| (item.date, item))
            .collect::<HashMap<_, _>>();
        app.daily_vlist.items = app
            .daily_notes
            .iter()
            .map(|note| {
                by_date
                    .remove(&note.date)
                    .unwrap_or(crate::app::DailyVirtualItem {
                        date: note.date,
                        cache: None,
                    })
            })
            .collect();
        app.daily_vlist.geometry = crate::vlist::VList::new(12);
        app.daily_vlist.geometry.resize(app.daily_notes.len());
        for (index, item) in app.daily_vlist.items.iter().enumerate() {
            if let Some(cache) = &item.cache {
                app.daily_vlist
                    .geometry
                    .set_height(index, cache.lines.len());
            }
        }
        app.daily_vlist.width = width;
    } else {
        app.daily_vlist.geometry.resize(app.daily_notes.len());
    }

    for (index, (item, note)) in app
        .daily_vlist
        .items
        .iter_mut()
        .zip(&app.daily_notes)
        .enumerate()
    {
        let date_label = note.date.format(DATE_FMT).to_string();
        if item.cache.as_ref().is_some_and(|cached| {
            cached.width != width || cached.date_label != date_label || cached.body != note.body
        }) {
            item.cache = None;
            app.daily_vlist.geometry.invalidate(index);
        }
    }
}

pub(super) fn ensure_daily_card_rendered(app: &mut App, index: usize) {
    if app.daily_vlist.items[index].cache.is_some() {
        return;
    }
    let date_label = app.daily_notes[index].date.format(DATE_FMT).to_string();
    let cached = render_daily_note(
        &app.daily_notes[index],
        date_label,
        app.daily_vlist.width,
        app.theme,
    );
    let height = cached.lines.len();
    app.daily_vlist.items[index].cache = Some(cached);
    app.daily_vlist.geometry.set_height(index, height);
}

pub(super) fn measure_visible_daily_cards(
    app: &mut App,
    mut scroll: usize,
    render_height: usize,
    scroll_view_height: usize,
    tail_pinned: bool,
) -> usize {
    loop {
        let range = app
            .daily_vlist
            .geometry
            .visible_range(scroll, render_height);
        if range.is_empty() {
            return 0;
        }
        let anchor = range.start;
        let anchor_offset = scroll.saturating_sub(app.daily_vlist.geometry.item_top(anchor));
        let missing = range
            .clone()
            .filter(|index| !app.daily_vlist.geometry.is_measured(*index))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            let maximum = app.daily_vlist.geometry.max_scroll(scroll_view_height);
            return if tail_pinned {
                maximum
            } else {
                scroll.min(maximum)
            };
        }
        for index in missing {
            ensure_daily_card_rendered(app, index);
        }
        scroll = if tail_pinned {
            app.daily_vlist.geometry.max_scroll(scroll_view_height)
        } else {
            let height = app.daily_vlist.geometry.height(anchor);
            app.daily_vlist.geometry.item_top(anchor) + anchor_offset.min(height.saturating_sub(1))
        };
        scroll = scroll.min(app.daily_vlist.geometry.max_scroll(scroll_view_height));
    }
}

pub(super) fn render_daily_note(
    note: &crate::model::DailyNote,
    date_label: String,
    width: usize,
    theme: Theme,
) -> crate::app::DailyCardRenderCache {
    let card_style = Style::default().bg(theme.surface_panel);
    let horizontal_padding = DAILY_PADDING_X.min(width.saturating_sub(1) / 2);
    let body_start = horizontal_padding + UnicodeWidthStr::width(date_label.as_str()) + 2;
    let (body_start, body_width) = centered_daily_body_axis(width, body_start);
    let mut lines = vec![
        line_with_background(Vec::new(), width, card_style),
        line_with_background(Vec::new(), width, card_style),
        line_with_background(
            vec![
                Span::raw(" ".repeat(body_start)),
                Span::styled(
                    date_label.clone(),
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
    let button_start = width
        .saturating_sub(body_start)
        .saturating_sub(action_buttons_width());
    let button_line = lines.len();
    lines.push(daily_button_line(width, button_start, false, theme));
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(Line::default());
    crate::app::DailyCardRenderCache {
        width,
        date: note.date,
        date_label,
        body: note.body.clone(),
        lines,
        links,
        tags,
        images,
        button_line,
        button_start,
    }
}

pub(super) fn daily_button_line(
    width: usize,
    button_start: usize,
    selected: bool,
    theme: Theme,
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(button_start))];
    spans.extend(render_button_line(selected, theme).spans);
    line_with_background(spans, width, Style::default().bg(theme.surface_panel))
}

pub(super) fn draw_selected_card_border(
    frame: &mut Frame,
    area: Rect,
    scroll: usize,
    first: usize,
    last: usize,
    tick: u64,
    theme: Theme,
) {
    draw_animated_card_border(
        frame,
        area,
        scroll,
        first,
        last,
        tick,
        theme,
        theme.surface_panel,
    );
}

pub(super) fn draw_animated_card_border(
    frame: &mut Frame,
    area: Rect,
    scroll: usize,
    first: usize,
    last: usize,
    tick: u64,
    theme: Theme,
    background: Color,
) {
    if area.width < 2 || first > last {
        return;
    }
    let visible_end = scroll.saturating_add(area.height as usize);
    let start = first.max(scroll);
    let end = last.min(visible_end.saturating_sub(1));
    if start > end {
        return;
    }
    let left = area.x;
    let right = area.x + area.width - 1;
    let width = area.width as usize;
    let height = last.saturating_sub(first).saturating_add(1);
    for row in start..=end {
        let y = area.y + (row - scroll) as u16;
        if row == first {
            for x in left..=right {
                let symbol = if x == left {
                    "┌"
                } else if x == right {
                    "┐"
                } else {
                    "─"
                };
                let position = (x - left) as usize;
                frame.buffer_mut()[(x, y)].set_symbol(symbol).set_style(
                    animated_card_border_style(position, tick, theme, background),
                );
            }
        } else if row == last {
            for x in left..=right {
                let symbol = if x == left {
                    "└"
                } else if x == right {
                    "┘"
                } else {
                    "─"
                };
                let position = width
                    .saturating_add(height.saturating_sub(1))
                    .saturating_add((right - x) as usize);
                frame.buffer_mut()[(x, y)].set_symbol(symbol).set_style(
                    animated_card_border_style(position, tick, theme, background),
                );
            }
        } else {
            let right_position = width.saturating_add(row.saturating_sub(first + 1));
            let left_position = width
                .saturating_add(height.saturating_sub(1))
                .saturating_add(width.saturating_sub(1))
                .saturating_add(last.saturating_sub(row + 1));
            frame.buffer_mut()[(left, y)]
                .set_symbol("│")
                .set_style(animated_card_border_style(
                    left_position,
                    tick,
                    theme,
                    background,
                ));
            frame.buffer_mut()[(right, y)]
                .set_symbol("│")
                .set_style(animated_card_border_style(
                    right_position,
                    tick,
                    theme,
                    background,
                ));
        }
    }
}

pub(super) fn animated_card_border_style(
    position: usize,
    tick: u64,
    theme: Theme,
    background: Color,
) -> Style {
    Style::default()
        .fg(animated_color(position, tick, theme))
        .bg(background)
}

pub(super) fn stable_card_scroll(
    scroll: usize,
    first: usize,
    button: usize,
    view_height: usize,
) -> usize {
    let card_height = button.saturating_sub(first).saturating_add(1);
    if card_height <= view_height {
        if first < scroll {
            first
        } else if button >= scroll.saturating_add(view_height) {
            button.saturating_sub(view_height.saturating_sub(1))
        } else {
            scroll
        }
    } else {
        let last_start = button.saturating_sub(view_height.saturating_sub(1));
        scroll.clamp(first, last_start)
    }
}

pub(super) fn centered_daily_body_axis(width: usize, desired_start: usize) -> (usize, usize) {
    let start = desired_start.min(width.saturating_sub(1));
    let trailing = start.min(width.saturating_sub(start).saturating_sub(1));
    (start, width.saturating_sub(start + trailing).max(1))
}

pub(super) fn action_buttons_width() -> usize {
    Action::all()
        .iter()
        .map(|action| action.label().width() + 2)
        .sum::<usize>()
        + Action::all().len().saturating_sub(1)
}

pub(super) fn render_button_line(selected: bool, theme: Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, action) in Action::all().iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if *action == Action::Ai {
            Style::default()
                .fg(theme.text_on_accent)
                .bg(theme.ui_action_ai)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default()
                .fg(theme.text_on_accent)
                .bg(theme.ui_action)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.ui_action)
        };
        spans.push(Span::styled(format!("[{}]", action.label()), style));
    }
    Line::from(spans)
}

pub(super) fn register_buttons_clipped(
    hitboxes: &mut Vec<ButtonHitbox>,
    date: NaiveDate,
    start_x: u16,
    y: u16,
    viewport: Rect,
) {
    if y < viewport.y || y >= viewport.y.saturating_add(viewport.height) {
        return;
    }
    let right = viewport.x.saturating_add(viewport.width);
    let mut x = start_x;
    for (index, action) in Action::all().iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(1);
        }
        let width = action.label().width() as u16 + 2;
        let clipped_x = x.max(viewport.x);
        let clipped_right = x.saturating_add(width).min(right);
        if clipped_x < clipped_right {
            hitboxes.push(ButtonHitbox {
                date,
                action: *action,
                area: Rect::new(clipped_x, y, clipped_right - clipped_x, 1),
            });
        }
        x = x.saturating_add(width);
        if x >= right {
            break;
        }
    }
}
