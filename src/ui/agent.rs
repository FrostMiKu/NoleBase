use std::sync::Arc;

use super::*;

pub(super) fn draw_agent_statistics(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(" Agent statistics ")
        .style(Style::default().bg(app.theme.surface_panel))
        .border_style(focus_border(app.focus == Focus::Agent, app.theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if app.ai_running {
        draw_animated_border(frame, area, app.animation_tick, app.theme);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let prompts = app
        .agent_panel
        .iter()
        .filter(|entry| {
            matches!(
                entry.as_ref(),
                crate::agent_session::AgentPanelEntry::Prompt { .. }
            )
        })
        .count();
    let replies = app
        .agent_panel
        .iter()
        .filter(|entry| {
            matches!(
                entry.as_ref(),
                crate::agent_session::AgentPanelEntry::Assistant {
                    final_output: true,
                    ..
                }
            )
        })
        .count();
    let tools = app
        .agent_panel
        .iter()
        .filter(|entry| {
            matches!(
                entry.as_ref(),
                crate::agent_session::AgentPanelEntry::Tool { .. }
            )
        })
        .count();
    let context_rate = if app.agent_context_capacity == 0 {
        0.0
    } else {
        app.agent_context_window as f64 * 100.0 / app.agent_context_capacity as f64
    };
    let cache_read = app.agent_usage.cache_read_input_tokens;
    let cache_write = app.agent_usage.cache_creation_input_tokens;
    let cache_rate = app.agent_usage.cache_hit_percent();
    let tps = if app.agent_response_duration.is_zero() {
        "--".to_string()
    } else {
        format!(
            "{:.1}",
            app.agent_timed_output_tokens as f64 / app.agent_response_duration.as_secs_f64()
        )
    };
    let context = if app.agent_context_capacity == 0 {
        "--".to_string()
    } else {
        format!(
            "{} / {}  {:.0}%",
            human_token_count(app.agent_context_window),
            human_token_count(app.agent_context_capacity),
            context_rate
        )
    };
    let state = if app.ai_running { "Working" } else { "Idle" };
    let rows = [
        ("State", state.to_string()),
        ("Context", context),
        ("Model in", human_token_count(app.agent_usage.total_input())),
        (
            "Model out",
            human_token_count(app.agent_usage.output_tokens),
        ),
        (
            "Cache",
            cache_rate.map_or_else(
                || "--".to_string(),
                |rate| {
                    format!(
                        "R {} · W {} · {rate:.0}%",
                        human_token_count(cache_read),
                        human_token_count(cache_write)
                    )
                },
            ),
        ),
        ("Stream", format!("{tps} t/s")),
        ("Turns", format!("{prompts} user · {replies} agent")),
        ("Tools", tools.to_string()),
        (
            "Round",
            if app.agent_round_limit == 0 {
                "--".to_string()
            } else {
                format!("{} / {}", app.agent_round, app.agent_round_limit)
            },
        ),
        ("Retries", app.agent_retry_count.to_string()),
    ];
    let mut lines = vec![Line::default()];
    for (label, value) in rows {
        if lines.len() >= inner.height as usize {
            break;
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label:<8}"),
                Style::default().fg(app.theme.text_muted),
            ),
            Span::styled(value, Style::default().fg(app.theme.text_primary)),
        ]));
        if lines.len() < inner.height as usize {
            lines.push(Line::default());
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

pub(super) fn draw_agent_output(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let context = if app.agent_context_window == 0 || app.agent_context_capacity == 0 {
        "Ctx --".to_string()
    } else {
        format!(
            "Ctx {}/{}",
            human_token_count(app.agent_context_window),
            human_token_count(app.agent_context_capacity)
        )
    };
    let title = if app.agent_round_limit == 0 {
        format!(" Agent · {context} ")
    } else {
        format!(
            " Agent · ↻{}/{} · {context} ",
            app.agent_round, app.agent_round_limit,
        )
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
    let tail_pinned = app.agent_follow_tail || app.agent_scroll == u16::MAX;
    let maximum = app.agent_vlist.geometry.max_scroll(view_height);
    let mut scroll = if tail_pinned {
        maximum
    } else {
        (app.agent_scroll as usize).min(maximum)
    };
    scroll = measure_visible_agent_entries(app, scroll, view_height, tail_pinned);
    evict_agent_caches(app, scroll, view_height);
    app.agent_scroll = scroll.min(u16::MAX as usize) as u16;
    if scroll >= app.agent_vlist.geometry.max_scroll(view_height) {
        app.agent_follow_tail = true;
    }
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
    sync_agent_vlist_with_style(app, width, crate::app::AgentEntryRenderStyle::Panel);
}

pub(super) fn sync_chat_vlist(app: &mut App, width: usize) {
    sync_agent_vlist_with_style(app, width, crate::app::AgentEntryRenderStyle::Cards);
}

fn sync_agent_vlist_with_style(
    app: &mut App,
    width: usize,
    style: crate::app::AgentEntryRenderStyle,
) {
    if app.agent_vlist.width != width || app.agent_vlist.style != style {
        app.agent_vlist.width = width;
        app.agent_vlist.style = style;
        app.agent_vlist.geometry = crate::vlist::VList::new(4);
        app.agent_vlist.caches.clear();
    }
    app.agent_vlist.caches.resize(app.agent_panel.len(), None);
    app.agent_vlist.geometry.resize(app.agent_panel.len());
    for (index, entry) in app.agent_panel.iter().enumerate() {
        app.agent_vlist.geometry.set_estimate(
            index,
            estimated_agent_entry_height(entry, width, style, app.show_full_thinking),
        );
    }
    for (index, cache) in app.agent_vlist.caches.iter_mut().enumerate() {
        let entry = &app.agent_panel[index];
        let volatile = is_volatile_agent_entry(entry);
        // A stable entry must always carry a render cache: its measured height
        // is only trustworthy while the cached render it was measured from is
        // still the current entry. Entries that streamed in place (a running
        // tool that completed, streamed text that finished) were volatile —
        // never cached — so `Arc::make_mut` mutates them in place without
        // changing the pointer, leaving the stale measured height from the
        // volatile phase behind unless we invalidate it here. Evicted entries
        // are already unmeasured, so invalidating again is a no-op.
        if volatile
            || cache.is_none()
            || cache
                .as_ref()
                .is_some_and(|cached| !Arc::ptr_eq(&cached.entry, entry))
        {
            *cache = None;
            app.agent_vlist.geometry.invalidate(index);
        }
    }
}

/// Entries whose rendered output changes every frame (streaming text, running tool
/// spinners) are kept out of the render cache: caching them would pin an `Arc`
/// reference on the panel entry, forcing `Arc::make_mut` to deep-copy the whole
/// text on every streamed delta.
fn is_volatile_agent_entry(entry: &Arc<crate::agent_session::AgentPanelEntry>) -> bool {
    matches!(
        entry.as_ref(),
        crate::agent_session::AgentPanelEntry::Assistant {
            streaming: true,
            ..
        } | crate::agent_session::AgentPanelEntry::Thinking {
            streaming: true,
            ..
        } | crate::agent_session::AgentPanelEntry::Tool { active: true, .. }
    )
}

/// Rough per-type height estimate used before an entry is measured. Keeps the
/// visible-range window tight so few out-of-view entries are rendered speculatively.
/// The Chat style needs its own numbers: the thinking box and plain assistant text
/// differ structurally from the panel's status rows, and underestimating clips the
/// block's outer blank row while streaming (when entries are re-rendered per frame).
pub(super) fn estimated_agent_entry_height(
    entry: &crate::agent_session::AgentPanelEntry,
    width: usize,
    style: crate::app::AgentEntryRenderStyle,
    show_full_thinking: bool,
) -> usize {
    let width = width.max(1);
    match (style, entry) {
        (
            crate::app::AgentEntryRenderStyle::Cards,
            crate::agent_session::AgentPanelEntry::Prompt { text, .. },
        ) => 3 + text.len().div_ceil(width.saturating_sub(4).max(1)),
        (
            crate::app::AgentEntryRenderStyle::Cards,
            crate::agent_session::AgentPanelEntry::Assistant { text, .. },
        ) => 1 + text.len().div_ceil(width.saturating_sub(2).max(1)),
        // Box rows: top pad + label + gap + body + bottom pad + blank.
        (
            crate::app::AgentEntryRenderStyle::Cards,
            crate::agent_session::AgentPanelEntry::Thinking { text, .. },
        ) => {
            let body_rows = text.len().div_ceil(width.saturating_sub(6).max(1));
            5 + if show_full_thinking {
                body_rows
            } else {
                body_rows.min(crate::ui::chat::DEFAULT_THINKING_BODY_ROWS)
            }
        }
        (
            crate::app::AgentEntryRenderStyle::Cards,
            crate::agent_session::AgentPanelEntry::Error(text),
        ) => text.lines().count() + 1,
        // Message blocks: top pad + label + gap + body + bottom pad + blank.
        (
            crate::app::AgentEntryRenderStyle::Panel,
            crate::agent_session::AgentPanelEntry::Prompt { text, .. }
            | crate::agent_session::AgentPanelEntry::Assistant { text, .. },
        ) => 6 + text.len().div_ceil(width),
        // The panel shows thinking as a single status row plus a trailing blank.
        (
            crate::app::AgentEntryRenderStyle::Panel,
            crate::agent_session::AgentPanelEntry::Thinking { .. },
        ) => 2,
        // Tool and error rows match across styles (activity lines + trailing blank).
        (_, crate::agent_session::AgentPanelEntry::Tool { text, preview, .. }) => {
            text.lines().count() + 2 + usize::from(preview.is_some())
        }
        (_, crate::agent_session::AgentPanelEntry::Error(_)) => 1,
    }
}

pub(super) fn ensure_agent_entry_rendered(app: &mut App, index: usize) {
    if app.agent_vlist.caches[index].is_some() {
        return;
    }
    let entry = Arc::clone(&app.agent_panel[index]);
    // Measure with the same renderer the display pass uses so volatile entry
    // heights cannot clip content or shift scroll anchors between frames.
    let (lines, links, images) = render_agent_entry_current(app, index);
    let height = lines.len();
    if !is_volatile_agent_entry(&entry) {
        app.agent_vlist.caches[index] = Some(crate::app::AgentEntryRenderCache {
            width: app.agent_vlist.width,
            entry,
            lines,
            links,
            images,
        });
    }
    app.agent_vlist.geometry.set_height(index, height);
}

/// Maximum entries rendered in one frame. Keeps a full-screen scroll (PageDown or
/// wheel burst) from stalling the frame on markdown rendering; the remainder is
/// measured on the following frames while the viewport shows estimated heights.
const MEASURE_BUDGET: usize = 16;

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
        let mut missing = range
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
        // Measure toward the anchor first: it pins the scroll position, so getting
        // it right early keeps the viewport stable while the rest fills in.
        missing.sort_by_key(|index| index.abs_diff(anchor));
        let budget = missing.len().min(MEASURE_BUDGET);
        for index in &missing[..budget] {
            ensure_agent_entry_rendered(app, *index);
        }
        if missing.len() > budget {
            // Budget exhausted: leave the remaining entries unmeasured for now and
            // return the current (clamped) scroll; the next frame continues.
            let maximum = app.agent_vlist.geometry.max_scroll(view_height);
            return if tail_pinned {
                maximum
            } else {
                scroll.min(maximum)
            };
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

/// Drop render caches (and their measurements) for entries far outside the viewport
/// window so long agent sessions do not accumulate rendered text for the whole panel.
/// The margin keeps a small scroll-back buffer so quick wheel bursts do not thrash.
const CACHE_KEEP_MARGIN: usize = 50;

pub(super) fn evict_agent_caches(app: &mut App, scroll: usize, view_height: usize) {
    let len = app.agent_panel.len();
    if len == 0 {
        return;
    }
    let range = app.agent_vlist.geometry.visible_range(scroll, view_height);
    let keep_start = range.start.saturating_sub(CACHE_KEEP_MARGIN);
    let keep_end = range.end.saturating_add(CACHE_KEEP_MARGIN).min(len);
    for index in 0..len {
        if (index < keep_start || index >= keep_end)
            && app.agent_vlist.caches[index].take().is_some()
        {
            app.agent_vlist.geometry.invalidate(index);
        }
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
            // A gap row separates the label from the message body.
            lines.push(agent_message_line(Line::default(), width, background));
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
            lines.push(Line::default());
        }
        crate::agent_session::AgentPanelEntry::Assistant { text, .. } if text.trim().is_empty() => {
            lines.push(Line::default());
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
            // A gap row separates the label from the message body.
            lines.push(agent_message_line(Line::default(), width, background));
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
            lines.push(Line::default());
        }
        crate::agent_session::AgentPanelEntry::Tool { text, active, .. } => {
            lines.extend(if *active && animate {
                animated_activity_lines(text, width, tick, theme)
            } else {
                activity_lines(text, width, theme)
            });
            lines.push(Line::default());
        }
        crate::agent_session::AgentPanelEntry::Thinking { streaming, .. } => {
            // The panel shows only a status row, not the reasoning detail: a
            // braille spinner while streaming, a check mark once done.
            let marker = if *streaming {
                animated_activity_marker(width, tick, theme)
            } else {
                Span::styled(
                    " ✓ ",
                    Style::default()
                        .fg(theme.ui_activity_marker)
                        .add_modifier(Modifier::BOLD),
                )
            };
            lines.push(Line::from(vec![
                marker,
                Span::styled("thinking", Style::default().fg(theme.text_muted)),
            ]));
            lines.push(Line::default());
        }
        crate::agent_session::AgentPanelEntry::Error(text) => lines.push(Line::from(Span::styled(
            text.clone(),
            Style::default().fg(theme.ui_error),
        ))),
    }
    (lines, links, images)
}

pub(super) fn agent_message_line(
    line: Line<'static>,
    width: usize,
    background: Color,
) -> Line<'static> {
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
        let from = scroll.saturating_sub(item_top);
        let to = height.min(end.saturating_sub(item_top));
        let content_from = from;
        if let Some(cached) = app.agent_vlist.caches[index].as_ref() {
            // Stable entry: reuse the cached render.
            let content_to = to.min(cached.lines.len());
            visible.extend(
                cached.lines[content_from.min(content_to)..content_to]
                    .iter()
                    .cloned(),
            );
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
        } else {
            // Volatile entry (streaming assistant or running tool): the sync pass
            // keeps it out of the cache, so render it fresh with the current tick.
            let (rendered_lines, rendered_links, rendered_images) =
                render_agent_entry_current(app, index);
            // The geometry height may still be an estimate when the measure
            // budget left this entry unmeasured this frame; never clip the fresh
            // render to that estimate or the block's trailing rows (body padding,
            // the shared outer blank) vanish while streaming. Slice to the real
            // rendered length instead so live rows match a fresh full render.
            let content_to = rendered_lines.len().min(end.saturating_sub(item_top));
            visible.extend(
                rendered_lines[content_from.min(content_to)..content_to]
                    .iter()
                    .cloned(),
            );
            links.extend(rendered_links.into_iter().filter_map(|link| {
                let global_row = item_top + link.row;
                (global_row >= scroll && global_row < end).then(|| {
                    let mut link = link;
                    link.row = global_row;
                    link
                })
            }));
            images.extend(rendered_images.into_iter().filter_map(|image| {
                let mut image = image;
                image.row += item_top;
                let image_end = image.row.saturating_add(image.height);
                (image_end > scroll && image.row < end).then_some(image)
            }));
        }
    }
    (visible, links, images)
}

/// Fresh render of a not-cached (volatile) agent entry, with animation enabled for
/// running tools so the spinner advances with `animation_tick`.
fn render_agent_entry_current(
    app: &App,
    index: usize,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let entry = app.agent_panel[index].as_ref();
    match app.agent_vlist.style {
        crate::app::AgentEntryRenderStyle::Panel => render_agent_entry(
            entry,
            app.agent_vlist.width,
            app.animation_tick,
            true,
            app.theme,
        ),
        crate::app::AgentEntryRenderStyle::Cards => match entry {
            crate::agent_session::AgentPanelEntry::Tool {
                text,
                active: true,
                preview,
                ..
            } => (
                render_chat_tool(
                    text,
                    preview.as_deref(),
                    app.agent_vlist.width,
                    app.animation_tick,
                    true,
                    app.theme,
                ),
                Vec::new(),
                Vec::new(),
            ),
            crate::agent_session::AgentPanelEntry::Thinking {
                text,
                streaming: true,
                ..
            } => render_chat_thinking_box(
                text,
                true,
                app.agent_vlist.width,
                app.animation_tick,
                app.show_full_thinking,
                app.theme,
            ),
            crate::agent_session::AgentPanelEntry::Assistant {
                text,
                streaming: true,
                ..
            } => render_chat_plain_text(text, app.agent_vlist.width, app.theme),
            _ => render_chat_entry_with_thinking_mode(
                entry,
                app.agent_vlist.width,
                app.show_full_thinking,
                app.theme,
            ),
        },
    }
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
    let cache_read_tokens = app.agent_usage.cache_read_input_tokens;
    let cache_write_tokens = app.agent_usage.cache_creation_input_tokens;
    let cache_read = human_token_count(cache_read_tokens);
    let cache_write = human_token_count(cache_write_tokens);
    let cache_rate = app.agent_usage.cache_hit_percent();
    let tps = if app.agent_response_duration.is_zero() {
        "--".to_string()
    } else {
        format!(
            "{:.1}",
            app.agent_timed_output_tokens as f64 / app.agent_response_duration.as_secs_f64()
        )
    };
    let tokens = format!("↑{input} ↓{output}");
    let cache_full = cache_rate.map_or_else(
        || "Cache --".to_string(),
        |rate| format!("Cache R{cache_read} W{cache_write} {rate:.0}%"),
    );
    let cache_compact = cache_rate.map_or_else(
        || "C--".to_string(),
        |rate| format!("C{cache_read}/{cache_write} {rate:.0}%"),
    );
    let full = format!("{tokens} · {tps} t/s · {cache_full}");
    let compact = format!("{tokens} · {tps}t/s · {cache_compact}");
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

/// Braille spinner frames for running tools; advanced by `animation_tick`.
/// Eight frames rotate a full braille pattern. Note: U+28xx is classified
/// Wide in UAX #11, so terminals may render it two columns while ratatui's
/// unicode-width reserves one; chosen deliberately by the user.
const BRAILLE_SPINNER_FRAMES: [char; 8] = ['⣷', '⣯', '⣟', '⡿', '⢿', '⣻', '⣽', '⣾'];

/// Spinner frame at `tick`, shared with the streaming card label.
pub(super) fn spinner_frame(tick: u64) -> char {
    BRAILLE_SPINNER_FRAMES[(tick as usize) % BRAILLE_SPINNER_FRAMES.len()]
}

fn animated_activity_marker(width: usize, tick: u64, theme: Theme) -> Span<'static> {
    let frame = spinner_frame(tick);
    let marker = match width {
        0 => String::new(),
        1 => frame.to_string(),
        2 => format!("{frame} "),
        _ => format!(" {frame} "),
    };
    Span::styled(
        marker,
        Style::default()
            .fg(theme.ui_activity_marker)
            .add_modifier(Modifier::BOLD),
    )
}

pub(super) fn animated_activity_lines(
    text: &str,
    width: usize,
    tick: u64,
    theme: Theme,
) -> Vec<Line<'static>> {
    let (status, details) = activity_parts(text);
    activity_rows(&status, &details, width, tick, true, theme)
}

pub(super) fn activity_lines(text: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (status, details) = activity_parts(text);
    activity_rows(&status, &details, width, 0, false, theme)
}

/// Shared activity rows used by both the agent panel and the chat tool block:
/// one status row (optionally animated with the braille spinner for a running
/// tool) followed by one tree row per detail, using the `├─`/`└─` convention
/// from [`activity_detail_line`]. The chat block appends a successful result
/// preview as the final detail so it shares the same glyphs and indentation.
pub(super) fn activity_rows(
    status: &str,
    details: &[String],
    width: usize,
    tick: u64,
    animate: bool,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(1 + details.len());
    if animate {
        let marker = animated_activity_marker(width, tick, theme);
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
        lines.push(Line::from(spans));
    } else {
        let marker = activity_marker(width, theme);
        let available = width.saturating_sub(marker.width());
        lines.push(Line::from(vec![
            marker,
            Span::styled(
                compact_activity_line(status, available),
                Style::default().fg(theme.text_muted),
            ),
        ]));
    }
    let detail_count = details.len();
    for (index, detail) in details.iter().enumerate() {
        lines.push(activity_detail_line(
            detail,
            width,
            theme,
            index + 1 == detail_count,
        ));
    }
    lines
}

pub(super) fn activity_parts(text: &str) -> (String, Vec<String>) {
    let mut parts = text.split('\n');
    let status = parts.next().unwrap_or_default();
    let details = parts
        .filter(|detail| !detail.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if status.starts_with("Failed ") {
        if let Some((status, error)) = status.split_once(": ") {
            let target = details.join(" · ");
            let detail = if target.is_empty() {
                error.to_string()
            } else {
                format!("{error} · {target}")
            };
            return (status.to_string(), vec![detail]);
        }
    }
    (status.to_string(), details)
}

pub(super) fn activity_detail_line(
    detail: &str,
    width: usize,
    theme: Theme,
    last: bool,
) -> Line<'static> {
    let marker = activity_detail_marker(width, theme, last);
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

pub(super) fn activity_detail_marker(width: usize, theme: Theme, last: bool) -> Span<'static> {
    let branch = if last { '└' } else { '├' };
    let marker = match width {
        0 => String::new(),
        1 => branch.to_string(),
        2 => format!("{branch}─"),
        3 => format!("{branch}─ "),
        4 => format!(" {branch}─"),
        _ => format!("   {branch}─ "),
    };
    Span::styled(marker, Style::default().fg(theme.ui_border_subtle))
}
