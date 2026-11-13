use super::*;

pub(in crate::tui::ui) fn draw_select_dialog(
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
    let option_item_height = if dialog.purpose == DialogPurpose::SkillBrowser {
        3
    } else {
        SELECT_OPTION_HEIGHT
    };
    let option_height = selection_list_height(
        u16::try_from(option_items).unwrap_or(u16::MAX),
        option_item_height,
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
    let text_width = options.width.saturating_sub(2) as usize;
    let option_heights = dialog
        .wrap_options
        .then(|| wrapped_option_heights(dialog, text_width));
    let item_offsets: Vec<u16> = match &option_heights {
        Some(heights) => {
            let mut offsets = Vec::with_capacity(heights.len() + 1);
            let mut offset = 0_u16;
            for height in heights {
                offsets.push(offset);
                offset = offset.saturating_add(*height);
            }
            offsets.push(offset);
            offsets
        }
        None => Vec::new(),
    };
    let visible_items = visible_selection_items(options.height, option_item_height);
    let list_start = match &option_heights {
        Some(heights) => {
            let heights: Vec<usize> = heights.iter().map(|height| *height as usize).collect();
            variable_selection_viewport_start(
                dialog.scroll as usize,
                dialog.selected,
                &heights,
                options.height.saturating_sub(1) as usize,
            )
        }
        None => selection_viewport_start(
            dialog.scroll as usize,
            dialog.selected,
            visible_items,
            option_items,
        ),
    };
    if let Some(state) = app.dialog.as_mut() {
        state.scroll = u16::try_from(list_start).unwrap_or(u16::MAX);
    }
    let base_y = options.y.saturating_add(1);
    // Row offset of an item from the first list row; uniform lists multiply
    // by the fixed item height, Ask-User lists use per-item wrapped heights.
    let offset_of = |index: usize| -> u16 {
        if !item_offsets.is_empty() {
            return item_offsets
                .get(index.saturating_sub(list_start))
                .copied()
                .unwrap_or_default();
        }
        u16::try_from(index.saturating_sub(list_start))
            .unwrap_or(u16::MAX)
            .saturating_mul(option_item_height)
    };
    let options_end = options.y.saturating_add(options.height);
    for (index, option) in dialog.options.iter().enumerate() {
        if index < list_start {
            continue;
        }
        let y = base_y.saturating_add(offset_of(index));
        if y >= options_end {
            break;
        }
        let natural_height = option_heights
            .as_ref()
            .and_then(|heights| heights.get(index).copied())
            .unwrap_or(option_item_height);
        let item_height = natural_height.min(options_end.saturating_sub(y));
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
        let text_height = if dialog.wrap_options { item_height } else { 1 };
        let text_area = Rect::new(
            item_area.x.saturating_add(2),
            item_area.y,
            item_area.width.saturating_sub(2),
            text_height,
        );
        if dialog.wrap_options {
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
            let rows = wrap_spans_to_width(&spans, text_area.width as usize);
            let lines = rows.into_iter().map(Line::from).collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(lines), text_area);
        } else if dialog.purpose == DialogPurpose::SkillBrowser {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(label, style))),
                text_area,
            );
            if item_height > 1 {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        option.hint.clone().unwrap_or_default(),
                        if selected {
                            style.add_modifier(Modifier::DIM)
                        } else {
                            Style::default().fg(app.theme.text_muted)
                        },
                    ))),
                    Rect::new(text_area.x, text_area.y + 1, text_area.width, 1),
                );
            }
        } else {
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
            frame.render_widget(Paragraph::new(Line::from(spans)), text_area);
        }
        if let Some(selection_area) = selection_area {
            draw_selection_indicator(frame, selection_area, app.theme);
        }
        app.dialog_hitboxes
            .push(crate::tui::model::DialogOptionHitbox {
                index,
                area: item_area,
            });
        if dialog.purpose == DialogPurpose::WikiLinkChoice {
            app.wiki_link_hitboxes
                .push(crate::tui::model::WikiLinkHitbox {
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
        let y = base_y.saturating_add(offset_of(other_index));
        if other_index >= list_start && y < options_end {
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
            app.dialog_hitboxes
                .push(crate::tui::model::DialogOptionHitbox {
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
        DialogMode::SingleSelect if dialog.purpose == DialogPurpose::SkillBrowser => {
            "↑↓ choose · Enter preview · Esc close"
        }
        _ => "↑↓ choose · Enter open · Esc cancel",
    };
    draw_dialog_footer(frame, footer, footer_text, app.theme);
}
/// Per-option rows for wrapping option lists: wrapped label (and hint) rows
/// plus one separator row so long answers wrap instead of clipping.
pub(super) fn wrapped_option_heights(dialog: &DialogState, text_width: usize) -> Vec<u16> {
    dialog
        .options
        .iter()
        .map(|option| {
            let mut spans = vec![Span::raw(option.label.clone())];
            if let Some(hint) = &option.hint {
                spans.push(Span::raw(format!("  {hint}")));
            }
            let rows = wrap_spans_to_width(&spans, text_width).len().max(1) as u16;
            rows.saturating_add(1)
        })
        .collect()
}
