use super::*;

pub(super) fn draw_files(
    frame: &mut Frame,
    app: &mut App,
    area: Rect,
    interactive: bool,
    cursor_position: &mut Option<Position>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let focused = app.focus == Focus::Files;
    let selection_visible =
        focused || app.center_view.sidebar_selection() == SidebarSelection::Files;
    let title = match app.files_context {
        FilesContext::Browse => " NólëBase ",
        FilesContext::Search => " NólëBase · search ",
        FilesContext::MoveTarget => " NólëBase · move to ",
        FilesContext::NewTarget => " NólëBase · new ",
        FilesContext::Rename => " NólëBase · rename ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::horizontal(PANEL_PADDING))
        .title(title)
        .style(Style::default().bg(app.theme.surface_panel))
        .border_style(focus_border(focused, app.theme));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (input_area, list_area) = match app.files_context {
        FilesContext::Search if inner.height > 0 => (
            Some(Rect::new(inner.x, inner.y, inner.width, 1)),
            Rect::new(
                inner.x,
                inner.y.saturating_add(2),
                inner.width,
                inner.height.saturating_sub(2),
            ),
        ),
        _ => (None, inner),
    };

    if let Some(input_area) = input_area {
        let (prompt, value) = match app.files_context {
            FilesContext::Search => ("/ ", app.file_query.as_str()),
            _ => ("", ""),
        };
        if let Some(position) = draw_single_line_input(
            frame,
            input_area,
            prompt,
            value,
            value.chars().count(),
            focused && interactive,
            app.theme,
        ) {
            *cursor_position = Some(position);
        }
        if inner.height > 1 {
            frame.render_widget(
                Paragraph::new("─".repeat(usize::from(inner.width)))
                    .style(Style::default().fg(app.theme.text_muted)),
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
            );
        }
    }

    if list_area.width == 0 || list_area.height == 0 {
        return;
    }

    let rows = app.visible_file_rows();
    if rows.is_empty() {
        let message = if app.files_context == FilesContext::Search && !app.file_query.is_empty() {
            "No matching notes"
        } else {
            "No notes yet"
        };
        frame.render_widget(
            Paragraph::new(message).alignment(Alignment::Center),
            list_area,
        );
        return;
    }

    let notes_count = app.note_files.iter().filter(|file| !file.archived).count();
    let archives_count = app.note_files.iter().filter(|file| file.archived).count();
    let searching = app.files_context == FilesContext::Search && !app.file_query.is_empty();
    let row_height = |row: &FileListRow| match row {
        FileListRow::Group(group) => {
            let has_visible_children = match group {
                FileGroup::Notes => (app.notes_expanded || searching) && notes_count > 0,
                FileGroup::Archives => (app.archives_expanded || searching) && archives_count > 0,
            };
            if has_visible_children {
                2
            } else {
                1
            }
        }
        FileListRow::File(_) => 3u16,
    };
    let selected_row = app.file_row.min(rows.len().saturating_sub(1));
    let row_heights = rows
        .iter()
        .map(|row| row_height(row) as usize)
        .collect::<Vec<_>>();
    let start = variable_selection_viewport_start(
        app.file_list_start,
        selected_row,
        &row_heights,
        list_area.height as usize,
    );
    app.file_list_start = start;

    let mut y = list_area.y;
    for (row_index, row) in rows.iter().copied().enumerate().skip(start) {
        if y >= list_area.y.saturating_add(list_area.height) {
            break;
        }
        let layout_height = row_height(&row).min(list_area.y + list_area.height - y);
        let selected = selection_visible && row_index == selected_row;
        let row_style = if selected {
            Style::default()
                .fg(app.theme.selection_foreground)
                .bg(if focused {
                    app.theme.selection_background
                } else {
                    app.theme.selection_background_inactive
                })
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let selection_area = if selected && matches!(row, FileListRow::File(_)) {
            Some(shared_selection_area(list_area, y, layout_height))
        } else {
            None
        };
        if let Some(selection_area) = selection_area {
            frame.render_widget(Block::default().style(row_style), selection_area);
        }
        match row {
            FileListRow::Group(group) => {
                let (label, expanded, count) = match group {
                    FileGroup::Notes => ("Notes", app.notes_expanded || searching, notes_count),
                    FileGroup::Archives => (
                        "Archives",
                        app.archives_expanded || searching,
                        archives_count,
                    ),
                };
                let marker = if expanded { "▼" } else { "▶" };
                let group_area = Rect::new(list_area.x, y, list_area.width, 1);
                frame.render_widget(Paragraph::new("").style(row_style), group_area);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            marker,
                            Style::default().fg(if selected {
                                app.theme.selection_foreground
                            } else {
                                app.theme.ui_group_marker
                            }),
                        ),
                        Span::raw(format!(" {label}")),
                    ]))
                    .style(row_style),
                    group_area,
                );
                let count = count.to_string();
                let count_width = (count.width() as u16).min(group_area.width);
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        count,
                        if selected {
                            Style::default()
                                .fg(app.theme.selection_foreground)
                                .add_modifier(Modifier::DIM)
                        } else {
                            Style::default().fg(app.theme.text_muted)
                        },
                    ))
                    .alignment(Alignment::Right),
                    Rect::new(
                        group_area.x + group_area.width.saturating_sub(count_width),
                        group_area.y,
                        count_width,
                        1,
                    ),
                );
                if interactive {
                    app.file_group_hitboxes.push(FileGroupHitbox {
                        group,
                        area: Rect::new(list_area.x, y, list_area.width, 1),
                    });
                }
            }
            FileListRow::File(absolute_index) => {
                let Some(file) = app.note_files.get(absolute_index) else {
                    continue;
                };
                let name = file
                    .path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?");
                let base_style = if file.archived && !selected {
                    row_style
                        .fg(app.theme.text_muted)
                        .add_modifier(Modifier::DIM)
                } else {
                    row_style
                };
                frame.render_widget(
                    Paragraph::new(Line::from(format!("  {name}"))).style(base_style),
                    Rect::new(list_area.x, y, list_area.width, 1),
                );
                let content_height = 2.min(layout_height);
                if content_height > 1 {
                    let modified: DateTime<Local> = file.modified.into();
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(
                            format!("  {}", modified.format("%y/%m/%d %H:%M")),
                            if selected {
                                Style::default()
                                    .fg(app.theme.selection_foreground)
                                    .add_modifier(Modifier::DIM)
                            } else {
                                Style::default().fg(app.theme.text_muted)
                            },
                        )))
                        .style(row_style),
                        Rect::new(list_area.x, y + 1, list_area.width, 1),
                    );
                }
                if interactive {
                    app.file_hitboxes.push(FileHitbox {
                        path: file.path.clone(),
                        area: Rect::new(list_area.x, y, list_area.width, content_height),
                    });
                }
                if let Some(selection_area) = selection_area {
                    draw_selection_indicator(frame, selection_area, app.theme);
                }
            }
        }
        y = y.saturating_add(layout_height);
    }
}
