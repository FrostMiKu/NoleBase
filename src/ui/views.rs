use super::*;

pub(super) fn draw_workspace_views(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    interactive: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(" Views ")
        .style(Style::default().bg(app.theme.surface_panel))
        .border_style(focus_border(app.focus == Focus::Views, app.theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let selected = app
        .workspace_view_index
        .min(WorkspaceView::ALL.len().saturating_sub(1));
    let focused = app.focus == Focus::Views;
    let selection_visible =
        focused || app.center_view.sidebar_selection() == SidebarSelection::Views;
    let mut y = inner.y.saturating_add(1);
    for (index, view) in WorkspaceView::ALL.iter().enumerate() {
        if y >= inner.y.saturating_add(inner.height) {
            break;
        }
        let layout_height = 3.min(inner.y + inner.height - y);
        let active = view.center_view == app.center_view;
        let item_selected = selection_visible && index == selected;
        let row_style = if item_selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .add_modifier(Modifier::BOLD)
                .bg(if focused {
                    app.theme.selection_background
                } else {
                    app.theme.selection_background_inactive
                })
        } else {
            Style::default()
        };
        let selection_area = shared_selection_area(inner, y, layout_height);
        if item_selected {
            frame.render_widget(Block::default().style(row_style), selection_area);
        }

        let title = Rect::new(inner.x, y, inner.width, 1);
        frame.render_widget(
            Paragraph::new(format!("  {}", view.label)).style(row_style),
            title,
        );
        if active {
            let active_label = "active";
            let active_width = active_label.width() as u16;
            frame.render_widget(
                Paragraph::new(Span::styled(
                    active_label,
                    if item_selected {
                        Style::default()
                            .fg(app.theme.selection_foreground)
                            .add_modifier(Modifier::DIM)
                    } else {
                        Style::default().fg(app.theme.ui_shortcut)
                    },
                ))
                .style(row_style)
                .alignment(Alignment::Right),
                Rect::new(
                    title.x + title.width.saturating_sub(active_width),
                    title.y,
                    active_width,
                    1,
                ),
            );
        }
        let content_height = 2.min(layout_height);
        if content_height > 1 {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    format!("  {}", view.description),
                    if item_selected {
                        Style::default()
                            .fg(app.theme.selection_foreground)
                            .add_modifier(Modifier::DIM)
                    } else {
                        Style::default().fg(app.theme.text_muted)
                    },
                ))
                .style(row_style),
                Rect::new(inner.x, y + 1, inner.width, 1),
            );
        }
        if interactive {
            app.workspace_view_hitboxes.push(WorkspaceViewHitbox {
                index,
                area: Rect::new(inner.x, y, inner.width, content_height),
            });
        }
        if item_selected {
            draw_selection_indicator(frame, selection_area, app.theme);
        }
        y = y.saturating_add(layout_height);
    }
}
