use super::*;

pub(super) fn draw_notification(frame: &mut Frame, root: Rect, message: &str, theme: Theme) {
    if root.width < 4 || root.height < 3 {
        return;
    }
    let width = root.width.saturating_sub(2).min(44);
    let text_width = width.saturating_sub(4).max(1) as usize;
    let rows = wrap_spans_to_width(&[Span::raw(message.to_string())], text_width)
        .len()
        .max(1);
    let height = (rows as u16).saturating_add(2).min(root.height.min(8));
    let area = Rect::new(
        root.x + root.width.saturating_sub(width).saturating_sub(1),
        root.y.saturating_add(1),
        width,
        height,
    );
    clear_widget(frame, area);
    frame.render_widget(
        Paragraph::new(message.to_string())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .padding(Padding::horizontal(1))
                    .title(" Notification ")
                    .style(Style::default().bg(theme.surface_panel))
                    .border_style(Style::default().fg(theme.ui_warning)),
            ),
        area,
    );
}
