use super::*;

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
        .title(if focused {
            " Compose "
        } else {
            " Compose · i "
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
                CenterView::Document => {
                    "Enter append · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline"
                }
                _ => "Enter send · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline",
            }
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

