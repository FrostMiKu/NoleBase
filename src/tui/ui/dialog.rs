use super::*;
mod help;
mod selection;
pub(super) use help::help_lines;
pub(super) use selection::draw_select_dialog;
use selection::wrapped_option_heights;

pub(super) fn approval_dialog_width(root_width: u16) -> u16 {
    if root_width >= APPROVAL_SIDE_BY_SIDE_MIN_WIDTH.saturating_add(8) {
        APPROVAL_SIDE_BY_SIDE_WIDTH
    } else {
        APPROVAL_UNIFIED_WIDTH
    }
}

fn approval_diff_lines(message: &str, content_width: u16, theme: Theme) -> Vec<Line<'static>> {
    if content_width >= APPROVAL_SIDE_BY_SIDE_MIN_WIDTH {
        side_by_side_diff_lines(message, content_width as usize, theme)
    } else {
        unified_diff_lines(message, content_width as usize, theme)
    }
}

/// Body lines for an approval dialog. An empty diff (for example when the
/// agent deletes an empty file) receives a content placeholder so the panel
/// keeps its body and footer structure.
fn approval_content_lines(message: &str, content_width: u16, theme: Theme) -> Vec<Line<'static>> {
    let lines = approval_diff_lines(message, content_width, theme);
    if !lines.is_empty() {
        return lines;
    }
    vec![Line::from(Span::styled(
        "No changes to display",
        Style::default().fg(theme.text_muted),
    ))]
}

fn command_approval_lines(
    dialog: &DialogState,
    content_width: u16,
    theme: Theme,
) -> Vec<Line<'static>> {
    let width = content_width.max(1) as usize;
    let mut lines = wrap_spans_to_width(
        &[
            Span::styled(
                "Agent: ",
                Style::default()
                    .fg(theme.ui_action_ai)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(dialog.message.clone()),
        ],
        width,
    )
    .into_iter()
    .map(Line::from)
    .collect::<Vec<_>>();
    lines.push(Line::default());
    let command = dialog.command.as_ref();
    lines.push(Line::from(Span::styled(
        format!(
            "{}:",
            command.map_or("Cmd", |command| command.label.as_str())
        ),
        Style::default()
            .fg(theme.markdown_code_label)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::default());
    let code = command.map_or("", |command| command.code.as_str());
    for source_line in code.split('\n') {
        let highlighted = shell_highlight_line(source_line, theme);
        let wrapped = wrap_spans_to_width(&highlighted, width);
        if wrapped.is_empty() {
            lines.push(code_background_line(Vec::new(), width, theme));
        } else {
            lines.extend(
                wrapped
                    .into_iter()
                    .map(|spans| code_background_line(spans, width, theme)),
            );
        }
    }
    lines.push(Line::default());
    lines
}

fn code_background_line(
    mut spans: Vec<Span<'static>>,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    for span in &mut spans {
        span.style = span.style.bg(theme.markdown_code_block_background);
    }
    let used = spans.iter().map(|span| span.content.width()).sum::<usize>();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(theme.markdown_code_block_background),
        ));
    }
    Line::from(spans)
}

pub(super) fn shell_highlight_line(line: &str, theme: Theme) -> Vec<Span<'static>> {
    let chars = line.chars().collect::<Vec<_>>();
    let mut spans = Vec::new();
    let mut index = 0usize;
    let mut command_word = true;
    while index < chars.len() {
        let start = index;
        let character = chars[index];
        let color = if character.is_whitespace() {
            while index < chars.len() && chars[index].is_whitespace() {
                index += 1;
            }
            theme.markdown_code_block_text
        } else if character == '#' && (start == 0 || chars[start - 1].is_whitespace()) {
            index = chars.len();
            theme.text_muted
        } else if character == '\'' || character == '"' {
            let quote = character;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' && quote == '"' {
                    index = (index + 2).min(chars.len());
                } else if chars[index] == quote {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }
            command_word = false;
            theme.markdown_hashtag
        } else if character == '$' {
            index += 1;
            while index < chars.len()
                && (chars[index].is_alphanumeric() || "_{}()?@*#-".contains(chars[index]))
            {
                index += 1;
            }
            command_word = false;
            theme.ui_action_ai
        } else if "|&;<>".contains(character) {
            while index < chars.len() && "|&;<>".contains(chars[index]) {
                index += 1;
            }
            command_word = true;
            theme.ui_warning
        } else {
            while index < chars.len()
                && !chars[index].is_whitespace()
                && !"'\"$|&;<>".contains(chars[index])
            {
                index += 1;
            }
            let color = if command_word {
                theme.markdown_link
            } else {
                theme.markdown_code_block_text
            };
            command_word = false;
            color
        };
        let text = chars[start..index].iter().collect::<String>();
        spans.push(Span::styled(text, Style::default().fg(color)));
    }
    spans
}

/// Render every modal interaction through one fixed-width, bounded-height
/// command surface. The body changes by mode, but title, scrolling, option
/// selection, input and footer geometry remain identical.
pub(super) fn draw_dialog(
    frame: &mut Frame,
    app: &mut App,
    root: Rect,
    cursor_position: &mut Option<Position>,
) -> Rect {
    let Some(dialog) = app.dialog.clone() else {
        return Rect::new(root.x, root.y, 0, 0);
    };
    let width = match dialog.mode {
        DialogMode::Approval => approval_dialog_width(root.width),
        DialogMode::Informational => 92,
        DialogMode::CommandApproval | DialogMode::CommandPalette => DIALOG_WIDTH,
        _ => DIALOG_WIDTH,
    }
    .min(root.width.saturating_sub(4).max(root.width.min(1)));
    let text_width = width.saturating_sub(4).max(1) as usize;
    let approval_rows = (dialog.mode == DialogMode::Approval).then(|| {
        u16::try_from(approval_content_lines(&dialog.message, text_width as u16, app.theme).len())
            .unwrap_or(u16::MAX)
    });
    let command_approval_rows = (dialog.mode == DialogMode::CommandApproval).then(|| {
        u16::try_from(command_approval_lines(&dialog, text_width as u16, app.theme).len())
            .unwrap_or(u16::MAX)
    });
    let message_rows = if dialog.purpose == DialogPurpose::Help {
        saturating_u16(help_lines(app.theme).len())
    } else {
        saturating_u16(
            wrap_spans_to_width(&[Span::raw(dialog.message.clone())], text_width)
                .len()
                .max(1),
        )
    };
    let option_count = saturating_u16(dialog.options.len());
    let select_option_height = if dialog.purpose == DialogPurpose::SkillBrowser {
        3
    } else {
        SELECT_OPTION_HEIGHT
    };
    let option_heights = dialog
        .wrap_options
        .then(|| wrapped_option_heights(&dialog, text_width));
    let desired_height = match dialog.mode {
        DialogMode::Confirm => 5,
        DialogMode::SingleLine => 5,
        DialogMode::SecretLine => message_rows.min(8).saturating_add(3),
        DialogMode::FreeText => 11,
        DialogMode::SelectOrInput => message_rows
            .min(8)
            .saturating_add(match &option_heights {
                Some(heights) => 1_u16
                    .saturating_add(
                        heights
                            .iter()
                            .fold(0_u16, |total, height| total.saturating_add(*height)),
                    )
                    .saturating_add(SELECT_OPTION_HEIGHT),
                None => selection_list_height(option_count.saturating_add(1), SELECT_OPTION_HEIGHT),
            })
            .saturating_add(4)
            .saturating_add(1)
            .saturating_add(2),
        DialogMode::SingleSelect | DialogMode::MultiSelect => message_rows
            .min(8)
            .saturating_add(match &option_heights {
                Some(heights) => 1_u16.saturating_add(
                    heights
                        .iter()
                        .fold(0_u16, |total, height| total.saturating_add(*height)),
                ),
                None => selection_list_height(option_count, select_option_height),
            })
            .saturating_add(1)
            .saturating_add(2),
        DialogMode::Approval => approval_rows
            .unwrap_or_default()
            .saturating_add(3)
            .min(root.height.saturating_sub(4).min(36)),
        DialogMode::CommandApproval => command_approval_rows
            .unwrap_or_default()
            .saturating_add(3)
            .min(root.height.saturating_sub(4).min(36)),
        DialogMode::Informational => root.height.saturating_sub(2).min(30),
        DialogMode::CommandPalette => selection_list_height(option_count.min(8), 3)
            .saturating_add(6)
            .max(7),
    };
    let height = desired_height
        .max(3)
        .min(root.height.saturating_sub(2).max(root.height.min(1)));
    // Keep the command palette's query field anchored while its result list
    // shrinks. Centering each filtered height would make the input jump.
    let area = if dialog.mode == DialogMode::CommandPalette {
        let maximum_height = selection_list_height(8, 3).saturating_add(6);
        let anchor_height =
            maximum_height.min(root.height.saturating_sub(2).max(root.height.min(1)));
        let anchor = centered_rect(root, width, anchor_height);
        Rect::new(anchor.x, anchor.y, width, height)
    } else {
        centered_rect(root, width, height)
    };
    if area.width == 0 || area.height == 0 {
        return area;
    }
    clear_widget(frame, area);
    let destructive = matches!(
        dialog.purpose,
        DialogPurpose::DeleteDaily
            | DialogPurpose::DeleteFile
            | DialogPurpose::AgentDestructiveApproval
    );
    let warning = dialog.purpose == DialogPurpose::ExportOverwrite
        || (dialog.mode == DialogMode::Confirm && dialog.purpose == DialogPurpose::AgentApproval);
    let border = match dialog.mode {
        _ if destructive => app.theme.ui_error,
        _ if warning => app.theme.ui_warning,
        DialogMode::Approval | DialogMode::CommandApproval => app.theme.ui_warning,
        DialogMode::SingleLine | DialogMode::SecretLine | DialogMode::FreeText => {
            app.theme.ui_dialog_input
        }
        DialogMode::SelectOrInput
        | DialogMode::SingleSelect
        | DialogMode::MultiSelect
        | DialogMode::CommandPalette => app.theme.ui_dialog_choice,
        _ => app.theme.text_disabled,
    };
    let modal_background = if matches!(dialog.mode, DialogMode::SingleLine | DialogMode::SecretLine)
    {
        app.theme.surface_panel
    } else {
        app.theme.surface_overlay
    };
    let border_style = if dialog.mode == DialogMode::CommandPalette {
        focus_border(true, app.theme)
    } else {
        Style::default().fg(border)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(1))
        .title(format!(" {} ", dialog.title))
        .style(Style::default().bg(modal_background))
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return area;
    }

    app.dialog_hitboxes.clear();
    match dialog.mode {
        DialogMode::Confirm => {
            let body = Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(1),
            );
            frame.render_widget(
                Paragraph::new(dialog.message.clone())
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: false })
                    .style(if destructive {
                        Style::default()
                            .fg(app.theme.ui_error)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    }),
                body,
            );
            if destructive {
                draw_dialog_footer_line(
                    frame,
                    inner,
                    Line::from(vec![
                        Span::styled(
                            "Enter/Y confirm",
                            Style::default()
                                .fg(app.theme.ui_error)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" · ", Style::default().fg(app.theme.text_muted)),
                        Span::styled(
                            "N/Esc cancel",
                            Style::default().fg(app.theme.text_secondary),
                        ),
                    ]),
                );
            } else {
                draw_dialog_footer(frame, inner, "Enter/Y confirm · N/Esc cancel", app.theme);
            }
        }
        DialogMode::SingleLine => {
            let (input, footer) = split_last_row(inner);
            let single = Rect::new(input.x, input.y, input.width, 1);
            if let Some(position) = draw_single_line_input(
                frame,
                single,
                &dialog.message,
                &dialog.input,
                dialog.cursor,
                true,
                app.theme,
            ) {
                *cursor_position = Some(position);
            }
            draw_dialog_footer(frame, footer, "Enter save · Esc cancel", app.theme);
        }
        DialogMode::SecretLine => {
            let footer_y = inner.y + inner.height.saturating_sub(1);
            let input_y = footer_y.saturating_sub(1);
            let message = Rect::new(
                inner.x,
                inner.y,
                inner.width,
                input_y.saturating_sub(inner.y),
            );
            if message.height > 0 {
                frame.render_widget(
                    Paragraph::new(dialog.message.clone())
                        .wrap(Wrap { trim: false })
                        .style(Style::default().fg(app.theme.text_secondary)),
                    message,
                );
            }
            let input = Rect::new(inner.x, input_y, inner.width, 1);
            // The private-input path stores only bullets in `DialogState` so
            // cloning it for rendering clones only the bullets. Mask again at
            // the mode boundary to keep `SecretLine` safe for future callers.
            let masked = "•".repeat(dialog.input.chars().count());
            if let Some(position) = draw_single_line_input(
                frame,
                input,
                "Private input  ",
                &masked,
                dialog.cursor,
                true,
                app.theme,
            ) {
                *cursor_position = Some(position);
            }
            let footer = Rect::new(inner.x, footer_y, inner.width, 1);
            draw_dialog_footer(frame, footer, "Enter submit · Esc cancel", app.theme);
        }
        DialogMode::FreeText => {
            let (input, footer) = split_last_row(inner);
            if let Some(position) = draw_multiline_input(
                frame,
                input,
                &dialog.input,
                dialog.cursor,
                "Optional prompt; empty formats this daily note",
                true,
                app.theme,
            ) {
                *cursor_position = Some(position);
            }
            draw_dialog_footer(
                frame,
                footer,
                "Enter submit · Shift/Ctrl/Alt+Enter newline · Esc cancel",
                app.theme,
            );
        }
        DialogMode::Approval => {
            let (content, footer) = split_last_row(inner);
            let lines = approval_content_lines(&dialog.message, content.width, app.theme);
            let maximum = lines.len().saturating_sub(content.height as usize);
            let scroll = dialog.scroll.min(maximum as u16);
            if let Some(state) = app.dialog.as_mut() {
                state.scroll = scroll;
            }
            app.approval_scroll = scroll;
            frame.render_widget(
                Paragraph::new(visible_line_window(
                    &lines,
                    scroll as usize,
                    content.height as usize,
                )),
                content,
            );
            draw_dialog_footer(
                frame,
                footer,
                "Enter/Y approve · N/Esc deny · ↑↓ scroll · Tab mode",
                app.theme,
            );
        }
        DialogMode::CommandApproval => {
            let (content, footer) = split_last_row(inner);
            let lines = command_approval_lines(&dialog, content.width, app.theme);
            let maximum = lines.len().saturating_sub(content.height as usize);
            let scroll = dialog.scroll.min(maximum as u16);
            if let Some(state) = app.dialog.as_mut() {
                state.scroll = scroll;
            }
            app.approval_scroll = scroll;
            frame.render_widget(
                Paragraph::new(visible_line_window(
                    &lines,
                    scroll as usize,
                    content.height as usize,
                )),
                content,
            );
            draw_dialog_footer(
                frame,
                footer,
                "Enter/Y approve · N/Esc deny · ↑↓ scroll · Tab mode",
                app.theme,
            );
        }
        DialogMode::Informational => {
            let lines = help_lines(app.theme);
            let maximum = lines.len().saturating_sub(inner.height as usize);
            let scroll = dialog.scroll.min(maximum as u16);
            if let Some(state) = app.dialog.as_mut() {
                state.scroll = scroll;
            }
            app.help_scroll = scroll;
            frame.render_widget(
                Paragraph::new(visible_line_window(
                    &lines,
                    scroll as usize,
                    inner.height as usize,
                )),
                inner,
            );
        }
        DialogMode::CommandPalette => {
            draw_command_palette(frame, app, &dialog, inner, cursor_position)
        }
        DialogMode::SingleSelect | DialogMode::MultiSelect | DialogMode::SelectOrInput => {
            draw_select_dialog(frame, app, &dialog, inner, cursor_position);
        }
    }
    area
}

pub(super) fn draw_command_palette(
    frame: &mut Frame,
    app: &mut App,
    dialog: &DialogState,
    inner: Rect,
    cursor_position: &mut Option<Position>,
) {
    if inner.height == 0 {
        return;
    }
    let input = Rect::new(inner.x, inner.y, inner.width, 1);
    if let Some(position) = draw_single_line_input(
        frame,
        input,
        "/ ",
        &dialog.input,
        dialog.cursor,
        true,
        app.theme,
    ) {
        *cursor_position = Some(position);
    }
    if inner.height > 1 {
        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize))
                .style(Style::default().fg(app.theme.ui_border_subtle)),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }

    let footer_y = inner.y + inner.height.saturating_sub(1);
    let gap_y = footer_y.saturating_sub(1);
    let options = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        gap_y.saturating_sub(inner.y.saturating_add(2)),
    );
    let visible_items = visible_selection_items(options.height, 3);
    if dialog.options.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching commands")
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.theme.text_muted)),
            options,
        );
    } else if options.height > 0 {
        let list_start = selection_viewport_start(
            dialog.scroll as usize,
            dialog.selected,
            visible_items,
            dialog.options.len(),
        );
        if let Some(state) = app.dialog.as_mut() {
            state.scroll = u16::try_from(list_start).unwrap_or(u16::MAX);
        }
        let options_end = options.y.saturating_add(options.height);
        let mut y = options.y.saturating_add(1);
        for (index, option) in dialog
            .options
            .iter()
            .enumerate()
            .skip(list_start)
            .take(visible_items)
        {
            if y >= options_end {
                break;
            }
            let item_height = 3.min(options_end.saturating_sub(y));
            let item_area = Rect::new(options.x, y, options.width, item_height);
            let selected = index == dialog.selected;
            let selection_style = if selected {
                Style::default()
                    .fg(app.theme.selection_foreground)
                    .bg(app.theme.selection_background)
            } else {
                Style::default()
            };
            let selection_area = selected.then(|| shared_selection_area(options, y, item_height));
            if let Some(selection_area) = selection_area {
                frame.render_widget(Block::default().style(selection_style), selection_area);
            }
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        option.label.clone(),
                        Style::default()
                            .fg(if selected {
                                app.theme.selection_foreground
                            } else {
                                app.theme.text_secondary
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                ])),
                Rect::new(
                    options.x.saturating_add(1),
                    y,
                    options.width.saturating_sub(1),
                    1,
                ),
            );
            if item_height > 1 {
                let description = option.hint.as_deref().unwrap_or("");
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(
                            description,
                            if selected {
                                Style::default()
                                    .fg(app.theme.selection_foreground)
                                    .add_modifier(Modifier::DIM)
                            } else {
                                Style::default().fg(app.theme.text_muted)
                            },
                        ),
                    ])),
                    Rect::new(
                        options.x.saturating_add(1),
                        y + 1,
                        options.width.saturating_sub(1),
                        1,
                    ),
                );
            }
            if let Some(selection_area) = selection_area {
                draw_selection_indicator(frame, selection_area, app.theme);
            }
            app.dialog_hitboxes
                .push(crate::tui::model::DialogOptionHitbox {
                    index,
                    area: item_area,
                });
            y = y.saturating_add(item_height);
        }
    }
    draw_dialog_footer(
        frame,
        Rect::new(inner.x, footer_y, inner.width, 1),
        "↑↓ select · Enter run · Esc close",
        app.theme,
    );
}

pub(super) fn draw_dialog_footer(frame: &mut Frame, area: Rect, text: &str, theme: Theme) {
    draw_dialog_footer_line(
        frame,
        area,
        Line::styled(text.to_string(), Style::default().fg(theme.text_muted)),
    );
}

pub(super) fn draw_dialog_footer_line(frame: &mut Frame, area: Rect, line: Line<'static>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let footer = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        1,
    );
    frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), footer);
}
