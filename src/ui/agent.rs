use super::*;

pub(super) fn draw_agent_output(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let title = if app.agent_round_limit == 0 {
        " Agent ".to_string()
    } else {
        format!(" Agent · ↻{}/{} ", app.agent_round, app.agent_round_limit,)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(title)
        .style(Style::default().bg(app.theme.surface_panel))
        .border_style(focus_border(app.focus == Focus::Agent, app.theme));
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if app.ai_running {
        draw_animated_border(frame, area, app.animation_tick, app.theme);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if let Some(stats) = agent_stats_line(app, inner.width) {
        frame.render_widget(
            Paragraph::new(stats),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        inner.y = inner.y.saturating_add(1);
        inner.height = inner.height.saturating_sub(1);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if app.agent_panel.is_empty() {
        app.agent_vlist.caches.clear();
        app.agent_vlist.geometry.resize(0);
        return;
    }
    let width = inner.width as usize;
    let view_height = inner.height as usize;
    sync_agent_vlist(app, width);
    let tail_pinned = app.agent_scroll == u16::MAX;
    let mut scroll =
        (app.agent_scroll as usize).min(app.agent_vlist.geometry.max_scroll(view_height));
    scroll = measure_visible_agent_entries(app, scroll, view_height, tail_pinned);
    app.agent_scroll = scroll.min(u16::MAX as usize) as u16;
    let (visible, rendered_links, rendered_images) = visible_agent_lines(app, scroll, view_height);
    let message_rows = Rect::new(
        area.x.saturating_add(1),
        inner.y,
        area.width.saturating_sub(2),
        inner.height,
    );
    fill_agent_message_rows(frame, message_rows, &visible);
    frame.render_widget(Paragraph::new(visible), inner);
    let image_base = app.storage.root.clone();
    app.images.render(
        frame,
        &rendered_images,
        inner,
        scroll,
        &image_base,
        app.theme,
    );
    register_link_hitboxes(
        &mut app.link_hitboxes,
        &rendered_links,
        inner,
        scroll,
        &image_base,
    );
}

pub(super) fn sync_agent_vlist(app: &mut App, width: usize) {
    if app.agent_vlist.width != width {
        app.agent_vlist.width = width;
        app.agent_vlist.geometry = crate::vlist::VList::new(4);
        app.agent_vlist.caches.clear();
    }
    app.agent_vlist.caches.resize(app.agent_panel.len(), None);
    app.agent_vlist.geometry.resize(app.agent_panel.len());
    for (index, cache) in app.agent_vlist.caches.iter_mut().enumerate() {
        if cache
            .as_ref()
            .is_some_and(|cached| cached.entry != app.agent_panel[index])
        {
            *cache = None;
            app.agent_vlist.geometry.invalidate(index);
        }
    }
}

pub(super) fn ensure_agent_entry_rendered(app: &mut App, index: usize) {
    if app.agent_vlist.caches[index].is_some() {
        return;
    }
    let entry = app.agent_panel[index].clone();
    let (lines, links, images) = render_agent_entry(
        &entry,
        app.agent_vlist.width,
        app.animation_tick,
        false,
        app.theme,
    );
    let height = lines.len();
    app.agent_vlist.caches[index] = Some(crate::app::AgentEntryRenderCache {
        width: app.agent_vlist.width,
        entry,
        lines,
        links,
        images,
    });
    app.agent_vlist.geometry.set_height(index, height);
}

pub(super) fn measure_visible_agent_entries(
    app: &mut App,
    mut scroll: usize,
    view_height: usize,
    tail_pinned: bool,
) -> usize {
    loop {
        let range = app.agent_vlist.geometry.visible_range(scroll, view_height);
        if range.is_empty() {
            return 0;
        }
        let anchor = range.start;
        let anchor_offset = scroll.saturating_sub(app.agent_vlist.geometry.item_top(anchor));
        let missing = range
            .clone()
            .filter(|index| !app.agent_vlist.geometry.is_measured(*index))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            let maximum = app.agent_vlist.geometry.max_scroll(view_height);
            return if tail_pinned {
                maximum
            } else {
                scroll.min(maximum)
            };
        }
        for index in missing {
            ensure_agent_entry_rendered(app, index);
        }
        scroll = if tail_pinned {
            app.agent_vlist.geometry.max_scroll(view_height)
        } else {
            let height = app.agent_vlist.geometry.height(anchor);
            app.agent_vlist.geometry.item_top(anchor) + anchor_offset.min(height.saturating_sub(1))
        };
        scroll = scroll.min(app.agent_vlist.geometry.max_scroll(view_height));
    }
}

pub(super) fn render_agent_entry(
    entry: &crate::agent_session::AgentPanelEntry,
    width: usize,
    tick: u64,
    animate: bool,
    theme: Theme,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let mut lines = Vec::new();
    let mut links = Vec::new();
    let mut images = Vec::new();
    match entry {
        crate::agent_session::AgentPanelEntry::Prompt { text, muted } => {
            let background = theme.surface_message_user;
            lines.push(agent_message_line(Line::default(), width, background));
            lines.push(agent_message_line(
                Line::from(Span::styled(
                    "User",
                    Style::default()
                        .fg(if *muted {
                            theme.text_muted
                        } else {
                            theme.ui_agent_user
                        })
                        .add_modifier(Modifier::BOLD),
                )),
                width,
                background,
            ));
            let row = lines.len();
            let mut rendered = crate::markdown::render_at_width(text, width, theme);
            if *muted {
                for line in &mut rendered.lines {
                    for span in &mut line.spans {
                        span.style = span.style.fg(theme.text_muted);
                    }
                }
            }
            links.extend(rendered.links.into_iter().map(|mut link| {
                link.row += row;
                link
            }));
            images.extend(rendered.images.into_iter().map(|mut image| {
                image.row += row;
                image
            }));
            lines.extend(
                rendered
                    .lines
                    .into_iter()
                    .map(|line| agent_message_line(line, width, background)),
            );
            lines.push(agent_message_line(Line::default(), width, background));
        }
        crate::agent_session::AgentPanelEntry::Assistant { text, .. } => {
            let background = theme.surface_message_agent;
            lines.push(agent_message_line(Line::default(), width, background));
            lines.push(agent_message_line(
                Line::from(Span::styled(
                    "Agent",
                    Style::default()
                        .fg(theme.ui_agent_assistant)
                        .add_modifier(Modifier::BOLD),
                )),
                width,
                background,
            ));
            let row = lines.len();
            let rendered = crate::markdown::render_at_width(text, width, theme);
            links.extend(rendered.links.into_iter().map(|mut link| {
                link.row += row;
                link
            }));
            images.extend(rendered.images.into_iter().map(|mut image| {
                image.row += row;
                image
            }));
            lines.extend(
                rendered
                    .lines
                    .into_iter()
                    .map(|line| agent_message_line(line, width, background)),
            );
            lines.push(agent_message_line(Line::default(), width, background));
        }
        crate::agent_session::AgentPanelEntry::Tool { text, active } => {
            lines.extend(if *active && animate {
                animated_activity_lines(text, width, tick, theme)
            } else {
                activity_lines(text, width, theme)
            });
        }
        crate::agent_session::AgentPanelEntry::Error(text) => lines.push(Line::from(Span::styled(
            text.clone(),
            Style::default().fg(theme.ui_error),
        ))),
    }
    (lines, links, images)
}

pub(super) fn agent_message_line(line: Line<'static>, width: usize, background: Color) -> Line<'static> {
    let mut line = line_with_background(line.spans, width, Style::default().bg(background));
    line.style = line.style.bg(background);
    line
}

pub(super) fn fill_agent_message_rows(frame: &mut Frame, area: Rect, lines: &[Line<'_>]) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for (offset, line) in lines.iter().take(area.height as usize).enumerate() {
        let Some(background) = line.style.bg else {
            continue;
        };
        let y = area.y.saturating_add(offset as u16);
        for x in area.x..area.x.saturating_add(area.width) {
            frame.buffer_mut()[(x, y)]
                .set_symbol(" ")
                .set_style(Style::default().bg(background));
        }
    }
}

pub(super) fn visible_agent_lines(
    app: &mut App,
    scroll: usize,
    view_height: usize,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let end = scroll.saturating_add(view_height);
    let mut visible = Vec::with_capacity(view_height);
    let mut links = Vec::new();
    let mut images = Vec::new();
    let range = app.agent_vlist.geometry.visible_range(scroll, view_height);
    for index in range {
        let item_top = app.agent_vlist.geometry.item_top(index);
        let height = app.agent_vlist.geometry.height(index);
        let cached = app.agent_vlist.caches[index]
            .as_ref()
            .expect("visible Agent entries are measured before rendering");
        let active = matches!(
            cached.entry,
            crate::agent_session::AgentPanelEntry::Tool { active: true, .. }
        );
        let from = scroll.saturating_sub(item_top);
        let to = height.min(end.saturating_sub(item_top));
        let content_from = from;
        let content_to = to.min(cached.lines.len());
        if active {
            let animated = render_agent_entry(
                &cached.entry,
                cached.width,
                app.animation_tick,
                true,
                app.theme,
            )
            .0;
            visible.extend(
                animated[content_from.min(content_to)..content_to]
                    .iter()
                    .cloned(),
            );
        } else {
            visible.extend(
                cached.lines[content_from.min(content_to)..content_to]
                    .iter()
                    .cloned(),
            );
        }
        links.extend(cached.links.iter().filter_map(|link| {
            let global_row = item_top + link.row;
            (global_row >= scroll && global_row < end).then(|| {
                let mut link = link.clone();
                link.row = global_row;
                link
            })
        }));
        images.extend(cached.images.iter().filter_map(|image| {
            let mut image = image.clone();
            image.row += item_top;
            let image_end = image.row.saturating_add(image.height);
            (image_end > scroll && image.row < end).then_some(image)
        }));
    }
    (visible, links, images)
}

pub(super) fn agent_stats_line(app: &App, width: u16) -> Option<Line<'static>> {
    if app.agent_usage.is_empty() && app.agent_retry_count == 0 {
        return None;
    }
    if app.agent_usage.is_empty() {
        return Some(Line::from(Span::styled(
            format!("Retry {}", app.agent_retry_count),
            Style::default().fg(app.theme.text_subtle),
        )));
    }
    let input = human_token_count(app.agent_usage.total_input());
    let output = human_token_count(app.agent_usage.output_tokens);
    let cache_read = human_token_count(app.agent_usage.cache_read_input_tokens);
    let cache_rate = if app.agent_usage.total_input() == 0 {
        0.0
    } else {
        app.agent_usage.cache_read_input_tokens as f64 * 100.0
            / app.agent_usage.total_input() as f64
    };
    let tps = if app.agent_response_duration.is_zero() {
        "--".to_string()
    } else {
        format!(
            "{:.1}",
            app.agent_timed_output_tokens as f64 / app.agent_response_duration.as_secs_f64()
        )
    };
    let tokens = format!("↑{input} ↓{output}");
    let full = format!("{tokens} · {tps} t/s · Cache {cache_read} {cache_rate:.0}%");
    let compact = format!("{tokens} · {tps}t/s · C{cache_read} {cache_rate:.0}%");
    let candidates = if app.agent_retry_count > 0 {
        vec![
            format!("{full} · Retry {}", app.agent_retry_count),
            format!("{compact} · R{}", app.agent_retry_count),
            format!("{tokens} · R{}", app.agent_retry_count),
            tokens,
        ]
    } else {
        vec![full, compact, tokens]
    };
    let text = candidates
        .into_iter()
        .find(|candidate| candidate.width() <= usize::from(width))
        .unwrap_or_default();
    Some(Line::from(Span::styled(
        text,
        Style::default().fg(app.theme.text_subtle),
    )))
}

pub(super) fn human_token_count(tokens: u64) -> String {
    let (value, suffix) = if tokens >= 1_000_000 {
        (tokens as f64 / 1_000_000.0, "m")
    } else if tokens >= 1_000 {
        (tokens as f64 / 1_000.0, "k")
    } else {
        return tokens.to_string();
    };
    let formatted = format!("{value:.1}");
    format!("{}{suffix}", formatted.trim_end_matches(".0"))
}

pub(super) fn animated_activity_lines(
    text: &str,
    width: usize,
    tick: u64,
    theme: Theme,
) -> Vec<Line<'static>> {
    let (status, detail) = activity_parts(text);
    let marker = activity_marker(width, theme);
    let available = width.saturating_sub(marker.width());
    let characters = compact_activity_line(status, available)
        .chars()
        .filter(|character| UnicodeWidthChar::width(*character).unwrap_or(0) > 0)
        .collect::<Vec<_>>();
    let mut spans = vec![marker];
    spans.extend(
        characters
            .into_iter()
            .enumerate()
            .map(|(index, character)| {
                Span::styled(
                    character.to_string(),
                    Style::default()
                        .fg(animated_color(index * 8, tick, theme))
                        .add_modifier(Modifier::BOLD),
                )
            })
            .collect::<Vec<_>>(),
    );
    let mut lines = vec![Line::from(spans)];
    if let Some(detail) = detail {
        lines.push(activity_detail_line(detail, width, theme));
    }
    lines
}

pub(super) fn activity_lines(text: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (status, detail) = activity_parts(text);
    let marker = activity_marker(width, theme);
    let available = width.saturating_sub(marker.width());
    let spans = vec![
        marker,
        Span::styled(
            compact_activity_line(status, available),
            Style::default().fg(theme.text_muted),
        ),
    ];
    let mut lines = vec![Line::from(spans)];
    if let Some(detail) = detail {
        lines.push(activity_detail_line(detail, width, theme));
    }
    lines
}

pub(super) fn activity_parts(text: &str) -> (&str, Option<&str>) {
    let (status, detail) = text.split_once('\n').unwrap_or((text, ""));
    (status, (!detail.is_empty()).then_some(detail))
}

pub(super) fn activity_detail_line(detail: &str, width: usize, theme: Theme) -> Line<'static> {
    let marker = activity_detail_marker(width, theme);
    let available = width.saturating_sub(marker.width());
    Line::from(vec![
        marker,
        Span::styled(
            compact_activity_line(detail, available),
            Style::default().fg(theme.text_muted),
        ),
    ])
}

pub(super) fn compact_activity_line(text: &str, width: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if UnicodeWidthStr::width(text.as_str()) <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }
    let content_width = width - 1;
    let mut result = String::new();
    let mut used = 0usize;
    for character in text.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used.saturating_add(character_width) > content_width {
            break;
        }
        result.push(character);
        used = used.saturating_add(character_width);
    }
    result.push('…');
    result
}

pub(super) fn activity_marker(width: usize, theme: Theme) -> Span<'static> {
    let marker = match width {
        0 => "",
        1 => "•",
        2 => "• ",
        _ => " • ",
    };
    Span::styled(
        marker,
        Style::default()
            .fg(theme.ui_activity_marker)
            .add_modifier(Modifier::BOLD),
    )
}

pub(super) fn activity_detail_marker(width: usize, theme: Theme) -> Span<'static> {
    let marker = match width {
        0 => "",
        1 => "└",
        2 => "└─",
        3 => "└─ ",
        4 => " └─",
        _ => "   └─ ",
    };
    Span::styled(marker, Style::default().fg(theme.ui_border_subtle))
}

