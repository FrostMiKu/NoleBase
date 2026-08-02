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
        crate::app::DocumentKind::Skill(_) => app.storage.skills_dir.clone(),
    };
    let (rendered_links, rendered_tags, rendered_images, document_scroll, visible_top_margin) = {
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
        let paper_height = document_paper_height(page_area.width as usize, lines.len());
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
        visible.extend(visible_line_window(
            lines,
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
    }
    draw_floating_compose(frame, app, compose_layout, interactive, cursor_position);
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
}
