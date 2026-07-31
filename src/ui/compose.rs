use super::*;

const BODY_TOP_MARGIN: u16 = 2;
const BODY_BOTTOM_GAP: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FloatingComposeLayout {
    pub(super) compose: Rect,
    pub(super) body: Rect,
    pub(super) visible_body: Rect,
}

pub(super) fn floating_compose_layout(content: Rect) -> FloatingComposeLayout {
    let compose = compose_rect(content);
    let body_y = content
        .y
        .saturating_add(BODY_TOP_MARGIN)
        .min(content.y.saturating_add(content.height));
    let body = Rect::new(
        content.x,
        body_y,
        content.width,
        content
            .y
            .saturating_add(content.height)
            .saturating_sub(body_y),
    );
    let visible_body = Rect::new(
        body.x,
        body.y,
        body.width,
        compose
            .y
            .saturating_sub(BODY_BOTTOM_GAP)
            .saturating_sub(body.y)
            .min(body.height),
    );
    FloatingComposeLayout {
        compose,
        body,
        visible_body,
    }
}

pub(super) fn draw_floating_compose(
    frame: &mut Frame,
    app: &App,
    layout: FloatingComposeLayout,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if layout.compose.width == 0 || layout.compose.height == 0 {
        return;
    }
    clear_widget(frame, layout.compose);
    draw_compose(frame, app, layout.compose, interactive, cursor_position);
}

pub(super) fn draw_compose(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    let focused = app.focus == Focus::Compose;
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(match (focused, app.center_view) {
            (true, CenterView::Chat) => " Message ",
            (false, CenterView::Chat) => " Message · i ",
            (true, _) => " Compose ",
            (false, _) => " Compose · i ",
        })
        .style(Style::default().bg(app.theme.surface_compose))
        .border_style(focus_border(focused, app.theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if focused {
        draw_animated_border(frame, area, app.animation_tick, app.theme);
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let (text_area, toolbar) = split_last_row(inner);
    if let Some(position) = draw_multiline_input(
        frame,
        text_area,
        &app.input,
        app.input_cursor,
        "Write something…",
        focused && interactive,
        app.theme,
    ) {
        *cursor_position = Some(position);
    }

    if toolbar.height > 0 {
        let lines = if app.input.is_empty() {
            0
        } else {
            app.input.lines().count().max(1)
        };
        let count = format!("{lines}l · {}c", app.input.chars().count());
        let hint = if focused && toolbar.width >= 72 {
            match app.center_view {
                CenterView::Chat => "Enter send · Ctrl+J newline · Esc chat",
                CenterView::Document => {
                    "Enter append · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline"
                }
                _ => "Enter send · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline",
            }
        } else if focused && app.center_view == CenterView::Chat && toolbar.width >= 25 {
            "Enter send · Ctrl+J newline"
        } else if focused && toolbar.width >= 42 {
            "Ctrl+Enter Agent · Ctrl+U recall"
        } else if focused && toolbar.width >= 25 {
            "Ctrl+Enter Agent"
        } else {
            ""
        };
        draw_left_right_line(frame, toolbar, &count, hint, app.theme.text_muted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floating_compose_layout_renders_full_body_but_keeps_scroll_above_compose() {
        let content = Rect::new(10, 3, 80, 24);
        let layout = floating_compose_layout(content);

        assert_eq!(layout.body.y, content.y + BODY_TOP_MARGIN);
        assert_eq!(layout.body.bottom(), content.bottom());
        assert_eq!(layout.visible_body.x, layout.body.x);
        assert_eq!(layout.visible_body.width, layout.body.width);
        assert_eq!(
            layout.visible_body.bottom() + BODY_BOTTOM_GAP,
            layout.compose.y
        );
    }
}
