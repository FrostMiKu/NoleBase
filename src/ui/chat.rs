use super::*;

pub(super) fn draw_chat(
    frame: &mut Frame,
    app: &mut App,
    surface: Rect,
    content: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if surface.width == 0 || surface.height == 0 {
        return;
    }
    let content = inset_horizontal(content, 2);
    if content.width == 0 || content.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "AI Chat",
            Style::default()
                .fg(app.theme.ui_page_heading)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(content.x, content.y, content.width, 1),
    );

    let compose_layout = floating_compose_layout(content);
    app.layout.compose = non_empty(compose_layout.compose);
    draw_chat_messages(
        frame,
        app,
        compose_layout.body,
        compose_layout.visible_body,
        interactive,
    );
    draw_floating_compose(frame, app, compose_layout, interactive, cursor_position);
}

fn draw_chat_messages(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    visible_area: Rect,
    interactive: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if app.agent_panel.is_empty() {
        app.agent_vlist.caches.clear();
        app.agent_vlist.geometry.resize(0);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Start a conversation with your Agent",
                Style::default().fg(app.theme.text_muted),
            ))
            .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let width = area.width as usize;
    let render_height = area.height as usize;
    let view_height = visible_area.height.min(area.height) as usize;
    sync_chat_vlist(app, width);
    let tail_pinned = app.agent_scroll == u16::MAX;
    let mut scroll =
        (app.agent_scroll as usize).min(app.agent_vlist.geometry.max_scroll(view_height));
    scroll = measure_visible_agent_entries(app, scroll, view_height, tail_pinned);
    app.agent_scroll = scroll.min(u16::MAX as usize) as u16;
    let (visible, rendered_links, rendered_images) =
        visible_agent_lines(app, scroll, render_height);

    fill_agent_message_rows(frame, area, &visible);
    frame.render_widget(Paragraph::new(visible), area);
    let image_base = app.storage.root.clone();
    app.images.render(
        frame,
        &rendered_images,
        area,
        scroll,
        &image_base,
        app.theme,
    );
    if interactive {
        register_link_hitboxes(
            &mut app.link_hitboxes,
            &rendered_links,
            visible_area,
            scroll,
            &image_base,
        );
    }
    if let Some(index) = active_chat_card(app) {
        let first = app.agent_vlist.geometry.item_top(index);
        let last = first
            + app.agent_vlist.caches[index]
                .as_ref()
                .expect("active chat card is rendered")
                .lines
                .len()
                .saturating_sub(2);
        draw_animated_card_border(
            frame,
            CardBorderGeometry {
                area,
                scroll,
                first,
                last,
            },
            app.animation_tick,
            app.theme,
            app.theme.surface_message_agent,
        );
    }
}

fn active_chat_card(app: &App) -> Option<usize> {
    app.ai_running.then(|| {
        app.agent_panel.iter().rposition(|entry| {
            matches!(
                entry,
                crate::agent_session::AgentPanelEntry::Assistant {
                    text,
                    streaming: true,
                    ..
                } if !text.trim().is_empty()
            ) || matches!(
                entry,
                crate::agent_session::AgentPanelEntry::Assistant {
                    text,
                    final_output: false,
                    ..
                } if !text.trim().is_empty()
            )
        })
    })?
}

pub(super) fn render_chat_entry(
    entry: &crate::agent_session::AgentPanelEntry,
    width: usize,
    theme: Theme,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    match entry {
        crate::agent_session::AgentPanelEntry::Prompt { text, muted } => render_chat_card(
            "User",
            text,
            *muted,
            theme.surface_message_user,
            theme.ui_agent_user,
            width,
            theme,
        ),
        crate::agent_session::AgentPanelEntry::Assistant { text, .. } if text.trim().is_empty() => {
            (vec![Line::default()], Vec::new(), Vec::new())
        }
        crate::agent_session::AgentPanelEntry::Assistant { text, .. } => render_chat_card(
            "Agent",
            text,
            false,
            theme.surface_message_agent,
            theme.ui_agent_assistant,
            width,
            theme,
        ),
        crate::agent_session::AgentPanelEntry::Tool { text, .. } => (
            render_chat_tool(text, width, 0, false, theme),
            Vec::new(),
            Vec::new(),
        ),
        crate::agent_session::AgentPanelEntry::Error(text) => {
            let (body_start, _) = centered_daily_body_axis(width, PAGE_PADDING_X);
            let padding = Span::raw(" ".repeat(body_start));
            let mut lines = text
                .lines()
                .map(|line| {
                    Line::from(vec![
                        padding.clone(),
                        Span::styled(line.to_string(), Style::default().fg(theme.ui_error)),
                    ])
                })
                .collect::<Vec<_>>();
            if lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::default());
            (lines, Vec::new(), Vec::new())
        }
    }
}

pub(super) fn render_chat_tool(
    text: &str,
    width: usize,
    tick: u64,
    animate: bool,
    theme: Theme,
) -> Vec<Line<'static>> {
    let (body_start, body_width) = centered_daily_body_axis(width, PAGE_PADDING_X);
    let activity = if animate {
        animated_activity_lines(text, body_width, tick, theme)
    } else {
        activity_lines(text, body_width, theme)
    };
    let mut lines = activity
        .into_iter()
        .map(|line| {
            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            spans.push(Span::raw(" ".repeat(body_start)));
            spans.extend(line.spans);
            let mut aligned = Line::from(spans);
            aligned.style = line.style;
            aligned
        })
        .collect::<Vec<_>>();
    lines.push(Line::default());
    lines
}

fn render_chat_card(
    label: &str,
    text: &str,
    muted: bool,
    background: Color,
    label_color: Color,
    width: usize,
    theme: Theme,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let card_style = Style::default().bg(background);
    let (body_start, body_width) = centered_daily_body_axis(width, PAGE_PADDING_X);
    let mut lines = vec![
        line_with_background(Vec::new(), width, card_style),
        line_with_background(Vec::new(), width, card_style),
        line_with_background(
            vec![
                Span::raw(" ".repeat(body_start)),
                Span::styled(
                    label.to_string(),
                    Style::default()
                        .fg(if muted { theme.text_muted } else { label_color })
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ],
            width,
            card_style,
        ),
        line_with_background(Vec::new(), width, card_style),
    ];
    let mut markdown = crate::markdown::render_at_width(text, body_width, theme);
    if muted {
        for line in &mut markdown.lines {
            for span in &mut line.spans {
                span.style = span.style.fg(theme.text_muted);
            }
        }
    }
    let body_row = lines.len();
    let links = markdown
        .links
        .into_iter()
        .map(|mut link| {
            link.row += body_row;
            link.column += body_start;
            link
        })
        .collect();
    let images = markdown
        .images
        .into_iter()
        .map(|mut image| {
            image.row += body_row;
            image.column += body_start;
            image
        })
        .collect();
    for markdown_line in markdown.lines {
        for body in wrap_spans_to_width(&markdown_line.spans, body_width) {
            let mut spans = Vec::with_capacity(body.len() + 1);
            spans.push(Span::raw(" ".repeat(body_start)));
            spans.extend(body);
            lines.push(line_with_background(spans, width, card_style));
        }
    }
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(line_with_background(Vec::new(), width, card_style));
    lines.push(Line::default());
    (lines, links, images)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tempfile::tempdir;

    use super::*;
    use crate::agent_session::{AgentPanelEntry, TokenUsage};
    use crate::storage::Storage;

    fn make_app() -> (App, tempfile::TempDir) {
        let directory = tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        (App::new(storage).unwrap(), directory)
    }

    fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let _ = draw(frame, app);
            })
            .unwrap();
        terminal
    }

    fn open_chat(app: &mut App) {
        app.center_view = CenterView::Chat;
        app.focus = Focus::Center;
        app.workspace_view_index = WorkspaceView::index_of(CenterView::Chat).unwrap();
        app.agent_scroll = u16::MAX;
    }

    fn find_text(terminal: &Terminal<TestBackend>, needle: &str) -> (u16, u16) {
        let buffer = terminal.backend().buffer();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let mut candidate = String::new();
                for column in x..buffer.area.width {
                    candidate.push_str(buffer[(column, y)].symbol());
                    if candidate == needle {
                        return (x, y);
                    }
                    if candidate.len() >= needle.len() || !needle.starts_with(&candidate) {
                        break;
                    }
                }
            }
        }
        panic!("missing text {needle:?}");
    }

    fn area_text(terminal: &Terminal<TestBackend>, area: Rect) -> String {
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn chat_renders_message_cards_and_plain_tool_activity() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        app.agent_panel = vec![
            AgentPanelEntry::Prompt {
                text: "hello user".to_string(),
                muted: false,
            },
            AgentPanelEntry::Tool {
                text: "Reading\ndata/Note.md".to_string(),
                active: false,
            },
            AgentPanelEntry::Assistant {
                text: "hello agent".to_string(),
                streaming: true,
                final_output: false,
            },
        ];
        app.ai_running = true;
        app.agent_scroll = 0;

        let terminal = render(&mut app, 180, 30);
        let buffer = terminal.backend().buffer();
        let (user_x, user_y) = find_text(&terminal, "hello user");
        let (tool_x, tool_y) = find_text(&terminal, "Reading");
        let (agent_x, agent_y) = find_text(&terminal, "hello agent");

        assert_eq!(buffer[(user_x, user_y)].bg, app.theme.surface_message_user);
        assert_eq!(
            buffer[(agent_x, agent_y)].bg,
            app.theme.surface_message_agent
        );
        assert_ne!(buffer[(tool_x, tool_y)].bg, app.theme.surface_message_user);
        assert_ne!(buffer[(tool_x, tool_y)].bg, app.theme.surface_message_agent);
        assert_eq!(tool_x, user_x + 3);
        assert_eq!(agent_x, user_x);
        let center = app.layout.center.expect("chat center");
        let content = inset_horizontal(center_content_axis(center), 2);
        let messages_y = content.y.saturating_add(2);
        let card_y =
            messages_y + (app.agent_vlist.geometry.item_top(2) - app.agent_scroll as usize) as u16;
        assert_eq!(buffer[(content.x, card_y)].symbol(), "┌");
        assert_eq!(
            buffer[(content.x, card_y)].fg,
            animated_color(0, app.animation_tick, app.theme)
        );
        assert!(app.layout.compose.is_some());
    }

    #[test]
    fn chat_omits_empty_agent_cards_and_spaces_tool_details() {
        let theme = Theme::default();
        let empty = render_chat_entry(
            &AgentPanelEntry::Assistant {
                text: String::new(),
                streaming: false,
                final_output: false,
            },
            80,
            theme,
        )
        .0;
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].to_string(), "");
        assert_eq!(empty[0].style.bg, None);
        let compact_empty = render_agent_entry(
            &AgentPanelEntry::Assistant {
                text: String::new(),
                streaming: false,
                final_output: false,
            },
            40,
            0,
            false,
            theme,
        )
        .0;
        assert_eq!(compact_empty.len(), 1);
        assert_eq!(compact_empty[0].to_string(), "");
        assert_eq!(compact_empty[0].style.bg, None);

        let tool = render_chat_entry(
            &AgentPanelEntry::Tool {
                text: "Failed Create File: MBDown validation failed\ndata/gantt-2026-08.md"
                    .to_string(),
                active: false,
            },
            80,
            theme,
        )
        .0;
        assert_eq!(tool.len(), 3);
        let padding = " ".repeat(PAGE_PADDING_X);
        assert_eq!(
            tool[0].to_string(),
            format!("{padding} • Failed Create File")
        );
        assert!(tool[1].to_string().starts_with(&format!(
            "{padding}   └─ MBDown validation failed · data/gantt-2026-08.md"
        )));
        assert_eq!(tool[2].to_string(), "");
        assert_eq!(tool[2].style.bg, None);

        let ask_user = render_chat_tool(
            "Completed Ask User.\nChoose a format\nMBDown",
            80,
            0,
            false,
            theme,
        );
        assert_eq!(ask_user.len(), 4);
        assert_eq!(
            ask_user[0].to_string(),
            format!("{padding} • Completed Ask User.")
        );
        assert_eq!(
            ask_user[1].to_string(),
            format!("{padding}   ├─ Choose a format")
        );
        assert_eq!(ask_user[2].to_string(), format!("{padding}   └─ MBDown"));
        assert_eq!(ask_user[3].to_string(), "");

        let error = render_chat_entry(
            &AgentPanelEntry::Error("Agent failed: request timed out".to_string()),
            80,
            theme,
        )
        .0;
        assert_eq!(error.len(), 2);
        assert_eq!(
            error[0].to_string(),
            format!("{padding}Agent failed: request timed out")
        );
        assert_eq!(error[0].spans[1].style.fg, Some(theme.ui_error));
        assert_eq!(error[1].to_string(), "");
        assert_eq!(error[1].style.bg, None);
    }

    #[test]
    fn chat_replaces_the_agent_panel_with_session_statistics() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        app.agent_panel = vec![
            AgentPanelEntry::Prompt {
                text: "private prompt text".to_string(),
                muted: false,
            },
            AgentPanelEntry::Assistant {
                text: "private reply text".to_string(),
                streaming: false,
                final_output: true,
            },
            AgentPanelEntry::Tool {
                text: "Read data/Note.md".to_string(),
                active: false,
            },
        ];
        app.agent_usage = TokenUsage {
            input_tokens: 800,
            output_tokens: 200,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 200,
        };
        app.agent_context_window = 1_000;
        app.agent_context_capacity = 10_000;
        app.agent_timed_output_tokens = 200;
        app.agent_response_duration = Duration::from_secs(10);

        let terminal = render(&mut app, 180, 40);
        let statistics_area = app.layout.agent.expect("statistics panel");
        let statistics = area_text(&terminal, statistics_area);
        let (_, state_y) = find_text(&terminal, "State");
        let (_, context_y) = find_text(&terminal, "Context");

        assert!(statistics.contains("Agent statistics"));
        assert!(statistics.contains("Context"));
        assert!(statistics.contains("1k / 10k"));
        assert!(statistics.contains("20.0 t/s"));
        assert!(statistics.contains("1 user · 1 agent"));
        assert!(!statistics.contains("private prompt text"));
        assert!(!statistics.contains("private reply text"));
        assert_eq!(state_y, statistics_area.y + 2);
        assert_eq!(context_y, state_y + 2);
    }

    #[test]
    fn enter_in_chat_compose_sends_to_the_agent_instead_of_daily() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        app.focus = Focus::Compose;
        app.ai_running = true;
        app.input = "answer this".to_string();
        app.input_cursor = app.input.chars().count();

        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(app.input.is_empty());
        assert!(matches!(
            app.agent_panel.last(),
            Some(AgentPanelEntry::Prompt { text, muted: true }) if text == "answer this"
        ));
    }
}
