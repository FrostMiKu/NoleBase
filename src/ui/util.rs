use super::*;

pub(super) fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// Draw the shared filter header used by the search and tags views: a bordered
/// single-line input centered at the top of `content`, followed by the list
/// area below it. Returns the list area.
#[allow(clippy::too_many_arguments)] // mirrors the existing view renderer signature (frame, app, area, interactive, cursor)
pub(super) fn draw_filter_header(
    frame: &mut Frame,
    content: Rect,
    app: &App,
    title: String,
    prefix: &str,
    query: &str,
    cursor: usize,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) -> Rect {
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
    let show_cursor = app.focus == Focus::Center && interactive;
    if input_height >= 3 {
        clear_widget(frame, input_box);
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .title(title)
            .style(input_style)
            .border_style(focus_border(app.focus == Focus::Center, app.theme));
        let input = block.inner(input_box);
        frame.render_widget(block, input_box);
        if let Some(position) =
            draw_single_line_input(frame, input, prefix, query, cursor, show_cursor, app.theme)
        {
            *cursor_position = Some(position);
        }
    } else if let Some(position) = draw_single_line_input(
        frame,
        input_box,
        prefix,
        query,
        cursor,
        show_cursor,
        app.theme,
    ) {
        *cursor_position = Some(position);
    }
    let list_y = input_box
        .y
        .saturating_add(input_box.height)
        .saturating_add(1);
    Rect::new(
        content.x,
        list_y,
        content.width,
        content
            .y
            .saturating_add(content.height)
            .saturating_sub(list_y),
    )
}

/// Clear a widget's rectangle while removing any wide-character continuation
/// cell from the content underneath it. Ratatui's diff buffer can otherwise
/// miss the cell next to a one-column border when a CJK glyph straddles that
/// boundary.
pub(super) fn clear_widget(frame: &mut Frame, area: Rect) {
    sanitize_floating_widget_sides(frame, area);
    frame.render_widget(Clear, area);
}

/// Erase only wide glyphs that actually cross an opaque widget's vertical
/// boundary. Continuation cells are deliberately left to Ratatui's normal
/// wide-cell diff instead of being emitted with their reset background.
pub(super) fn sanitize_floating_widget_sides(frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let buffer = frame.buffer_mut();
    let bounds = buffer.area;
    let inside_right = area.x.saturating_add(area.width).saturating_sub(1);
    let bottom = area.y.saturating_add(area.height);

    for y in area.y..bottom {
        if area.x > bounds.x {
            clear_wide_cell(buffer, bounds, area.x - 1, y);
        }
        if inside_right != area.x {
            clear_wide_cell(buffer, bounds, inside_right, y);
        }
    }
}

pub(super) fn clear_wide_cell(buffer: &mut Buffer, bounds: Rect, x: u16, y: u16) {
    let in_bounds = x >= bounds.x
        && y >= bounds.y
        && x < bounds.x.saturating_add(bounds.width)
        && y < bounds.y.saturating_add(bounds.height);
    if !in_bounds {
        return;
    }
    let cell = &mut buffer[(x, y)];
    if cell.symbol().width() > 1 {
        cell.set_symbol(" ").set_diff_option(CellDiffOption::None);
    }
}

/// Prevent Ratatui's VS16-specific diff path from emitting reset-style
/// continuation cells. Crossterm can otherwise paint a default-background
/// block after the emoji and shift the remainder of that terminal row.
pub(super) fn skip_vs16_continuation_cells(buffer: &mut Buffer) {
    let area = buffer.area;
    let right = area.x.saturating_add(area.width);
    let bottom = area.y.saturating_add(area.height);

    for y in area.y..bottom {
        for x in area.x..right {
            let (cell_width, needs_skip) = {
                let cell = &buffer[(x, y)];
                let symbol = cell.symbol();
                (
                    UnicodeWidthStr::width(symbol)
                        .max(1)
                        .min((right - x) as usize),
                    symbol.contains('\u{fe0f}')
                        && matches!(
                            cell.diff_option,
                            CellDiffOption::None | CellDiffOption::AlwaysUpdate
                        ),
                )
            };
            if needs_skip && cell_width > 1 {
                for offset in 1..cell_width {
                    buffer[(x + offset as u16, y)].set_diff_option(CellDiffOption::Skip);
                }
            }
        }
    }
}

pub(super) fn center_content_axis(area: Rect) -> Rect {
    let width = area.width.min(CENTER_MAX_WIDTH);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y,
        width,
        area.height,
    )
}

pub(super) fn non_empty(area: Rect) -> Option<Rect> {
    (area.width > 0 && area.height > 0).then_some(area)
}

pub(super) fn inset_horizontal(area: Rect, padding: u16) -> Rect {
    let left = padding.min(area.width);
    let right = padding.min(area.width.saturating_sub(left));
    Rect::new(
        area.x.saturating_add(left),
        area.y,
        area.width.saturating_sub(left).saturating_sub(right),
        area.height,
    )
}

pub(super) fn shared_selection_area(container: Rect, item_y: u16, item_height: u16) -> Rect {
    let selection_y = item_y.saturating_sub(1).max(container.y);
    let selection_end = item_y
        .saturating_add(item_height)
        .min(container.y.saturating_add(container.height));
    Rect::new(
        container.x,
        selection_y,
        container.width,
        selection_end.saturating_sub(selection_y),
    )
}

/// Geometry for one row in a selectable list.
///
/// Selectable lists share a one-row breathing space before their first item.
/// The selected area starts in that space and extends through the item, while
/// adjacent rows share their boundary row. Keeping this geometry in one place
/// prevents individual list renderers from drifting apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelectionRow {
    pub(super) index: usize,
    pub(super) item_area: Rect,
    pub(super) selection_area: Option<Rect>,
}

pub(super) fn selection_rows(
    container: Rect,
    item_height: u16,
    start: usize,
    visible: usize,
    total: usize,
    selected: usize,
) -> Vec<SelectionRow> {
    let item_height = item_height.max(1);
    let end = container.y.saturating_add(container.height);
    let selected = selected.min(total.saturating_sub(1));
    (start.min(total)..total)
        .take(visible)
        .enumerate()
        .filter_map(|(row, index)| {
            let y = selection_item_y(container, row, item_height);
            if y >= end {
                return None;
            }
            let height = item_height.min(end.saturating_sub(y));
            let item_area = Rect::new(container.x, y, container.width, height);
            let selection_area =
                (index == selected).then(|| shared_selection_area(container, y, height));
            Some(SelectionRow {
                index,
                item_area,
                selection_area,
            })
        })
        .collect()
}

pub(super) fn render_selection_background(frame: &mut Frame, row: SelectionRow, style: Style) {
    let Some(area) = row.selection_area else {
        return;
    };
    frame.render_widget(Block::default().style(style), area);
}

pub(super) fn selection_styles(selected: bool, theme: Theme) -> (Style, Style) {
    if selected {
        let style = Style::default()
            .fg(theme.selection_foreground)
            .bg(theme.selection_background);
        (style, style.add_modifier(Modifier::DIM))
    } else {
        (Style::default(), Style::default().fg(theme.text_muted))
    }
}

pub(super) fn render_label_metadata_row(
    frame: &mut Frame,
    area: Rect,
    label: String,
    metadata: String,
    label_style: Style,
    metadata_style: Style,
) {
    let metadata_width = UnicodeWidthStr::width(metadata.as_str()).min(area.width as usize);
    let label_width = (area.width as usize).saturating_sub(metadata_width.saturating_add(1));
    if label_width > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(label, label_style)),
            Rect::new(area.x, area.y, label_width as u16, 1),
        );
    }
    if metadata_width > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(metadata, metadata_style)).alignment(Alignment::Right),
            Rect::new(
                area.x + area.width.saturating_sub(metadata_width as u16),
                area.y,
                metadata_width as u16,
                1,
            ),
        );
    }
}

pub(super) fn draw_selection_indicator(frame: &mut Frame, area: Rect, theme: Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for y in area.y..area.y.saturating_add(area.height) {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "▌",
                Style::default()
                    .fg(theme.selection_indicator)
                    .remove_modifier(Modifier::BOLD | Modifier::DIM),
            )),
            Rect::new(area.x, y, 1, 1),
        );
    }
}

pub(super) fn selection_list_height(item_count: u16, item_height: u16) -> u16 {
    if item_count == 0 {
        0
    } else {
        1_u16.saturating_add(item_count.saturating_mul(item_height))
    }
}

pub(super) fn visible_selection_items(list_height: u16, item_height: u16) -> usize {
    let item_height = usize::from(item_height.max(1));
    (list_height.saturating_sub(1) as usize).div_ceil(item_height)
}

pub(super) fn selection_viewport_start(
    start: usize,
    selected: usize,
    visible: usize,
    total: usize,
) -> usize {
    if visible == 0 || total == 0 {
        return 0;
    }
    let selected = selected.min(total - 1);
    let mut start = start.min(total.saturating_sub(visible));
    if selected < start {
        start = selected;
    } else if selected >= start.saturating_add(visible) {
        start = selected.saturating_add(1).saturating_sub(visible);
    }
    start.min(total.saturating_sub(visible))
}

pub(super) fn variable_selection_viewport_start(
    start: usize,
    selected: usize,
    heights: &[usize],
    viewport_height: usize,
) -> usize {
    if heights.is_empty() || viewport_height == 0 {
        return 0;
    }
    let selected = selected.min(heights.len() - 1);
    let mut start = start.min(heights.len() - 1);
    if selected < start {
        return selected;
    }
    let mut used = heights[start..=selected]
        .iter()
        .fold(0usize, |total, height| total.saturating_add(*height));
    while start < selected && used > viewport_height {
        used = used.saturating_sub(heights[start]);
        start += 1;
    }
    start
}

pub(super) fn selection_item_y(container: Rect, row: usize, item_height: u16) -> u16 {
    container.y.saturating_add(1).saturating_add(
        u16::try_from(row)
            .unwrap_or(u16::MAX)
            .saturating_mul(item_height),
    )
}

pub(super) fn focus_border(focused: bool, theme: Theme) -> Style {
    Style::default().fg(if focused {
        theme.ui_focus_border
    } else {
        theme.ui_border
    })
}

pub(super) fn animated_color(position: usize, tick: u64, theme: Theme) -> Color {
    let stops = theme.animation_gradient.map(rgb_components);
    const STEPS: usize = 24;
    // Phase walks backward along the character position as the tick advances,
    // so the gradient flows left-to-right across the line (and clockwise along
    // animated borders). Six phase steps per tick double the original speed.
    let phase = (tick as usize * 6).wrapping_sub(position) % (stops.len() * STEPS);
    let stop = phase / STEPS;
    let amount = phase % STEPS;
    let from = stops[stop];
    let to = stops[(stop + 1) % stops.len()];
    let blend = |a: u8, b: u8| {
        let a = usize::from(a);
        let b = usize::from(b);
        ((a * (STEPS - amount) + b * amount) / STEPS) as u8
    };
    Color::Rgb(
        blend(from.0, to.0),
        blend(from.1, to.1),
        blend(from.2, to.2),
    )
}

pub(super) fn rgb_components(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(red, green, blue) => (red, green, blue),
        _ => unreachable!("theme colors are parsed as RGB"),
    }
}

pub(super) fn draw_animated_border(frame: &mut Frame, area: Rect, tick: u64, theme: Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let mut position = 0usize;
    for x in area.x..area.x.saturating_add(area.width) {
        frame.buffer_mut()[(x, area.y)].set_fg(animated_color(position, tick, theme));
        position += 1;
    }
    for y in area.y.saturating_add(1)..area.y.saturating_add(area.height) {
        let x = area.x.saturating_add(area.width.saturating_sub(1));
        frame.buffer_mut()[(x, y)].set_fg(animated_color(position, tick, theme));
        position += 1;
    }
    if area.height > 1 {
        let y = area.y.saturating_add(area.height - 1);
        for x in (area.x..area.x.saturating_add(area.width.saturating_sub(1))).rev() {
            frame.buffer_mut()[(x, y)].set_fg(animated_color(position, tick, theme));
            position += 1;
        }
    }
    if area.width > 1 {
        for y in
            (area.y.saturating_add(1)..area.y.saturating_add(area.height.saturating_sub(1))).rev()
        {
            frame.buffer_mut()[(area.x, y)].set_fg(animated_color(position, tick, theme));
            position += 1;
        }
    }
}

pub(super) fn compose_rect(area: Rect) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let width = if area.width > 4 {
        area.width.saturating_sub(4).min(CENTER_MAX_WIDTH)
    } else {
        area.width
    };
    let desired_height = if area.height >= 14 { 7 } else { 5 };
    let height = desired_height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let bottom_margin = u16::from(area.height > height);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height + bottom_margin));
    Rect::new(x, y, width, height)
}

pub(super) fn register_link_hitboxes(
    hitboxes: &mut Vec<LinkHitbox>,
    links: &[crate::markdown::RenderedLink],
    viewport: Rect,
    scroll: usize,
    base_dir: &Path,
) {
    let bottom = scroll.saturating_add(viewport.height as usize);
    for link in links
        .iter()
        .filter(|link| link.row >= scroll && link.row < bottom)
    {
        let column = link.column.min(viewport.width as usize);
        let width = link
            .width
            .min((viewport.width as usize).saturating_sub(column));
        if width == 0 {
            continue;
        }
        let target = match &link.target {
            LinkTarget::LocalFile(path) if !path.is_absolute() => {
                LinkTarget::LocalFile(base_dir.join(path))
            }
            target => target.clone(),
        };
        hitboxes.push(LinkHitbox {
            target,
            area: Rect::new(
                viewport.x.saturating_add(column as u16),
                viewport.y.saturating_add((link.row - scroll) as u16),
                width as u16,
                1,
            ),
        });
    }
}

pub(super) fn register_tag_hitboxes(
    hitboxes: &mut Vec<TagHitbox>,
    tags: &[crate::markdown::RenderedTag],
    viewport: Rect,
    scroll: usize,
) {
    let bottom = scroll.saturating_add(viewport.height as usize);
    for tag in tags
        .iter()
        .filter(|tag| tag.row >= scroll && tag.row < bottom)
    {
        let column = tag.column.min(viewport.width as usize);
        let width = tag
            .width
            .min((viewport.width as usize).saturating_sub(column));
        if width == 0 {
            continue;
        }
        hitboxes.push(TagHitbox {
            name: tag.name.clone(),
            area: Rect::new(
                viewport.x.saturating_add(column as u16),
                viewport.y.saturating_add((tag.row - scroll) as u16),
                width as u16,
                1,
            ),
        });
    }
}

pub(super) fn line_with_background(
    mut spans: Vec<Span<'static>>,
    width: usize,
    style: Style,
) -> Line<'static> {
    for span in &mut spans {
        span.style = style.patch(span.style);
    }
    let used: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    Line::from(spans)
}

pub(super) fn split_last_row(area: Rect) -> (Rect, Rect) {
    if area.height < 2 {
        return (area, Rect::new(area.x, area.y + area.height, area.width, 0));
    }
    (
        Rect::new(area.x, area.y, area.width, area.height - 1),
        Rect::new(area.x, area.y + area.height - 1, area.width, 1),
    )
}

pub(super) fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

/// Greedy display-width wrapping that keeps span styles and explicit newlines.
pub(super) fn wrap_spans_to_width(spans: &[Span<'_>], width: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut row_width = 0;
    for span in spans {
        for character in span.content.chars() {
            if character == '\n' {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
                continue;
            }
            let character_width = character.width().unwrap_or(1);
            if width > 0 && row_width + character_width > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
            }
            if let Some(last) = row.last_mut().filter(|last| last.style == span.style) {
                last.content.to_mut().push(character);
            } else {
                row.push(Span::styled(character.to_string(), span.style));
            }
            row_width += character_width;
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}
