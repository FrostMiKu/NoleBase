use super::*;

const DOCUMENT_TOP_MARGIN: usize = 2;
const DOCUMENT_BOTTOM_MARGIN: usize = 2;
// A terminal cell is roughly twice as tall as it is wide, so A4's physical
// sqrt(2) height-to-width ratio maps to about 0.7 rows per column.
const A4_CELL_HEIGHT_NUMERATOR: usize = 7;
const A4_CELL_HEIGHT_DENOMINATOR: usize = 10;

fn document_paper_height(width: usize, content_height: usize) -> usize {
    let minimum_a4_height = width
        .saturating_mul(A4_CELL_HEIGHT_NUMERATOR)
        .div_ceil(A4_CELL_HEIGHT_DENOMINATOR);
    DOCUMENT_TOP_MARGIN
        .saturating_add(content_height)
        .saturating_add(DOCUMENT_BOTTOM_MARGIN)
        .max(minimum_a4_height)
}

/// The "Backlinks" section rendered after the note body: two blank separator
/// rows, a level-2 heading, one blank row, then one indented row per distinct
/// managed note linking to the open document. The section scrolls with the
/// document; entry rows are registered as clickable hitboxes by the caller.
fn backlink_section_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    if app.document_backlinks.is_empty() {
        return Vec::new();
    }
    let heading = Line::from(Span::styled(
        "Backlinks",
        Style::default()
            .fg(app.theme.markdown_heading_2)
            .add_modifier(Modifier::BOLD),
    ));
    let name_style = Style::default()
        .fg(app.theme.markdown_wikilink)
        .underline_color(app.theme.markdown_link)
        .add_modifier(Modifier::UNDERLINED);
    let bullet = Span::styled("• ", Style::default().fg(app.theme.markdown_list));
    let mut lines = vec![Line::default(), Line::default(), heading, Line::default()];
    for path in &app.document_backlinks {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let name = truncate_to_display_width(&name, width.saturating_sub(3));
        lines.push(Line::from(vec![
            Span::raw(" "),
            bullet.clone(),
            Span::styled(name, name_style),
        ]));
    }
    lines
}

/// Truncate `value` to fit `width` display columns, appending `…` when cut.
fn truncate_to_display_width(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_string();
    }
    let target = width.saturating_sub(1);
    let mut output = String::new();
    let mut used: usize = 0;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used.saturating_add(character_width) > target {
            break;
        }
        output.push(character);
        used += character_width;
    }
    format!("{output}…")
}

/// Slice a visible window across the body and the trailing backlink section,
/// treating them as one scrollable sequence in body-row coordinate space.
fn visible_window<'a>(
    body: &[Line<'a>],
    backlinks: &[Line<'a>],
    scroll: usize,
    viewport_height: usize,
) -> Vec<Line<'a>> {
    let body_skip = scroll.min(body.len());
    let mut visible: Vec<Line<'a>> = body
        .iter()
        .skip(body_skip)
        .take(viewport_height)
        .cloned()
        .collect();
    let remaining = viewport_height.saturating_sub(visible.len());
    let backlink_skip = scroll.saturating_sub(body.len());
    visible.extend(
        backlinks
            .iter()
            .skip(backlink_skip)
            .take(remaining)
            .cloned(),
    );
    visible
}

pub(super) fn draw_document(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if app.document.is_none() {
        frame.render_widget(
            Paragraph::new("No document").alignment(Alignment::Center),
            area,
        );
        return;
    }
    let content = inset_horizontal(area, 2);
    if content.width == 0 || content.height == 0 {
        return;
    }
    let compose_layout = floating_compose_layout(content);
    app.layout.compose = non_empty(compose_layout.compose);
    let header = Rect::new(content.x, content.y, content.width, 1);
    let page_area = compose_layout.body;
    let page_style = Style::default().bg(app.theme.surface_panel);
    let horizontal_padding = (PAGE_PADDING_X as u16).min(page_area.width.saturating_sub(1) / 2);
    let document_area = Rect::new(
        page_area.x.saturating_add(horizontal_padding),
        page_area.y,
        page_area
            .width
            .saturating_sub(horizontal_padding.saturating_mul(2)),
        page_area.height,
    );
    let unoccluded_document_height = compose_layout.visible_body.height.min(document_area.height);
    let image_base = match &app.document.as_ref().expect("document checked above").kind {
        crate::app::DocumentKind::File(path) => path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| app.storage.root.clone()),
        crate::app::DocumentKind::Daily(_) => app.storage.daily_dir.clone(),
        crate::app::DocumentKind::Skill(path) => path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| app.storage.skills_dir.clone()),
    };
    let backlink_lines = backlink_section_lines(app, document_area.width as usize);
    let (
        rendered_links,
        rendered_tags,
        rendered_images,
        document_scroll,
        visible_top_margin,
        body_line_count,
    ) = {
        let document = app.document.as_mut().expect("document checked above");
        frame.render_widget(
            Paragraph::new(Span::styled(
                document.title.clone(),
                Style::default()
                    .fg(app.theme.ui_page_heading)
                    .add_modifier(Modifier::BOLD),
            )),
            header,
        );
        document.ensure_rendered(document_area.width as usize, app.theme);
        if let Some(target_line) = document.target_line.take() {
            let target_scroll = DOCUMENT_TOP_MARGIN
                .saturating_add(crate::markdown::rendered_row_for_source_line_in(
                    &document.source,
                    target_line,
                    &document
                        .render_cache
                        .as_ref()
                        .expect("completed document render cache")
                        .rendered
                        .lines,
                    document_area.width as usize,
                    app.theme,
                ))
                .min(u16::MAX as usize);
            let current_scroll = document.scroll as usize;
            let viewport_end = current_scroll.saturating_add(unoccluded_document_height as usize);
            if target_scroll < current_scroll || target_scroll >= viewport_end {
                document.scroll = target_scroll as u16;
            }
        }
        let rendered = &document
            .render_cache
            .as_ref()
            .expect("document render cache was initialized")
            .rendered;
        let rendered_links = rendered.links.clone();
        let rendered_tags = rendered.tags.clone();
        let rendered_images = rendered.images.clone();
        let lines = &rendered.lines;
        let body_line_count = lines.len();
        let paper_height = document_paper_height(
            page_area.width as usize,
            body_line_count.saturating_add(backlink_lines.len()),
        );
        let max_scroll = paper_height.saturating_sub(unoccluded_document_height as usize);
        document.scroll = (document.scroll as usize).min(max_scroll) as u16;
        let document_scroll = document.scroll as usize;
        let visible_paper_height = paper_height
            .saturating_sub(document_scroll)
            .min(page_area.height as usize) as u16;
        frame.render_widget(
            Block::default().style(page_style),
            Rect::new(
                page_area.x,
                page_area.y,
                page_area.width,
                visible_paper_height,
            ),
        );
        let visible_top_margin = DOCUMENT_TOP_MARGIN
            .saturating_sub(document_scroll)
            .min(document_area.height as usize);
        let content_scroll = document_scroll.saturating_sub(DOCUMENT_TOP_MARGIN);
        let mut visible = vec![Line::default(); visible_top_margin];
        visible.extend(visible_window(
            lines,
            &backlink_lines,
            content_scroll,
            (document_area.height as usize).saturating_sub(visible_top_margin),
        ));
        frame.render_widget(Paragraph::new(visible), document_area);
        (
            rendered_links,
            rendered_tags,
            rendered_images,
            content_scroll,
            visible_top_margin as u16,
            body_line_count,
        )
    };
    let content_document_area = Rect::new(
        document_area.x,
        document_area.y.saturating_add(visible_top_margin),
        document_area.width,
        document_area.height.saturating_sub(visible_top_margin),
    );
    app.images.render(
        frame,
        &rendered_images,
        content_document_area,
        document_scroll,
        &image_base,
        app.theme,
    );
    if interactive {
        let interactive_document_area = Rect::new(
            content_document_area.x,
            content_document_area.y,
            content_document_area.width,
            unoccluded_document_height.saturating_sub(visible_top_margin),
        );
        register_link_hitboxes(
            &mut app.link_hitboxes,
            &rendered_links,
            interactive_document_area,
            document_scroll,
            &image_base,
        );
        register_tag_hitboxes(
            &mut app.tag_hitboxes,
            &rendered_tags,
            interactive_document_area,
            document_scroll,
        );
        register_backlink_hitboxes(
            &mut app.backlink_hitboxes,
            &app.document_backlinks,
            body_line_count,
            interactive_document_area,
            document_scroll,
        );
    }
    draw_floating_compose(frame, app, compose_layout, interactive, cursor_position);
}

/// Register one clickable row per backlink entry. Entry rows live directly
/// after the body (two separators + heading + one separator), so their row
/// numbers are `body_line_count + 4 + index` in the same coordinate space the
/// document links use; entries outside the scrolled viewport are skipped.
fn register_backlink_hitboxes(
    hitboxes: &mut Vec<BacklinkHitbox>,
    backlinks: &[std::path::PathBuf],
    body_line_count: usize,
    viewport: Rect,
    scroll: usize,
) {
    let bottom = scroll.saturating_add(viewport.height as usize);
    for (index, path) in backlinks.iter().enumerate() {
        let row = body_line_count.saturating_add(4).saturating_add(index);
        if row < scroll || row >= bottom {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        // Match the renderer's truncation (backlink_section_lines) so the
        // hitbox covers exactly the visible name, then clamp both the column
        // and the width into the viewport like the link/tag hitboxes.
        let name = truncate_to_display_width(&name, viewport.width.saturating_sub(3) as usize);
        let column = 3.min(viewport.width as usize); // after " " + "• "
        let width = UnicodeWidthStr::width(name.as_str())
            .min((viewport.width as usize).saturating_sub(column));
        if width == 0 {
            continue;
        }
        hitboxes.push(BacklinkHitbox {
            path: path.clone(),
            area: Rect::new(
                viewport
                    .x
                    .saturating_add(u16::try_from(column).unwrap_or(u16::MAX)),
                viewport
                    .y
                    .saturating_add(u16::try_from(row - scroll).unwrap_or(u16::MAX)),
                u16::try_from(width).unwrap_or(u16::MAX),
                1,
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_uses_a4_minimum_height_and_keeps_vertical_padding() {
        assert_eq!(document_paper_height(100, 1), 70);
        assert_eq!(
            document_paper_height(100, 100),
            DOCUMENT_TOP_MARGIN + 100 + DOCUMENT_BOTTOM_MARGIN
        );
    }

    #[test]
    fn backlink_hitbox_covers_exactly_the_rendered_truncated_name() {
        let path = std::path::PathBuf::from("/root/A-Very-Long-Backlink-Name-That-Overflows.md");
        // The renderer truncates to viewport.width - 3 (the " " + "• " prefix),
        // so the hitbox must stop there too instead of spanning the full width.
        let mut hitboxes = Vec::new();
        register_backlink_hitboxes(
            &mut hitboxes,
            std::slice::from_ref(&path),
            10,
            Rect::new(0, 0, 40, 20),
            0,
        );
        assert_eq!(hitboxes.len(), 1);
        let hitbox = &hitboxes[0];
        assert_eq!(hitbox.area.x, 3);
        assert_eq!(hitbox.area.width, 37);
        assert_eq!(hitbox.area.y, 14); // body_line_count + 4 + index - scroll
        assert!(hitbox.area.x + hitbox.area.width <= 40);
    }

    #[test]
    fn backlink_hitbox_skips_names_the_viewport_cannot_show() {
        let path = std::path::PathBuf::from("/root/Name.md");
        for width in 0..4u16 {
            let mut hitboxes = Vec::new();
            register_backlink_hitboxes(
                &mut hitboxes,
                std::slice::from_ref(&path),
                10,
                Rect::new(0, 0, width, 20),
                0,
            );
            assert!(hitboxes.is_empty(), "no hitbox below width 4: {width}");
        }
        // At four columns only the ellipsized name fits next to the prefix.
        let mut hitboxes = Vec::new();
        register_backlink_hitboxes(
            &mut hitboxes,
            std::slice::from_ref(&path),
            10,
            Rect::new(0, 0, 4, 20),
            0,
        );
        assert_eq!(hitboxes.len(), 1);
        assert_eq!(hitboxes[0].area.x, 3);
        assert_eq!(hitboxes[0].area.width, 1);
        assert!(hitboxes[0].area.x + hitboxes[0].area.width <= 4);
    }

    #[test]
    fn backlink_hitbox_keeps_row_math_when_scrolled_past_the_entries() {
        let path = std::path::PathBuf::from("/root/Name.md");
        let mut hitboxes = Vec::new();
        // Rows live in body-row space: body_line_count + 4 + index; entries
        // below the viewport bottom are dropped, entries above the fold too.
        register_backlink_hitboxes(
            &mut hitboxes,
            &[path.clone(), path.clone()],
            10,
            Rect::new(0, 0, 40, 3),
            13,
        );
        assert_eq!(
            hitboxes
                .iter()
                .map(|hitbox| hitbox.area.y)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "row 14 = 10 + 4 skips the top; both entries fit the three-row viewport"
        );
        let mut hitboxes = Vec::new();
        register_backlink_hitboxes(
            &mut hitboxes,
            std::slice::from_ref(&path),
            10,
            Rect::new(0, 0, 40, 20),
            30,
        );
        assert!(
            hitboxes.is_empty(),
            "rows above the scroll window are skipped"
        );
    }
}
