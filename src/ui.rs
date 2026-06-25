//! Terminal rendering: chat layout, message cards, buttons, and modals.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Mode};
use crate::model::{Action, ButtonHitbox, FileHitbox};

const TIME_FMT: &str = "%H:%M";

/// Render the whole app. Rebuilds hitboxes for the current frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    app.hitboxes.clear();
    app.file_hitboxes.clear();

    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),    // messages
            Constraint::Length(8), // input (Weibo-style compose box)
            Constraint::Length(1), // footer: header + hints (bottom)
        ])
        .split(area);

    draw_messages(f, app, chunks[0]);
    draw_input(f, app, chunks[1]);
    draw_footer(f, app, chunks[2]);

    match app.mode {
        Mode::SelectTarget => draw_select_target(f, app),
        Mode::NewFile => draw_new_file(f, app),
        Mode::ConfirmDelete => draw_confirm_delete(f, app),
        Mode::Preview => draw_preview(f, app),
        Mode::FileList => draw_file_list(f, app),
        Mode::Normal | Mode::Insert => {}
    }
}

fn draw_file_list(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let w = (area.width / 2).clamp(40, 70);
    let count = app.sidebar_files.len() as u16;
    let h = count
        .saturating_add(4)
        .min(area.height.saturating_sub(4))
        .max(7);
    let rect = centered(area, w, h);
    f.render_widget(Clear, rect);

    if app.sidebar_files.is_empty() {
        let hint = Paragraph::new("No files yet. Move a message with `m` or `n` first.")
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Files  (Esc close)"),
            );
        f.render_widget(hint, rect);
        return;
    }

    let items: Vec<ListItem> = app
        .sidebar_files
        .iter()
        .map(|p| {
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
            ListItem::new(name.to_string())
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Files  (↑↓ select · Enter/v preview · e edit · Esc close)"),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Green)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default();
    state.select(Some(app.sidebar_index));
    f.render_stateful_widget(list, rect, &mut state);

    // Register clickable rows for mouse support.
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    for (i, path) in app.sidebar_files.iter().enumerate() {
        let row = inner.y + i as u16;
        if row < inner.y + inner.height {
            app.file_hitboxes.push(FileHitbox {
                path: path.clone(),
                area: Rect {
                    x: inner.x,
                    y: row,
                    width: inner.width,
                    height: 1,
                },
            });
        }
    }
}

/// Bottom footer: `Note <path> [MODE]` (and status) on the left, keybinding
/// hints on the right — i.e. the hints sit in the bottom-right corner.
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let mode = match app.mode {
        Mode::Insert => Span::styled(
            " INSERT ",
            Style::default().bg(Color::Green).fg(Color::Black),
        ),
        Mode::Normal => Span::styled(
            " NORMAL ",
            Style::default().bg(Color::Blue).fg(Color::Black),
        ),
        _ => Span::raw(""),
    };
    let path = app.storage.chat_path.to_string_lossy();
    let mut left = vec![
        Span::styled(" Note ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(path.to_string()),
        Span::raw(" "),
        mode,
    ];
    if !app.status.is_empty() {
        left.push(Span::raw("  "));
        left.push(Span::styled(
            &app.status,
            Style::default().fg(Color::Yellow),
        ));
    }

    // The newline/send hint lives in the input toolbar, so the footer avoids
    // repeating it.
    let hint = match app.mode {
        Mode::Insert => "Tab/Esc → normal mode",
        Mode::Normal => {
            "t todo · m move · n new · v view · e edit · d del · f files · i insert · q quit"
        }
        Mode::FileList => "↑↓ select · Enter/v preview · e edit · Esc close",
        _ => "",
    };

    let hint_w = hint.width() as u16;
    let tb = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(hint_w)])
        .split(area);
    f.render_widget(Paragraph::new(Line::from(left)), tb[0]);
    if !hint.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray)))
                .alignment(Alignment::Right),
            tb[1],
        );
    }
}

fn draw_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default().borders(Borders::TOP).title("Chat");
    f.render_widget(block, area);

    if app.messages.is_empty() {
        let hint = Paragraph::new("No notes yet.").alignment(Alignment::Center);
        f.render_widget(hint, area);
        return;
    }

    // Inner content area (account for the top border).
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    };
    let width = inner.width as usize;

    // Build the full content. Each card = wrapped body lines + button row + blank.
    let mut lines: Vec<Line> = Vec::new();
    let mut card_first: Vec<usize> = Vec::with_capacity(app.messages.len());
    let mut button_lines: Vec<usize> = Vec::with_capacity(app.messages.len());

    for (idx, m) in app.messages.iter().enumerate() {
        card_first.push(lines.len());
        let selected = Some(idx) == Some(app.selected);

        let time = m.created_at.format(TIME_FMT).to_string();
        let prefix = format!("{time}  ");
        let prefix_w = prefix.width();
        let budget = width.saturating_sub(prefix_w).max(1);
        let body_style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        // Wrap the full body (respecting explicit newlines) to the card width.
        for (i, seg) in wrap_to_width(&m.body, budget).into_iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::styled(prefix.clone(), Style::default().fg(Color::DarkGray)),
                    Span::styled(seg, body_style),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(prefix_w)),
                    Span::styled(seg, body_style),
                ]));
            }
        }

        // Buttons row.
        button_lines.push(lines.len());
        lines.push(render_button_line(selected));

        // Blank separator.
        lines.push(Line::raw(""));
    }

    // Resolve effective scroll (u32::MAX means "stick to bottom").
    let total = lines.len() as u16;
    let view_h = inner.height;
    let max_scroll = total.saturating_sub(view_h);
    let scroll = if app.scroll > max_scroll {
        max_scroll
    } else {
        app.scroll
    };
    app.scroll = scroll; // clamp persisted value

    // Keep the selected card (and its buttons) visible.
    if let Some(&first) = card_first.get(app.selected) {
        let first = first as u16;
        let btn = button_lines[app.selected] as u16;
        if first < app.scroll {
            app.scroll = first;
        } else if btn >= app.scroll + view_h {
            app.scroll = btn.saturating_sub(view_h.saturating_sub(1));
        }
    }
    let scroll = app.scroll;

    // Record button hitboxes for visible cards.
    for (idx, m) in app.messages.iter().enumerate() {
        let Some(&btn) = button_lines.get(idx) else {
            continue;
        };
        let abs_row = inner.y + btn as u16 - scroll;
        if abs_row < inner.y || abs_row >= inner.y + inner.height {
            continue;
        }
        register_buttons(&mut app.hitboxes, &m.id, inner.x + 1, abs_row);
    }

    let para = Paragraph::new(lines).scroll((scroll, 0));
    f.render_widget(para, inner);
}

/// Build the button row, returning it as a Line.
fn render_button_line(selected: bool) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    for (i, action) in Action::all().iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let label = format!("[{}]", action.label());
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan)
        };
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
}

/// Push hitboxes for each button on a given row.
fn register_buttons(hitboxes: &mut Vec<ButtonHitbox>, id: &str, x0: u16, y: u16) {
    let mut x = x0;
    for (i, action) in Action::all().iter().enumerate() {
        if i > 0 {
            x += 1; // separating space
        }
        let label = action.label();
        let width = label.len() as u16 + 2; // brackets
        hitboxes.push(ButtonHitbox {
            message_id: id.to_string(),
            action: *action,
            area: Rect {
                x,
                y,
                width,
                height: 1,
            },
        });
        x += width;
    }
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let focused = matches!(app.mode, Mode::Insert);
    let border = if focused {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default().borders(Borders::ALL).border_style(border);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Split the compose box into an editable text area and a Weibo-style
    // toolbar row (hint on the left, counts on the right).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // --- Text area: one Line per '\n' segment, with a prompt on line 1 ---
    let prompt = Span::styled("❯ ", Style::default().fg(Color::Green));
    let cont = Span::styled("  ", Style::default().fg(Color::DarkGray));
    let mut lines: Vec<Line> = Vec::new();
    if app.input.is_empty() {
        lines.push(Line::from(vec![
            prompt,
            Span::styled("记录点什么…", Style::default().fg(Color::DarkGray)),
        ]));
    } else {
        for (i, seg) in app.input.split('\n').enumerate() {
            let lead = if i == 0 { prompt.clone() } else { cont.clone() };
            lines.push(Line::from(vec![lead, Span::raw(seg.to_string())]));
        }
    }

    // Keep the latest lines visible as the buffer grows past the text area.
    let text_h = chunks[0].height as usize;
    let scroll = lines.len().saturating_sub(text_h) as u16;
    let para = Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false });
    f.render_widget(para, chunks[0]);

    // --- Toolbar: counts on the left, newline/send hint on the right ---
    let tb = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(44)])
        .split(chunks[1]);
    let line_count = if app.input.is_empty() {
        0
    } else {
        app.input.split('\n').count()
    };
    let char_count = app.input.chars().count();
    let count = format!("{line_count} 行 · {char_count} 字");
    f.render_widget(
        Paragraph::new(count).style(Style::default().fg(Color::DarkGray)),
        tb[0],
    );
    let hint = if focused {
        "Enter 换行 · Ctrl/Alt+Enter 发送"
    } else {
        "按 i 或点击进入输入"
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("⏎ ", Style::default().fg(Color::DarkGray)),
            Span::styled(hint, Style::default().fg(Color::DarkGray)),
        ]))
        .alignment(Alignment::Right),
        tb[1],
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let pop = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(width)) / 2),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(pop[1])[1]
}

fn draw_select_target(f: &mut Frame, app: &App) {
    let area = f.area();
    let w = (area.width / 2).clamp(40, 70);
    let h = (app.target_files.len() as u16 + 4).min(area.height.saturating_sub(4));
    let rect = centered(area, w, h);
    f.render_widget(Clear, rect);
    let items: Vec<ListItem> = app
        .target_files
        .iter()
        .map(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ListItem::new(name)
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Move to file  (↑↓ select · Enter move · v preview · Esc cancel)"),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default();
    state.select(Some(app.target_index));
    f.render_stateful_widget(list, rect, &mut state);
}

fn draw_new_file(f: &mut Frame, app: &App) {
    let area = f.area();
    let rect = centered(area, 50, 3);
    f.render_widget(Clear, rect);
    let para = Paragraph::new(format!("{}_", app.new_file_input)).block(
        Block::default()
            .borders(Borders::ALL)
            .title("New file name  (Enter create+move · Esc cancel)"),
    );
    f.render_widget(para, rect);
}

fn draw_confirm_delete(f: &mut Frame, _app: &App) {
    let area = f.area();
    let rect = centered(area, 40, 3);
    f.render_widget(Clear, rect);
    let para = Paragraph::new("Delete this message?  [y/N]")
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title("Delete"));
    f.render_widget(para, rect);
}

fn draw_preview(f: &mut Frame, app: &App) {
    let Some(p) = app.preview.as_ref() else {
        return;
    };
    let area = f.area();
    let w = (area.width * 4 / 5).max(50);
    let h = (area.height * 4 / 5).max(10);
    let rect = centered(area, w, h);
    f.render_widget(Clear, rect);
    let lines: Vec<Line> = p.lines.iter().map(|s| Line::raw(s.clone())).collect();
    let para = Paragraph::new(lines)
        .scroll((p.scroll, 0))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("{}  (↑↓ scroll · Esc close)", p.title)),
        );
    f.render_widget(para, rect);
}

/// Truncate a string to fit `width` display columns, appending "…" if cut.
/// Greedy char-width wrap of `s` to `width` display columns, preserving
/// explicit newlines as their own (possibly empty) output lines.
fn wrap_to_width(s: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for logical in s.split('\n') {
        if logical.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_w = 0usize;
        for c in logical.chars() {
            let cw = c.width().unwrap_or(1);
            if cur_w + cw > width && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cur.push(c);
            cur_w += cw;
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tempfile::tempdir;

    /// Flatten a rendered TestBackend buffer into a readable string.
    fn buffer_string(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let area = buf.area();
        let mut s = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn chat_renders_full_multiline_body() {
        let dir = tempdir().unwrap();
        let st = Storage::new(dir.path()).unwrap();
        st.ensure_files().unwrap();
        st.append_chat_message("alpha\nbeta\ngamma").unwrap();
        let mut app = App::new(st).unwrap();
        app.mode = Mode::Normal;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let s = buffer_string(&terminal);

        // The full multi-line body must render (not just the first line).
        for word in ["alpha", "beta", "gamma"] {
            assert!(s.contains(word), "{word} missing from chat render");
        }
        // Each word on its own line.
        let a = s.lines().find(|l| l.contains("alpha"));
        let b = s.lines().find(|l| l.contains("beta"));
        let c = s.lines().find(|l| l.contains("gamma"));
        assert_ne!(a, b, "alpha/beta must be on distinct lines");
        assert_ne!(b, c, "beta/gamma must be on distinct lines");
        // Buttons still present and registered.
        assert!(s.contains("[todo]"), "buttons missing");
        assert!(!app.hitboxes.is_empty(), "button hitboxes missing");
    }

    #[test]
    fn file_list_popup_shows_files() {
        let dir = tempdir().unwrap();
        let st = Storage::new(dir.path()).unwrap();
        st.ensure_files().unwrap();
        st.create_named_file("Work").unwrap();
        st.create_named_file("Ideas").unwrap();
        let mut app = App::new(st).unwrap();
        app.open_file_list(); // mode -> FileList, populates sidebar_files

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let s = buffer_string(&terminal);

        assert!(s.contains("Files"), "popup title missing");
        assert!(s.contains("TODO"), "TODO missing from popup");
        assert!(s.contains("Work"), "Work missing from popup");
        assert!(s.contains("Ideas"), "Ideas missing from popup");

        // File rows must register clickable hitboxes.
        assert!(!app.file_hitboxes.is_empty(), "no file hitboxes registered");
        assert!(
            app.file_hitboxes
                .iter()
                .any(|h| h.path.file_name().unwrap() == "Work.md"),
            "Work.md hitbox missing"
        );
    }

    #[test]
    fn input_renders_multiple_lines() {
        let dir = tempdir().unwrap();
        let st = Storage::new(dir.path()).unwrap();
        st.ensure_files().unwrap();
        let mut app = App::new(st).unwrap();
        // Two segments fit in the input box; both must be visible on distinct
        // lines (no glyph collapse).
        app.input = "alpha\nbeta".to_string();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let s = buffer_string(&terminal);
        let alpha_line = s.lines().find(|l| l.contains("alpha"));
        let beta_line = s.lines().find(|l| l.contains("beta"));
        assert!(alpha_line.is_some(), "alpha line missing");
        assert!(beta_line.is_some(), "beta line missing");
        assert_ne!(alpha_line, beta_line, "segments must be on distinct lines");
    }

    #[test]
    fn input_toolbar_shows_counts() {
        let dir = tempdir().unwrap();
        let st = Storage::new(dir.path()).unwrap();
        st.ensure_files().unwrap();
        let mut app = App::new(st).unwrap();
        app.input = "ab\ncd".to_string(); // 2 lines, 5 chars (incl. '\n')
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        // Wide-char trailing cells render as spaces in the flattened buffer,
        // so compare against a space-normalized view.
        let s = buffer_string(&terminal).replace(' ', "");
        assert!(s.contains("2行"), "line count missing");
        assert!(s.contains("5字"), "char count missing");
    }

    #[test]
    fn input_placeholder_when_empty() {
        let dir = tempdir().unwrap();
        let st = Storage::new(dir.path()).unwrap();
        st.ensure_files().unwrap();
        let mut app = App::new(st).unwrap();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let s = buffer_string(&terminal).replace(' ', "");
        assert!(s.contains("记录点什么"), "placeholder missing");
    }
}
