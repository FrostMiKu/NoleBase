use super::*;

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
        DialogMode::CommandPalette => DIALOG_WIDTH,
        _ => DIALOG_WIDTH,
    }
    .min(root.width.saturating_sub(4).max(root.width.min(1)));
    let text_width = width.saturating_sub(4).max(1) as usize;
    let approval_rows = (dialog.mode == DialogMode::Approval).then(|| {
        u16::try_from(approval_diff_lines(&dialog.message, text_width as u16, app.theme).len())
            .unwrap_or(u16::MAX)
    });
    let message_rows = if dialog.purpose == DialogPurpose::Help {
        help_lines(app.theme).len() as u16
    } else {
        wrap_spans_to_width(&[Span::raw(dialog.message.clone())], text_width)
            .len()
            .max(1) as u16
    };
    let option_count = dialog.options.len() as u16;
    let desired_height = match dialog.mode {
        DialogMode::Confirm => 5,
        DialogMode::SingleLine => 5,
        DialogMode::FreeText => 11,
        DialogMode::SelectOrInput => message_rows
            .min(8)
            .saturating_add(selection_list_height(
                option_count.saturating_add(1),
                SELECT_OPTION_HEIGHT,
            ))
            .saturating_add(4)
            .saturating_add(1)
            .saturating_add(2),
        DialogMode::SingleSelect | DialogMode::MultiSelect => message_rows
            .min(8)
            .saturating_add(selection_list_height(option_count, SELECT_OPTION_HEIGHT))
            .saturating_add(1)
            .saturating_add(2),
        DialogMode::Approval => approval_rows
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
        DialogPurpose::DeleteDaily | DialogPurpose::DeleteFile
    );
    let border = match dialog.mode {
        _ if destructive => app.theme.ui_error,
        DialogMode::Approval => app.theme.ui_warning,
        DialogMode::SingleLine | DialogMode::FreeText => app.theme.ui_dialog_input,
        DialogMode::SelectOrInput
        | DialogMode::SingleSelect
        | DialogMode::MultiSelect
        | DialogMode::CommandPalette => app.theme.ui_dialog_choice,
        _ => app.theme.text_disabled,
    };
    let modal_background = if dialog.mode == DialogMode::SingleLine {
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
            let lines = approval_diff_lines(&dialog.message, content.width, app.theme);
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
                "Enter/Y approve · N/Esc deny · ↑↓ scroll · Tab bypass",
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
            app.dialog_hitboxes.push(crate::model::DialogOptionHitbox {
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

pub(super) fn draw_select_dialog(
    frame: &mut Frame,
    app: &mut App,
    dialog: &DialogState,
    inner: Rect,
    cursor_position: &mut Option<Position>,
) {
    if inner.height == 0 {
        return;
    }
    let has_input = dialog.mode == DialogMode::SelectOrInput;
    let footer_height = 1;
    let input_height = if has_input {
        4.min(inner.height.saturating_sub(footer_height))
    } else {
        0
    };
    let message_height = if dialog.message.is_empty() {
        0
    } else {
        wrap_spans_to_width(&[Span::raw(dialog.message.clone())], inner.width as usize)
            .len()
            .min(8) as u16
    };
    let available = inner
        .height
        .saturating_sub(footer_height)
        .saturating_sub(input_height);
    let option_capacity = available.saturating_sub(message_height);
    let option_items = dialog.options.len() + usize::from(has_input);
    let option_height = selection_list_height(
        u16::try_from(option_items).unwrap_or(u16::MAX),
        SELECT_OPTION_HEIGHT,
    )
    .min(option_capacity);
    let message = Rect::new(inner.x, inner.y, inner.width, message_height);
    let options = Rect::new(
        inner.x,
        message.y + message.height,
        inner.width,
        option_height,
    );
    let input = Rect::new(
        inner.x,
        options.y + options.height,
        inner.width,
        input_height,
    );
    let footer = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(footer_height),
        inner.width,
        footer_height,
    );
    if message.height > 0 {
        frame.render_widget(
            Paragraph::new(dialog.message.clone())
                .wrap(Wrap { trim: false })
                .style(Style::default().add_modifier(Modifier::BOLD)),
            message,
        );
    }
    let visible_items = visible_selection_items(options.height, SELECT_OPTION_HEIGHT);
    let list_start = selection_viewport_start(
        dialog.scroll as usize,
        dialog.selected,
        visible_items,
        option_items,
    );
    if let Some(state) = app.dialog.as_mut() {
        state.scroll = u16::try_from(list_start).unwrap_or(u16::MAX);
    }
    let options_end = options.y.saturating_add(options.height);
    for (index, option) in dialog
        .options
        .iter()
        .enumerate()
        .skip(list_start)
        .take(visible_items)
    {
        let row = index - list_start;
        let y = selection_item_y(options, row, SELECT_OPTION_HEIGHT);
        if y >= options_end {
            break;
        }
        let item_height = SELECT_OPTION_HEIGHT.min(options_end.saturating_sub(y));
        let item_area = Rect::new(options.x, y, options.width, item_height);
        let selected = dialog.selected == index;
        let style = if selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .bg(app.theme.selection_background)
        } else {
            Style::default().fg(app.theme.text_disabled)
        };
        let label = if dialog.mode == DialogMode::MultiSelect {
            let marker = if dialog.checked.get(index).copied().unwrap_or(false) {
                "[x]"
            } else {
                "[ ]"
            };
            format!("{marker} {}", option.label)
        } else {
            option.label.clone()
        };
        let mut spans = vec![Span::styled(label, style)];
        if let Some(hint) = &option.hint {
            spans.push(Span::styled(
                format!("  {hint}"),
                if selected {
                    style.add_modifier(Modifier::DIM)
                } else {
                    Style::default().fg(app.theme.text_muted)
                },
            ));
        }
        let selection_area = selected.then(|| shared_selection_area(options, y, item_height));
        if let Some(selection_area) = selection_area {
            frame.render_widget(
                Block::default().style(
                    Style::default()
                        .fg(app.theme.selection_foreground)
                        .bg(app.theme.selection_background),
                ),
                selection_area,
            );
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(
                item_area.x.saturating_add(2),
                item_area.y,
                item_area.width.saturating_sub(2),
                1,
            ),
        );
        if let Some(selection_area) = selection_area {
            draw_selection_indicator(frame, selection_area, app.theme);
        }
        app.dialog_hitboxes.push(crate::model::DialogOptionHitbox {
            index,
            area: item_area,
        });
        if dialog.purpose == DialogPurpose::WikiLinkChoice {
            app.wiki_link_hitboxes.push(crate::model::WikiLinkHitbox {
                index,
                area: item_area,
            });
        }
    }
    if has_input && input.height > 0 {
        let custom_selected = dialog.selected >= dialog.options.len();
        let input_block = Block::default()
            .borders(Borders::ALL)
            .title(" Your answer ")
            .border_style(focus_border(custom_selected, app.theme));
        let input_inner = input_block.inner(input);
        frame.render_widget(input_block, input);
        if let Some(position) = draw_multiline_input(
            frame,
            input_inner,
            &dialog.input,
            dialog.cursor,
            "Type a different response",
            custom_selected,
            app.theme,
        ) {
            *cursor_position = Some(position);
        }
        let other_index = dialog.options.len();
        if other_index >= list_start && other_index < list_start + visible_items {
            let row = other_index - list_start;
            let y = selection_item_y(options, row, SELECT_OPTION_HEIGHT);
            let item_height = SELECT_OPTION_HEIGHT.min(options_end.saturating_sub(y));
            let item_area = Rect::new(options.x, y, options.width, item_height);
            let selection_area =
                custom_selected.then(|| shared_selection_area(options, y, item_height));
            if let Some(selection_area) = selection_area {
                frame.render_widget(
                    Block::default().style(
                        Style::default()
                            .fg(app.theme.selection_foreground)
                            .bg(app.theme.selection_background),
                    ),
                    selection_area,
                );
            }
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "Other answer",
                    if custom_selected {
                        Style::default()
                            .fg(app.theme.selection_foreground)
                            .bg(app.theme.selection_background)
                    } else {
                        Style::default().fg(app.theme.text_disabled)
                    },
                ))),
                Rect::new(
                    item_area.x.saturating_add(2),
                    item_area.y,
                    item_area.width.saturating_sub(2),
                    1,
                ),
            );
            if let Some(selection_area) = selection_area {
                draw_selection_indicator(frame, selection_area, app.theme);
            }
            app.dialog_hitboxes.push(crate::model::DialogOptionHitbox {
                index: other_index,
                area: item_area,
            });
        }
    }
    let footer_text = match dialog.mode {
        DialogMode::MultiSelect => "↑↓ move · Space toggle · Enter submit · Esc cancel",
        DialogMode::SelectOrInput => "↑↓ choose · Enter submit · type custom · Esc cancel",
        DialogMode::SingleSelect if dialog.purpose == DialogPurpose::AskUser => {
            "↑↓ choose · Enter submit · Esc stop"
        }
        _ => "↑↓ choose · Enter open · Esc cancel",
    };
    draw_dialog_footer(frame, footer, footer_text, app.theme);
}

pub(super) fn help_lines(theme: Theme) -> Vec<Line<'static>> {
    let heading = |text: &str| {
        Line::from(Span::styled(
            text.to_string(),
            Style::default()
                .fg(theme.ui_section_heading)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let key = |keys: &str, description: &str| {
        Line::from(vec![
            Span::styled(
                format!(" {keys:<16}"),
                Style::default()
                    .fg(theme.ui_shortcut)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(description.to_string()),
        ])
    };
    vec![
        heading("Workspace"),
        key("f / t", "focus Files / Todo"),
        key("← → / ↑ ↓", "move focus between panes"),
        key("Tab", "toggle approve / bypass mode"),
        key("Ctrl+P", "open command palette"),
        key("#", "browse workspace tags"),
        key("Ctrl+`", "toggle workspace terminal"),
        key("Esc", "return / cancel"),
        key("?", "open this help"),
        Line::default(),
        heading("Daily"),
        key("i / Enter", "focus Compose"),
        key("j k / ↑ ↓", "select DailyNote"),
        key("m a n", "move · archive · new file"),
        key("v e d / AI", "view · edit · delete · Agent"),
        key("/ / u", "search · undo"),
        Line::default(),
        heading("Compose / editor"),
        key("Enter", "send / save"),
        key("Ctrl+Enter", "send prompt directly to Agent"),
        key("Ctrl+U", "recall the last append into Compose"),
        key("Ctrl+J", "insert newline"),
        key("Esc", "leave / cancel"),
        Line::default(),
        heading("Files"),
        key("j k / ↑ ↓", "select"),
        key("Enter / e", "open / external editor"),
        key("/ r d", "filter · rename · delete"),
        Line::default(),
        heading("Todo / document"),
        key("Enter / Space", "toggle todo"),
        key("j k / PgUp/Dn", "scroll document"),
        key("i / Enter", "append while reading"),
        Line::default(),
        heading("Agent approval"),
        key("Enter / y", "approve displayed diff"),
        key("n / Esc", "deny displayed diff"),
        Line::default(),
        heading("Agent questions"),
        key("↑ ↓ / Enter", "choose and submit an option"),
        key("type / Esc", "custom answer / cancel question"),
        Line::default(),
        heading("Agent output"),
        key("c", "cancel running Agent while panel is focused"),
        key("C", "clear Agent conversation and start a new session"),
    ]
}
