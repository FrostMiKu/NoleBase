use super::*;

const DOCUMENT_TOP_MARGIN: usize = 2;

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
    let compose = compose_rect(content);
    app.layout.compose = non_empty(compose);
    let header = Rect::new(content.x, content.y, content.width, 1);
    let page_area = Rect::new(
        content.x,
        content.y.saturating_add(2),
        content.width,
        content
            .y
            .saturating_add(content.height)
            .saturating_sub(content.y.saturating_add(2)),
    );
    let page_style = Style::default().bg(app.theme.surface_panel);
    frame.render_widget(Block::default().style(page_style), page_area);
    let horizontal_padding = (PAGE_PADDING_X as u16).min(page_area.width.saturating_sub(1) / 2);
    let document_area = Rect::new(
        page_area.x.saturating_add(horizontal_padding),
        page_area.y,
        page_area
            .width
            .saturating_sub(horizontal_padding.saturating_mul(2)),
        page_area.height,
    );
    let unoccluded_document_height = compose
        .y
        .saturating_sub(1)
        .saturating_sub(document_area.y)
        .min(document_area.height);
    let image_base = match &app.document.as_ref().expect("document checked above").kind {
        crate::app::DocumentKind::File(path) => path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| app.storage.root.clone()),
        crate::app::DocumentKind::Daily(_) => app.storage.daily_dir.clone(),
    };
    let (
        rendered_links,
        rendered_tags,
        rendered_images,
        document_scroll,
        visible_top_margin,
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
        if let Some(target_line) = document.target_line.take() {
            document.scroll = DOCUMENT_TOP_MARGIN
                .saturating_add(crate::markdown::rendered_row_for_source_line(
                    &document.source,
                    target_line,
                    document_area.width as usize,
                    app.theme,
                ))
                .min(u16::MAX as usize) as u16;
        }
        document.ensure_rendered(document_area.width as usize, app.theme);
        let rendered = &document
            .render_cache
            .as_ref()
            .expect("document render cache was initialized")
            .rendered;
        let rendered_links = rendered.links.clone();
        let rendered_tags = rendered.tags.clone();
        let rendered_images = rendered.images.clone();
        let lines = &rendered.lines;
        let max_scroll = lines
            .len()
            .saturating_add(DOCUMENT_TOP_MARGIN)
            .saturating_sub(unoccluded_document_height as usize);
        document.scroll = (document.scroll as usize).min(max_scroll) as u16;
        let document_scroll = document.scroll as usize;
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
        frame.render_widget(Paragraph::new(visible).style(page_style), document_area);
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
    if compose.width > 0 && compose.height > 0 {
        clear_widget(frame, compose);
        draw_compose(frame, app, compose, interactive, cursor_position);
    }
}
