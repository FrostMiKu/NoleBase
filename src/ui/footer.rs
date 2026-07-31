use super::*;

pub(super) fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let surface = match (app.focus, app.center_view, app.files_context) {
        (Focus::Files, _, FilesContext::Search) => "FILES/SEARCH",
        (Focus::Files, _, FilesContext::MoveTarget) => "FILES/MOVE",
        (Focus::Files, _, FilesContext::NewTarget) => "FILES/NEW",
        (Focus::Files, _, FilesContext::Rename) => "FILES/RENAME",
        (Focus::Files, _, _) => "FILES",
        (Focus::Views, _, _) => "VIEWS",
        (Focus::Agent, _, _) => "AGENT",
        (Focus::Compose, _, _) => "COMPOSE",
        (_, CenterView::Document, _) => "DOCUMENT",
        (_, CenterView::Search, _) => "SEARCH",
        (_, CenterView::DocumentSearch, _) => "FIND",
        (_, CenterView::Tags, _) => "TAGS",
        (_, CenterView::Chat, _) => "AI CHAT",
        (_, CenterView::Todo, _) => "TODO",
        _ => "DAILY",
    };
    let surface_segment = format!(" {surface} ");
    let permission_segment = format!(" {} ", app.permission_mode.label());
    let mouse_status = Span::styled(
        " ",
        Style::default().bg(if app.mouse_captured {
            app.theme.ui_shortcut
        } else {
            app.theme.ui_warning
        }),
    );
    let surface_style = Style::default()
        .bg(app.theme.surface_status_context)
        .fg(app.theme.text_on_accent);
    let mode_line = if app.permission_mode == PermissionMode::Bypass {
        let mut spans = vec![
            mouse_status,
            Span::styled(surface_segment.clone(), surface_style),
        ];
        let bypass_style = Style::default().bg(app.theme.surface_overlay);
        spans.push(Span::styled(" ", bypass_style));
        spans.extend("BYPASS".chars().enumerate().map(|(index, character)| {
            Span::styled(
                character.to_string(),
                bypass_style
                    .fg(animated_color(index * 8, app.animation_tick, app.theme))
                    .add_modifier(Modifier::BOLD),
            )
        }));
        spans.push(Span::styled(" ", bypass_style));
        Line::from(spans)
    } else {
        Line::from(vec![
            mouse_status,
            Span::styled(surface_segment.clone(), surface_style),
            Span::styled(
                permission_segment.clone(),
                Style::default()
                    .bg(app.theme.surface_status_mode)
                    .fg(app.theme.text_on_accent),
            ),
        ])
    };
    let status_bar_style = Style::default().bg(app.theme.surface_status_bar);
    frame.render_widget(Paragraph::new(mode_line).style(status_bar_style), area);

    let hint = footer_hint(app, area.width);
    let mode_width = 1usize
        .saturating_add(surface_segment.width())
        .saturating_add(permission_segment.width()) as u16;
    let available_status = area
        .width
        .saturating_sub(mode_width)
        .saturating_sub(hint.width() as u16)
        .saturating_sub(u16::from(!hint.is_empty()));
    if !app.status.is_empty() && available_status > 2 {
        let status = Line::from(Span::styled(
            format!(" {}", app.status),
            Style::default().fg(app.theme.ui_warning),
        ));
        frame.render_widget(
            Paragraph::new(status).style(status_bar_style),
            Rect::new(area.x + mode_width, area.y, available_status, area.height),
        );
    }
    if !hint.is_empty() {
        let width = (hint.width() as u16).min(area.width);
        frame.render_widget(
            Paragraph::new(Span::styled(
                hint,
                Style::default().fg(app.theme.text_muted),
            ))
            .style(status_bar_style)
            .alignment(Alignment::Right),
            Rect::new(area.x + area.width - width, area.y, width, area.height),
        );
    }
}

pub(super) fn footer_hint(app: &App, width: u16) -> &'static str {
    if width < 28 {
        return "";
    }
    if app.overlay == Some(Overlay::Terminal) {
        return "Ctrl+` close terminal";
    }
    if width < 55 {
        return match (app.focus, app.center_view) {
            (Focus::Compose, CenterView::Document) => "Esc document",
            (Focus::Compose, CenterView::Chat) => "Enter send · Esc chat",
            (Focus::Compose, _) => "Esc daily",
            (Focus::Files, _) => "Esc back · Enter open",
            (Focus::Views, _) => "Esc back · Enter switch",
            (Focus::Agent, _) if app.ai_running => "c cancel · C clear · Esc back",
            (Focus::Agent, _) => "C clear · Esc back",
            (Focus::Center, CenterView::Document)
                if app.document.as_ref().is_some_and(|document| {
                    matches!(document.kind, crate::app::DocumentKind::Skill(_))
                }) =>
            {
                "e edit · r rename · d delete"
            }
            (Focus::Center, CenterView::Document)
                if app.document.as_ref().is_some_and(|document| {
                    matches!(document.kind, crate::app::DocumentKind::File(_))
                }) =>
            {
                if app.current_note_archived() == Some(true) {
                    "e edit · u restore · r rename · d delete"
                } else {
                    "e edit · a archive · r rename · d delete"
                }
            }
            (Focus::Center, CenterView::Tags) => "type filter · ↑↓ select · Enter search",
            (Focus::Center, CenterView::Chat) => "i message · ↑↓ scroll · C clear",
            (Focus::Center, CenterView::Todo) => "↑↓ select · Enter toggle",
            (Focus::Center, _) => "# tags · Ctrl+P commands",
        };
    }
    match (app.focus, app.center_view) {
        (Focus::Compose, CenterView::Daily) => {
            "Enter send · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline · Ctrl+P commands"
        }
        (Focus::Compose, CenterView::Document) => {
            "Enter append · Ctrl+Enter Agent · Ctrl+U recall · Ctrl+J newline · Ctrl+P commands"
        }
        (Focus::Compose, CenterView::Chat) => {
            "Enter send · Ctrl+J newline · Esc chat · Ctrl+P commands"
        }
        (Focus::Files, _) => "↑↓ select · Enter open · a/u archive/restore · e edit · / filter",
        (Focus::Views, _) => "↑↓ select · Enter switch · Esc back",
        (Focus::Agent, _) if app.ai_running => "c cancel · C clear session · ↑↓ scroll · ← center",
        (Focus::Agent, _) => "C clear session · ↑↓ scroll · ← center",
        (_, CenterView::Daily) if width >= 95 => {
            "i compose · f files · t todo · / search · # tags · Ctrl+P commands · ? help"
        }
        (_, CenterView::Document)
            if app.document.as_ref().is_some_and(|document| {
                matches!(document.kind, crate::app::DocumentKind::Skill(_))
            }) =>
        {
            "↑↓ scroll · e edit · r rename · d delete · / find · Esc skills"
        }
        (_, CenterView::Document)
            if app.document.as_ref().is_some_and(|document| {
                matches!(document.kind, crate::app::DocumentKind::File(_))
            }) =>
        {
            if app.current_note_archived() == Some(true) {
                if width >= 85 {
                    "↑↓ scroll · e edit · u restore · r rename · d delete · / find · Esc back"
                } else {
                    "e edit · u restore · r rename · d delete · / find"
                }
            } else if width >= 85 {
                "↑↓ scroll · e edit · a archive · r rename · d delete · / find · Esc back"
            } else {
                "e edit · a archive · r rename · d delete · / find"
            }
        }
        (_, CenterView::Document) => "↑↓ scroll · e edit DailyNote · / find · Esc back",
        (_, CenterView::Search) => "type query · ↑↓ select · Enter open · Esc back",
        (_, CenterView::DocumentSearch) => "type query · ↑↓ select · Enter jump · Esc article",
        (_, CenterView::Tags) => "type filter · ↑↓ select · Enter search · Esc back",
        (_, CenterView::Chat) if app.ai_running => {
            "i message · ↑↓ scroll · c cancel · C clear · ← files · → views"
        }
        (_, CenterView::Chat) => "i message · ↑↓ scroll · C clear session · ← files · → views",
        (_, CenterView::Todo) => "↑↓ select · Enter toggle · ← files · → views · Esc daily",
        _ => "f files · t todo · Ctrl+P commands · ? help",
    }
}

pub(super) fn draw_left_right_line(
    frame: &mut Frame,
    area: Rect,
    left: &str,
    right: &str,
    color: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(left.to_string(), Style::default().fg(color))),
        area,
    );
    if !right.is_empty() {
        let width = (right.width() as u16).min(area.width);
        frame.render_widget(
            Paragraph::new(Span::styled(right.to_string(), Style::default().fg(color)))
                .alignment(Alignment::Right),
            Rect::new(area.x + area.width - width, area.y, width, 1),
        );
    }
}
