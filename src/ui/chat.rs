use super::*;

/// Horizontal padding (columns) shared by every chat block. The left pad also
/// carries the user prompt's "> " prefix and the thinking spinner.
const CHAT_BLOCK_PAD: usize = 1;

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
    let tail_pinned = app.agent_follow_tail || app.agent_scroll == u16::MAX;
    let maximum = app.agent_vlist.geometry.max_scroll(view_height);
    let mut scroll = if tail_pinned {
        maximum
    } else {
        (app.agent_scroll as usize).min(maximum)
    };
    scroll = measure_visible_agent_entries(app, scroll, view_height, tail_pinned);
    evict_agent_caches(app, scroll, view_height);
    app.agent_scroll = scroll.min(u16::MAX as usize) as u16;
    if scroll >= app.agent_vlist.geometry.max_scroll(view_height) {
        app.agent_follow_tail = true;
    }
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
        crate::agent_session::AgentPanelEntry::Prompt { text, muted } => {
            render_chat_prompt_bar(text, *muted, width, theme)
        }
        crate::agent_session::AgentPanelEntry::Assistant { text, .. } if text.trim().is_empty() => {
            (vec![Line::default()], Vec::new(), Vec::new())
        }
        // Ordinary agent text renders directly without a card.
        crate::agent_session::AgentPanelEntry::Assistant { text, .. } => {
            render_chat_plain_text(text, width, theme)
        }
        crate::agent_session::AgentPanelEntry::Thinking { text, streaming } => {
            render_chat_thinking_box(text, *streaming, width, 0, theme)
        }
        crate::agent_session::AgentPanelEntry::Tool { text, preview, .. } => (
            render_chat_tool(text, preview.as_deref(), width, 0, false, theme),
            Vec::new(),
            Vec::new(),
        ),
        crate::agent_session::AgentPanelEntry::Error(text) => {
            let (body_start, _) = centered_daily_body_axis(width, CHAT_BLOCK_PAD);
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
    preview: Option<&str>,
    width: usize,
    tick: u64,
    animate: bool,
    theme: Theme,
) -> Vec<Line<'static>> {
    let (body_start, body_width) = centered_daily_body_axis(width, CHAT_BLOCK_PAD);
    let (status, mut details) = activity_parts(text);
    // A successful result preview is the last detail row: it reuses the shared
    // activity-tree `├─`/`└─` glyphs and indentation instead of a chat-only
    // marker. Previews are withheld for structured (JSON) results upstream in
    // `tool_result_preview`.
    if let Some(preview) = preview {
        details.push(preview.to_string());
    }
    let mut lines = activity_rows(&status, &details, body_width, tick, animate, theme)
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

/// User messages render as a selection-style horizontal bar: a lighter
/// background with a "> " prefix on the first row and continuation rows
/// indented to align under it, one thin pad row above and below, then a shared
/// blank row as outer margin.
fn render_chat_prompt_bar(
    text: &str,
    muted: bool,
    width: usize,
    theme: Theme,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let background = theme.selection_background;
    let style = Style::default().bg(background);
    let prefix = "> ";
    let body_width = width
        .saturating_sub(CHAT_BLOCK_PAD * 2 + prefix.len())
        .max(1);
    let mut markdown = crate::markdown::render_at_width(text, body_width, theme);
    if muted {
        for line in &mut markdown.lines {
            for span in &mut line.spans {
                span.style = span.style.fg(theme.text_muted);
            }
        }
    }
    let mut lines = vec![line_with_background(Vec::new(), width, style)];
    let body_row = lines.len();
    let lead_width = CHAT_BLOCK_PAD + prefix.len();
    let links = markdown
        .links
        .into_iter()
        .map(|mut link| {
            link.row += body_row;
            link.column += lead_width;
            link
        })
        .collect();
    let images = markdown
        .images
        .into_iter()
        .map(|mut image| {
            image.row += body_row;
            image.column += lead_width;
            image
        })
        .collect();
    for (index, markdown_line) in markdown.lines.into_iter().enumerate() {
        let lead = if index == 0 { prefix } else { "  " };
        for body in wrap_spans_to_width(&markdown_line.spans, body_width) {
            let mut spans = Vec::with_capacity(body.len() + 1);
            spans.push(Span::raw(" ".repeat(CHAT_BLOCK_PAD)));
            spans.push(Span::styled(
                lead,
                Style::default().fg(theme.selection_indicator),
            ));
            spans.extend(body);
            lines.push(line_with_background(spans, width, style));
        }
    }
    lines.push(line_with_background(Vec::new(), width, style));
    lines.push(Line::default());
    (lines, links, images)
}

/// Ordinary agent text renders directly, no card background and no loading
/// marker — streaming simply grows the text in place.
pub(super) fn render_chat_plain_text(
    text: &str,
    width: usize,
    theme: Theme,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let body_width = width.saturating_sub(CHAT_BLOCK_PAD * 2).max(1);
    let markdown = crate::markdown::render_at_width(text, body_width, theme);
    let links = markdown
        .links
        .into_iter()
        .map(|mut link| {
            link.column += CHAT_BLOCK_PAD;
            link
        })
        .collect();
    let images = markdown
        .images
        .into_iter()
        .map(|mut image| {
            image.column += CHAT_BLOCK_PAD;
            image
        })
        .collect();
    let mut lines = Vec::with_capacity(markdown.lines.len() + 1);
    for markdown_line in markdown.lines {
        let mut spans = Vec::with_capacity(markdown_line.spans.len() + 1);
        spans.push(Span::raw(" ".repeat(CHAT_BLOCK_PAD)));
        spans.extend(markdown_line.spans);
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    (lines, links, images)
}

/// Thinking content sits in a background-colored box with the braille spinner
/// and a left-aligned "thinking" label on its top row, one pad row above and
/// below, one column of horizontal padding, then a shared blank row as outer
/// margin.
pub(super) fn render_chat_thinking_box(
    text: &str,
    streaming: bool,
    width: usize,
    tick: u64,
    theme: Theme,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    let background = theme.surface_message_agent;
    let style = Style::default().bg(background);
    // The braille spinner rotates while streaming; a Unicode check mark marks
    // the finished state.
    let marker = if streaming {
        crate::ui::agent::spinner_frame(tick)
    } else {
        '\u{2713}'
    };
    let mut lines = vec![line_with_background(Vec::new(), width, style)];
    lines.push(line_with_background(
        vec![
            Span::raw(" ".repeat(CHAT_BLOCK_PAD)),
            Span::styled(
                marker.to_string(),
                Style::default()
                    .fg(theme.ui_activity_marker)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled("thinking", Style::default().fg(theme.text_muted)),
        ],
        width,
        style,
    ));
    // A pad row separates the label from the body.
    lines.push(line_with_background(Vec::new(), width, style));
    // Body rows align with the "thinking" label's first letter: one pad column
    // plus the spinner cell plus its trailing space. Thinking text stays muted.
    let body_indent = CHAT_BLOCK_PAD + 1 + 1;
    let body_width = width.saturating_sub(body_indent * 2).max(1);
    let mut markdown = crate::markdown::render_at_width(text, body_width, theme);
    for line in &mut markdown.lines {
        for span in &mut line.spans {
            span.style = span.style.fg(theme.text_muted);
        }
    }
    let body_row = lines.len();
    let links = markdown
        .links
        .into_iter()
        .map(|mut link| {
            link.row += body_row;
            link.column += body_indent;
            link
        })
        .collect();
    let images = markdown
        .images
        .into_iter()
        .map(|mut image| {
            image.row += body_row;
            image.column += body_indent;
            image
        })
        .collect();
    for markdown_line in markdown.lines {
        for body in wrap_spans_to_width(&markdown_line.spans, body_width) {
            let mut spans = Vec::with_capacity(body.len() + 1);
            spans.push(Span::raw(" ".repeat(body_indent)));
            spans.extend(body);
            lines.push(line_with_background(spans, width, style));
        }
    }
    lines.push(line_with_background(Vec::new(), width, style));
    lines.push(Line::default());
    (lines, links, images)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
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
        app.agent_scroll = 0;
        app.agent_follow_tail = true;
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

    fn chat_renders_prompt_bar_plain_text_and_tool_rows() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        app.agent_panel = vec![
            Arc::new(AgentPanelEntry::Prompt {
                text: "hello user".to_string(),
                muted: false,
            }),
            Arc::new(AgentPanelEntry::Tool {
                text: "Reading\ndata/Note.md".to_string(),
                active: false,
                preview: None,
            }),
            // A streaming reply renders as plain text, no spinner, no card.
            Arc::new(AgentPanelEntry::Assistant {
                text: "hello agent".to_string(),
                streaming: true,
                final_output: false,
            }),
        ];
        app.ai_running = true;
        app.agent_scroll = 0;

        let terminal = render(&mut app, 180, 30);
        let buffer = terminal.backend().buffer();
        let (user_x, user_y) = find_text(&terminal, "hello user");
        let (tool_x, _tool_y) = find_text(&terminal, "Reading");
        let (agent_x, agent_y) = find_text(&terminal, "hello agent");

        // The user message is a lighter-background bar.
        assert_eq!(buffer[(user_x, user_y)].bg, app.theme.selection_background);
        let (prefix_x, _) = find_text(&terminal, "> hello user");
        assert_eq!(prefix_x, user_x - 2);
        // Tool rows and plain agent text keep no card background.
        assert_ne!(buffer[(tool_x, _tool_y)].bg, app.theme.selection_background);
        assert_ne!(
            buffer[(tool_x, _tool_y)].bg,
            app.theme.surface_message_agent
        );
        assert_ne!(
            buffer[(agent_x, agent_y)].bg,
            app.theme.surface_message_user
        );
        assert_ne!(
            buffer[(agent_x, agent_y)].bg,
            app.theme.surface_message_agent
        );
        assert!(app.layout.compose.is_some());
    }

    #[test]
    fn assistant_streaming_and_completed_share_plain_shape() {
        let theme = Theme::default();
        let width = 160;
        let text = "A reply that stays plain while its state changes.";
        let streaming = render_chat_entry(
            &AgentPanelEntry::Assistant {
                text: text.to_string(),
                streaming: true,
                final_output: false,
            },
            width,
            theme,
        )
        .0;
        let completed = render_chat_entry(
            &AgentPanelEntry::Assistant {
                text: text.to_string(),
                streaming: false,
                final_output: false,
            },
            width,
            theme,
        )
        .0;

        assert_eq!(streaming.len(), completed.len());
        // Neither state carries a spinner; plain text renders identically.
        assert!(!streaming
            .iter()
            .any(|line| line.to_string().contains("\u{28fe}")));
        assert!(!completed
            .iter()
            .any(|line| line.to_string().contains("\u{28fe}")));
        let card_background = |line: &Line<'static>| {
            line.spans.iter().any(|span| {
                span.style.bg == Some(theme.surface_message_agent)
                    || span.style.bg == Some(theme.surface_message_user)
            })
        };
        assert!(!streaming.iter().any(card_background));
        assert!(!completed.iter().any(card_background));
        assert!(streaming.iter().any(|line| line.to_string().contains(text)));
        assert!(completed.iter().any(|line| line.to_string().contains(text)));
    }

    #[test]
    fn chat_entry_height_estimates_stay_close_to_rendered_rows() {
        let theme = Theme::default();
        let width = 160;
        let cases: Vec<(&str, AgentPanelEntry)> = vec![
            (
                "prompt",
                AgentPanelEntry::Prompt {
                    text: "Summarize the note and the agent panel design.".repeat(3),
                    muted: false,
                },
            ),
            (
                "final",
                AgentPanelEntry::Assistant {
                    text: "The final answer wraps the whole analysis with details.".repeat(6),
                    streaming: false,
                    final_output: true,
                },
            ),
            (
                "streaming",
                AgentPanelEntry::Assistant {
                    text: "In progress reply text that grows while streaming.".repeat(5),
                    streaming: true,
                    final_output: false,
                },
            ),
            (
                "thinking",
                AgentPanelEntry::Thinking {
                    text: "Let me read the file first.".to_string(),
                    streaming: false,
                },
            ),
            (
                "tool",
                AgentPanelEntry::Tool {
                    text: "Completed Read File.\ndata/Note.md".to_string(),
                    active: false,
                    preview: Some("first line of the note".to_string()),
                },
            ),
        ];
        for (name, entry) in cases {
            let estimate = crate::ui::agent::estimated_agent_entry_height(
                &entry,
                width,
                crate::app::AgentEntryRenderStyle::Cards,
            );
            let actual = render_chat_entry(&entry, width, theme).0.len();
            assert!(
                estimate.abs_diff(actual) <= 4,
                "{name}: estimate {estimate} vs actual {actual}"
            );
        }
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
                preview: None,
            },
            80,
            theme,
        )
        .0;
        assert_eq!(tool.len(), 3);
        let padding = " ".repeat(CHAT_BLOCK_PAD);
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
            None,
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

        let with_preview = render_chat_tool(
            "Completed Read.\ndata/Note.md",
            Some("first line of the note content"),
            80,
            0,
            false,
            theme,
        );
        assert_eq!(with_preview.len(), 4);
        assert_eq!(
            with_preview[0].to_string(),
            format!("{padding} • Completed Read.")
        );
        // The detail row and the result preview share the activity-tree glyphs:
        // `├─` for the path, then the final `└─` for the preview, no chat-only ↳.
        assert_eq!(
            with_preview[1].to_string(),
            format!("{padding}   ├─ data/Note.md")
        );
        assert_eq!(
            with_preview[2].to_string(),
            format!("{padding}   └─ first line of the note content")
        );
        assert_eq!(with_preview[2].spans[2].style.fg, Some(theme.text_muted));
        assert_eq!(with_preview[3].to_string(), "");

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
            Arc::new(AgentPanelEntry::Prompt {
                text: "private prompt text".to_string(),
                muted: false,
            }),
            Arc::new(AgentPanelEntry::Assistant {
                text: "private reply text".to_string(),
                streaming: false,
                final_output: true,
            }),
            Arc::new(AgentPanelEntry::Tool {
                text: "Read data/Note.md".to_string(),
                active: false,
                preview: None,
            }),
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
    fn chat_final_reply_is_plain_text_and_thinking_is_boxed() {
        let theme = Theme::default();
        let plain = render_chat_entry(
            &AgentPanelEntry::Assistant {
                text: "The finished answer".to_string(),
                streaming: false,
                final_output: true,
            },
            180,
            theme,
        )
        .0;

        // Final reply: direct plain text, no card background.
        assert!(plain
            .iter()
            .any(|line| line.to_string().contains("The finished answer")));
        assert!(plain.iter().all(|line| {
            !line.spans.iter().any(|span| {
                span.style.bg == Some(theme.surface_message_agent)
                    || span.style.bg == Some(theme.surface_message_user)
            })
        }));

        // Thinking: background box with spinner and left-aligned label on its
        // top row while streaming.
        let boxed = render_chat_entry(
            &AgentPanelEntry::Thinking {
                text: "Let me check first".to_string(),
                streaming: true,
            },
            180,
            theme,
        )
        .0;
        // boxed[0] is the top pad row; boxed[1] carries spinner + label.
        assert_eq!(boxed[0].to_string().trim(), "");
        let top = &boxed[1];
        assert!(top.to_string().starts_with(" \u{28fe} thinking"));
        assert!(top
            .spans
            .iter()
            .any(|span| { span.style.bg == Some(theme.surface_message_agent) }));
        // The box carries a pad row below the content rows.
        assert!(
            boxed.last().unwrap().to_string().trim().is_empty()
                || boxed[boxed.len() - 2].to_string().trim().is_empty()
        );
        assert!(boxed
            .iter()
            .any(|line| line.to_string().contains("Let me check first")));
        // Thinking body text is muted; a pad row separates label from body.
        assert!(boxed[2].to_string().trim().is_empty());
        assert!(boxed.iter().any(|line| {
            line.to_string().contains("Let me check first")
                && line
                    .spans
                    .iter()
                    .any(|span| span.style.fg == Some(theme.text_muted))
        }));
        assert!(boxed.iter().all(|line| {
            line.spans
                .iter()
                .all(|span| span.style.bg == Some(theme.surface_message_agent))
                || line.to_string().trim().is_empty()
        }));

        // Completed thinking swaps the spinner for a Unicode check mark.
        let done = render_chat_entry(
            &AgentPanelEntry::Thinking {
                text: "Let me check first".to_string(),
                streaming: false,
            },
            180,
            theme,
        )
        .0;
        assert!(done[1].to_string().starts_with(" \u{2713} thinking"));
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
            app.agent_panel.last().map(|entry| entry.as_ref()),
            Some(AgentPanelEntry::Prompt { text, muted: true }) if text == "answer this"
        ));
    }

    #[test]
    fn chat_renders_running_tool_with_animated_spinner() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        app.ai_running = true;
        app.agent_panel = vec![Arc::new(AgentPanelEntry::Tool {
            text: "Calling Read File...\ndata/Note.md".to_string(),
            active: true,
            preview: None,
        })];
        app.agent_scroll = 0;
        let terminal = render(&mut app, 180, 30);
        let screen = area_text(&terminal, Rect::new(0, 0, 180, 30));
        assert!(
            screen.contains("Calling Read File"),
            "active tool not rendered:\n{screen}"
        );
        let (_, spinner_y) = find_text(&terminal, "\u{28fe} Calling Read File");
        let (_, detail_y) = find_text(&terminal, "data/Note.md");
        assert!(spinner_y < detail_y);
    }

    #[test]
    fn chat_streaming_preserves_manual_scroll_position() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        app.agent_panel = (0..40)
            .map(|index| {
                Arc::new(AgentPanelEntry::Thinking {
                    text: format!("Thinking round {index}"),
                    streaming: false,
                })
            })
            .collect();

        let _ = render(&mut app, 180, 30);
        let bottom = app.agent_scroll;
        assert!(bottom > 6);
        app.agent_follow_tail = false;
        app.agent_scroll = bottom - 6;
        let _ = render(&mut app, 180, 30);
        let settled = app.agent_scroll;
        assert!(!app.agent_follow_tail);

        app.agent_panel.push(Arc::new(AgentPanelEntry::Thinking {
            text: "New streamed thought".to_string(),
            streaming: true,
        }));
        let _ = render(&mut app, 180, 30);

        assert_eq!(app.agent_scroll, settled);
        assert!(!app.agent_follow_tail);

        app.agent_scroll = u16::MAX;
        let _ = render(&mut app, 180, 30);
        assert!(app.agent_follow_tail);
    }

    #[test]
    fn chat_streaming_thinking_keeps_body_and_outer_blank_when_measure_budget_exhausted() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        // Fill the viewport with finished thinking boxes plus one streamed box at
        // the tail. With the old shared estimate (2 rows) the streamed box's real
        // height (7) was clipped whenever the measure budget cut it from a frame;
        // the estimate must match the rendered rows so unmeasured frames still show
        // the body and its outer blank row.
        app.agent_panel = (0..48)
            .map(|index| {
                Arc::new(AgentPanelEntry::Thinking {
                    text: format!("Earlier thought {index}"),
                    streaming: false,
                })
            })
            .collect();
        app.agent_panel.push(Arc::new(AgentPanelEntry::Thinking {
            text: "Streamed thought".to_string(),
            streaming: true,
        }));
        app.agent_follow_tail = true;

        let terminal = render(&mut app, 180, 60);
        let (_, body_y) = find_text(&terminal, "Streamed thought");
        let buffer = terminal.backend().buffer();
        // Bottom pad row carries the box background right under the body…
        assert_eq!(
            buffer[(40, body_y + 1)].bg,
            app.theme.surface_message_agent,
            "pad row below the thinking body must be visible"
        );
        // …and the outer blank row sits below it without a background.
        assert_ne!(
            buffer[(40, body_y + 2)].bg,
            app.theme.surface_message_agent,
            "outer blank row below the thinking box must be visible"
        );
    }

    #[test]
    fn chat_remeasures_completed_tool_after_streaming_finishes() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        // The running tool is volatile: measured at the active height, never
        // cached, so no `Arc` reference keeps its allocation alive.
        app.agent_panel.push(Arc::new(AgentPanelEntry::Tool {
            text: "Calling Read File...\ndata/Note.md".to_string(),
            active: true,
            preview: None,
        }));
        app.agent_scroll = 0;
        let _ = render(&mut app, 180, 30);
        let active_height = app.agent_vlist.geometry.height(0);
        assert!(app.agent_vlist.caches[0].is_none());

        // Completing the tool mutates the entry in place (`Arc::make_mut` keeps
        // the pointer because the volatile entry has no cached Arc reference).
        let before = Arc::as_ptr(&app.agent_panel[0]);
        let entry = &mut app.agent_panel[0];
        if let AgentPanelEntry::Tool {
            text,
            active,
            preview,
        } = Arc::make_mut(entry)
        {
            *text = "Completed Read File.\ndata/Note.md".to_string();
            *active = false;
            *preview = Some("first line of the note".to_string());
        }
        assert_eq!(Arc::as_ptr(&app.agent_panel[0]), before);

        // The next frame must invalidate the stale active measurement and
        // re-measure the completed entry, or its result preview row and the
        // trailing blank are clipped by the old height.
        let terminal = render(&mut app, 180, 30);
        let screen = area_text(&terminal, Rect::new(0, 0, 180, 30));
        assert!(screen.contains("Completed Read File."), "{screen}");
        assert!(screen.contains("data/Note.md"), "{screen}");
        assert!(screen.contains("first line of the note"), "{screen}");
        let completed_height = app.agent_vlist.geometry.height(0);
        assert!(completed_height > active_height);
        assert!(app.agent_vlist.caches[0].is_some());
    }

    #[test]
    fn chat_live_visible_lines_match_fresh_entry_renders() {
        let (mut app, _directory) = make_app();
        open_chat(&mut app);
        let entries = vec![
            Arc::new(AgentPanelEntry::Prompt {
                text: "Read the note".to_string(),
                muted: false,
            }),
            Arc::new(AgentPanelEntry::Tool {
                text: "Completed Read File.\ndata/Note.md".to_string(),
                active: false,
                preview: Some("first line of the note".to_string()),
            }),
            Arc::new(AgentPanelEntry::Thinking {
                text: "Let me check the note first.".to_string(),
                streaming: true,
            }),
            Arc::new(AgentPanelEntry::Assistant {
                text: "The reply text.".to_string(),
                streaming: true,
                final_output: false,
            }),
        ];
        app.agent_panel = entries;
        app.agent_scroll = 0;

        const WIDTH: usize = 120;
        crate::ui::agent::sync_chat_vlist(&mut app, WIDTH);
        crate::ui::agent::measure_visible_agent_entries(&mut app, 0, 60, true);
        let (visible, _, _) = crate::ui::agent::visible_agent_lines(&mut app, 0, 60);
        let expected = app
            .agent_panel
            .iter()
            .flat_map(|entry| render_chat_entry(entry, WIDTH, app.theme).0)
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        let actual = visible
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn chat_thinking_box_keeps_blank_separator_between_blocks() {
        let theme = Theme::default();
        let width = 120;
        let tool = render_chat_entry(
            &AgentPanelEntry::Tool {
                text: "Completed Read.\ndata/Note.md".to_string(),
                active: false,
                preview: None,
            },
            width,
            theme,
        )
        .0;
        let thinking = render_chat_entry(
            &AgentPanelEntry::Thinking {
                text: "Let me check the note first.".to_string(),
                streaming: true,
            },
            width,
            theme,
        )
        .0;
        // Each block owns exactly one trailing outer blank row…
        assert!(tool.last().unwrap().to_string().trim().is_empty());
        assert_eq!(tool.last().unwrap().style.bg, None);
        // …so the streamed thinking box always has one blank row above its
        // backgrounded top pad row.
        assert!(thinking[0].to_string().trim().is_empty());
        assert!(thinking[0]
            .spans
            .iter()
            .any(|span| span.style.bg == Some(theme.surface_message_agent)));
        // The box itself ends with its own trailing outer blank row.
        assert!(thinking.last().unwrap().to_string().trim().is_empty());
        assert_eq!(thinking.last().unwrap().style.bg, None);
        // The label row sits right under the top pad, spinner preserved.
        assert!(thinking[1].to_string().starts_with(" \u{28fe} thinking"));
    }

    #[test]
    fn chat_directory_tool_detail_appears_immediately_after_activity_row() {
        let theme = Theme::default();
        let padding = " ".repeat(CHAT_BLOCK_PAD);
        // A running directory read: the path detail row follows the activity row
        // at once with the shared tree glyph, and the braille spinner is kept.
        let running = render_chat_tool(
            "Calling Read File...\ndata/Note.md",
            None,
            120,
            3,
            true,
            theme,
        );
        assert_eq!(
            running[0].to_string(),
            format!("{padding} \u{28bf} Calling Read File...")
        );
        assert_eq!(
            running[1].to_string(),
            format!("{padding}   └─ data/Note.md")
        );
        assert_eq!(running.len(), 3);
        assert_eq!(running[2].to_string(), "");

        // Completed: the directory path and the result preview both land
        // immediately after the activity row as tree rows, the preview last.
        let done = render_chat_tool(
            "Completed Read File.\ndata/Note.md",
            Some("first line of the note"),
            120,
            0,
            false,
            theme,
        );
        assert_eq!(done.len(), 4);
        assert_eq!(
            done[0].to_string(),
            format!("{padding} • Completed Read File.")
        );
        assert_eq!(done[1].to_string(), format!("{padding}   ├─ data/Note.md"));
        assert_eq!(
            done[2].to_string(),
            format!("{padding}   └─ first line of the note")
        );
        assert_eq!(done[3].to_string(), "");
    }
}
