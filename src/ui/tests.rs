use std::sync::Arc;
use std::time::{Duration, Instant};

use std::fs;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::tempdir;

use super::*;
use crate::app::{Command, CODE_COPY_FEEDBACK_TTL};

#[test]
fn clearing_a_floating_widget_sanitizes_wide_characters_at_its_edges() {
    let backend = TestBackend::new(12, 5);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new("界"), Rect::new(4, 2, 2, 1));
            frame.render_widget(Paragraph::new("界"), Rect::new(8, 2, 2, 1));

            clear_widget(frame, Rect::new(5, 1, 4, 3));

            let left_outside = &frame.buffer_mut()[(4, 2)];
            assert_eq!(left_outside.symbol(), " ");
            assert_eq!(left_outside.diff_option, CellDiffOption::None);
            assert_eq!(frame.buffer_mut()[(5, 2)].symbol(), " ");

            assert_eq!(frame.buffer_mut()[(8, 2)].symbol(), " ");
            let right_outside = &frame.buffer_mut()[(9, 2)];
            assert_eq!(right_outside.symbol(), " ");
            assert_eq!(right_outside.diff_option, CellDiffOption::None);

            assert_eq!(frame.buffer_mut()[(6, 0)].diff_option, CellDiffOption::None);
            assert_eq!(frame.buffer_mut()[(6, 4)].diff_option, CellDiffOption::None);
        })
        .unwrap();
}

#[test]
fn terminal_snapshot_renders_ansi_styles_and_cursor() {
    let backend = TestBackend::new(12, 5);
    let mut terminal = Terminal::new(backend).unwrap();
    let snapshot = TerminalSnapshot::from_bytes(2, 5, b"\x1b[31;44;1mX");
    let mut cursor = None;
    terminal
        .draw(|frame| {
            draw_terminal_snapshot(
                frame,
                Rect::new(2, 1, 5, 2),
                &snapshot,
                Theme::default(),
                &mut cursor,
            );
        })
        .unwrap();

    let cell = &terminal.backend().buffer()[(2, 1)];
    assert_eq!(cell.symbol(), "X");
    assert_eq!(cell.fg, Color::Indexed(1));
    assert_eq!(cell.bg, Color::Indexed(4));
    assert!(cell.modifier.contains(Modifier::BOLD));
    assert_eq!(cursor, Some(Position::new(3, 1)));
}

#[test]
fn terminal_overlay_uses_the_shared_animated_border() {
    let (mut app, _directory) = make_app();
    app.animation_tick = 7;
    app.overlay = Some(Overlay::Terminal);

    let terminal = render(&mut app, 100, 24);
    let overlay = app.layout.overlay.expect("terminal overlay");
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(overlay.x, overlay.y)].symbol(), "┌");
    assert_eq!(
        buffer[(overlay.x, overlay.y)].fg,
        animated_color(0, app.animation_tick)
    );
    assert_eq!(
        buffer[(overlay.x + 1, overlay.y)].fg,
        animated_color(1, app.animation_tick)
    );
}

#[test]
fn terminal_overlay_advertises_its_close_shortcut_in_the_footer() {
    let (mut app, _directory) = make_app();
    app.overlay = Some(Overlay::Terminal);

    let terminal = render(&mut app, 80, 24);
    let footer = buffer_string(&terminal).lines().last().unwrap().to_string();
    assert!(footer.contains("Ctrl+` close terminal"));
}

#[test]
fn vs16_continuation_cells_are_not_emitted_by_the_buffer_diff() {
    let area = Rect::new(0, 0, 4, 1);
    let previous = Buffer::with_lines(["abcd"]);
    let mut next = Buffer::empty(area);
    next.set_string(0, 0, "☀️xy", Style::default().bg(Color::Blue));

    skip_vs16_continuation_cells(&mut next);

    assert_eq!(next[(1, 0)].diff_option, CellDiffOption::Skip);
    let updates = previous.diff(&next);
    assert!(updates
        .iter()
        .any(|(x, _, cell)| *x == 0 && cell.symbol() == "☀️"));
    assert!(!updates.iter().any(|(x, _, _)| *x == 1));
    assert!(updates
        .iter()
        .any(|(x, _, cell)| *x == 2 && cell.symbol() == "x"));
}

use crate::theme::catppuccin as ctp;

fn animated_activity_lines(text: &str, width: usize, tick: u64) -> Vec<Line<'static>> {
    super::animated_activity_lines(text, width, tick, Theme::default())
}

fn activity_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    super::activity_lines(text, width, Theme::default())
}

fn render_agent_entry(
    entry: &crate::agent_session::AgentPanelEntry,
    width: usize,
    tick: u64,
    animate: bool,
) -> (
    Vec<Line<'static>>,
    Vec<crate::markdown::RenderedLink>,
    Vec<mbtui::ImagePlacement>,
) {
    super::render_agent_entry(entry, width, tick, animate, Theme::default())
}

fn render_daily_note(
    note: &crate::model::DailyNote,
    date_label: String,
    width: usize,
) -> crate::app::DailyCardRenderCache {
    super::render_daily_note(note, date_label, width, Theme::default())
}

fn animated_color(position: usize, tick: u64) -> Color {
    super::animated_color(position, tick, Theme::default())
}
use crate::agent::AskUserKind;
use crate::agent::{
    AgentTerminalSnapshot, AgentTerminalStatus, ApprovalKind, ApprovalRequest, CommandApproval,
    PrivateTerminalInputRequest,
};
use crate::agent_session::AgentPanelEntry;
use crate::app::{Document, DocumentKind, DocumentReturn};
use crate::model::{LinkTarget, TodoItem, WikiLinkCandidate, WikiLinkLocation};
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

fn buffer_string(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let area = buffer.area();
    let mut output = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}

#[test]
fn rendering_uses_colors_loaded_from_the_app_theme() {
    let (mut app, _directory) = make_app();
    let custom = crate::theme::DEFAULT_THEME_TOML
        .replace("canvas = \"terminal\"", "canvas = \"#0a0b0c\"")
        .replace("panel = \"#181825\"", "panel = \"#010203\"")
        .replace("compose = \"#313244\"", "compose = \"#0d0e0f\"")
        .replace("status_bar = \"terminal\"", "status_bar = \"#101112\"")
        .replace(
            "status_context = \"#89b4fa\"",
            "status_context = \"#040506\"",
        )
        .replace("heading_1 = \"#b4befe\"", "heading_1 = \"#070809\"");
    app.theme = Theme::from_toml(&custom).unwrap();

    let terminal = render(&mut app, 220, 24);
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 1)].bg, Color::Rgb(1, 2, 3));
    let center = app.layout.center.unwrap();
    assert_eq!(buffer[(center.x, 0)].bg, Color::Rgb(10, 11, 12));
    let compose = app.layout.compose.unwrap();
    assert_eq!(
        buffer[(compose.x + 1, compose.y + 1)].bg,
        Color::Rgb(13, 14, 15)
    );
    assert_eq!(buffer[(0, 23)].bg, app.theme.ui_shortcut);
    assert_eq!(buffer[(1, 23)].bg, Color::Rgb(4, 5, 6));
    assert_eq!(buffer[(219, 23)].bg, Color::Rgb(16, 17, 18));

    let markdown = crate::markdown::render_at_width("# Heading", 40, app.theme);
    assert!(markdown
        .lines
        .iter()
        .flat_map(|line| &line.spans)
        .any(|span| span.style.fg == Some(Color::Rgb(7, 8, 9))));
}

#[test]
fn agent_header_shows_rounds_usage_stream_speed_and_cache_reads() {
    assert_eq!(human_token_count(999), "999");
    assert_eq!(human_token_count(1_000), "1k");
    assert_eq!(human_token_count(12_400), "12.4k");
    assert_eq!(human_token_count(1_250_000), "1.2m");

    let (mut app, _directory) = make_app();
    app.agent_round = 3;
    app.agent_round_limit = 25;
    app.agent_usage = crate::agent_session::TokenUsage {
        input_tokens: 500,
        output_tokens: 1_234,
        cache_creation_input_tokens: 1_000,
        cache_read_input_tokens: 2_000,
    };
    app.agent_context_window = 6_789;
    app.agent_context_capacity = 200_000;
    app.agent_timed_output_tokens = 1_234;
    app.agent_response_duration = std::time::Duration::from_secs(2);
    app.agent_retry_count = 2;
    let terminal = render(&mut app, 220, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Agent · ↻3/25"));
    assert!(screen.contains("Ctx 6.8k/200k"));
    assert!(screen.contains("↑3.5k↓1.2k"));
    assert!(screen.contains("617.0t/s"));
    assert!(screen.contains("Cache 2k/57.1%"));
    assert!(!screen.contains("Cache R"));
    assert!(!screen.contains("Cache 2k/1k"));
    assert!(screen.contains("R2"));
}

#[test]
fn agent_header_shows_retries_without_confirmed_usage() {
    let (mut app, _directory) = make_app();
    app.agent_retry_count = 2;

    let terminal = render(&mut app, 170, 24);
    assert!(buffer_string(&terminal).contains("Retry 2"));
}

#[test]
fn command_palette_is_fixed_width_and_renders_query_and_commands() {
    let (mut app, _directory) = make_app();
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    app.handle_paste("agent");

    let terminal = render(&mut app, 120, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Command Palette · Ctrl+P"));
    assert!(screen.contains("/ agent"));
    assert!(screen.contains("Agent: Interrupt task"));
    assert!(screen.contains("Stop the active Agent task"));
    assert!(screen.contains("Agent: Clear session"));
    let palette = app.layout.overlay.unwrap();
    assert_eq!(palette.width, 80);
    assert_eq!(
        terminal.backend().buffer()[(palette.x, palette.y)].fg,
        ctp::GREEN
    );
    assert!(app.dialog_hitboxes.len() >= 4);
    let selected = &app.dialog_hitboxes[0].area;
    assert_eq!(selected.height, 3);
    assert_eq!(
        terminal.backend().buffer()[(selected.x, selected.y - 1)].symbol(),
        "▌",
        "the first command needs an upper shared blank row"
    );
    assert_eq!(
        terminal.backend().buffer()[(selected.x, selected.y - 1)].bg,
        ctp::SURFACE_1
    );
    assert_eq!(
        terminal.backend().buffer()[(selected.x, selected.y)].symbol(),
        "▌"
    );
    assert_eq!(
        terminal.backend().buffer()[(selected.x, selected.y + 1)].symbol(),
        "▌"
    );
    assert_eq!(
        terminal.backend().buffer()[(selected.x, selected.y + 2)].symbol(),
        "▌",
        "selection rail should fill the shared blank row"
    );
    assert_eq!(
        terminal.backend().buffer()[(selected.x, selected.y + 2)].bg,
        ctp::SURFACE_1,
        "selection should include the shared blank row"
    );
    assert!(terminal.backend().buffer()[(selected.x + 2, selected.y)]
        .modifier
        .contains(Modifier::BOLD));
    assert_eq!(
        terminal.backend().buffer()[(selected.x + 2, selected.y + 1)].fg,
        app.theme.selection_foreground
    );
    let last = &app.dialog_hitboxes.last().unwrap().area;
    let gap_y = last.y + last.height;
    assert_eq!(
        gap_y,
        palette.y + palette.height - 3,
        "one blank row should separate commands from the footer"
    );
}

#[test]
fn export_format_picker_reserves_and_selects_the_shared_blank_row() {
    let (mut app, _directory) = make_app();
    let options = crate::export::ExportFormat::ALL
        .into_iter()
        .map(|format| crate::app::DialogOption::with_hint(format.label(), format.hint()))
        .collect();
    app.open_dialog(DialogState::new(
        "Export file · Select format",
        String::new(),
        DialogMode::SingleSelect,
        DialogPurpose::ExportFormat,
        options,
    ));

    let terminal = render(&mut app, 100, 20);
    assert_eq!(app.dialog_hitboxes.len(), 2);
    let first = app.dialog_hitboxes[0].area;
    let second = app.dialog_hitboxes[1].area;
    assert_eq!(first.height, SELECT_OPTION_HEIGHT);
    assert_eq!(second.y, first.y + SELECT_OPTION_HEIGHT);
    let buffer = terminal.backend().buffer();
    assert_eq!(
        buffer[(first.x + 2, first.y - 1)].symbol(),
        " ",
        "the export picker needs a blank row above its first format"
    );
    for y in first.y - 1..first.y + first.height {
        assert_eq!(buffer[(first.x, y)].symbol(), "▌");
        assert_eq!(buffer[(first.x, y)].fg, app.theme.selection_indicator);
        assert_eq!(
            buffer[(first.x + first.width - 1, y)].bg,
            app.theme.selection_background,
            "the export selection must cover the complete shared row"
        );
    }
}

#[test]
fn export_destination_uses_spaced_shared_single_line_input() {
    let (mut app, _directory) = make_app();
    let mut dialog = DialogState::new(
        "Export file · Enter destination",
        "Destination path  ",
        DialogMode::SingleLine,
        DialogPurpose::ExportDestination,
        Vec::new(),
    );
    dialog.input = "out.pdf".to_string();
    dialog.cursor = 3;
    app.open_dialog(dialog);

    let terminal = render(&mut app, 100, 12);
    assert!(buffer_string(&terminal).contains("Destination path  out.pdf"));
}

#[test]
fn export_overwrite_confirmation_uses_warning_border() {
    let (mut app, _directory) = make_app();
    app.open_dialog(DialogState::new(
        "Export file · Overwrite destination",
        "/tmp/out.html already exists. Replace it?",
        DialogMode::Confirm,
        DialogPurpose::ExportOverwrite,
        Vec::new(),
    ));

    let terminal = render(&mut app, 100, 12);
    let overlay = app.layout.overlay.expect("overwrite confirmation overlay");
    let corner = &terminal.backend().buffer()[(overlay.x, overlay.y)];
    assert_eq!(corner.fg, app.theme.ui_warning);
    assert_ne!(corner.fg, app.theme.ui_error);
}

#[test]
fn command_palette_keeps_its_query_field_position_when_filtering() {
    let (mut app, _directory) = make_app();
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));

    let _terminal = render(&mut app, 120, 40);
    let initial = app.layout.overlay.unwrap();

    app.handle_paste("theme");
    let _terminal = render(&mut app, 120, 40);
    let filtered = app.layout.overlay.unwrap();

    assert!(filtered.height < initial.height);
    assert_eq!(filtered.y, initial.y);
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x.saturating_add(inner.width) <= outer.x.saturating_add(outer.width)
        && inner.y.saturating_add(inner.height) <= outer.y.saturating_add(outer.height)
}

#[test]
fn narrow_center_surface_fills_body_while_content_axis_is_capped() {
    for width in [60, 80, 120, 169] {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        let terminal = render(&mut app, width, 24);
        let center = app.layout.center.expect("center surface");
        assert_eq!(center, Rect::new(0, 0, width, 23), "width {width}");
        let content = center_content_axis(center);
        assert_eq!(content.width, width.min(CENTER_MAX_WIDTH), "width {width}");
        assert_eq!(
            content.x,
            width.saturating_sub(content.width) / 2,
            "width {width}"
        );
        assert!(app.layout.files.is_none());
        assert!(app.layout.views.is_none());
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), " ");
        assert!(buffer_string(&terminal).contains("Daily"));
    }
}

#[test]
fn wide_layout_uses_terminal_edges_and_center_content_axis() {
    for width in [170, 171, 220] {
        let (mut app, _directory) = make_app();
        app.focus = Focus::Center;
        render(&mut app, width, 24);
        let files = app.layout.files.unwrap();
        let center = app.layout.center.unwrap();
        let views = app.layout.views.unwrap();
        let agent = app.layout.agent.unwrap();
        assert_eq!(files, Rect::new(0, 0, FILES_WIDTH, 23), "width {width}");
        assert_eq!(views.width, RIGHT_SIDEBAR_WIDTH, "width {width}");
        assert_eq!(views.x + views.width, width, "width {width}");
        assert_eq!(views.height, workspace_views_height(23), "width {width}");
        assert_eq!(agent.y, 0, "width {width}");
        assert_eq!(views.y, agent.y + agent.height, "width {width}");
        assert_eq!(agent.height, 23 - views.height, "width {width}");
        let region_width = width - FILES_WIDTH - RIGHT_SIDEBAR_WIDTH;
        assert_eq!(center, Rect::new(FILES_WIDTH, 0, region_width, 23));
        let content = center_content_axis(center);
        assert_eq!(content.width, region_width.min(CENTER_MAX_WIDTH));
        assert_eq!(
            content.x,
            FILES_WIDTH + region_width.saturating_sub(content.width) / 2,
            "width {width}"
        );
    }
}

#[test]
fn workspace_view_sidebar_is_rendered_from_the_registry_and_switches_pages() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Views;
    let terminal = render(&mut app, 170, 24);
    let screen = buffer_string(&terminal);
    let views = app.layout.views.expect("views panel");

    assert_eq!(app.workspace_view_hitboxes.len(), WorkspaceView::ALL.len());
    assert_eq!(
        app.workspace_view_hitboxes
            .iter()
            .map(|hitbox| hitbox.index)
            .collect::<Vec<_>>(),
        (0..WorkspaceView::ALL.len()).collect::<Vec<_>>()
    );
    assert!(app
        .workspace_view_hitboxes
        .windows(2)
        .all(|pair| pair[0].area.y < pair[1].area.y));
    assert!(app
        .workspace_view_hitboxes
        .iter()
        .all(|hitbox| contains(views, hitbox.area)));
    let daily_index = WorkspaceView::index_of(CenterView::Daily).unwrap();
    let daily = app
        .workspace_view_hitboxes
        .iter()
        .find(|hitbox| hitbox.index == daily_index)
        .unwrap()
        .area;
    let buffer = terminal.backend().buffer();
    for y in daily.y.saturating_sub(1)..daily.y + daily.height {
        assert_eq!(buffer[(daily.x, y)].symbol(), "▌");
        assert_eq!(buffer[(daily.x, y)].bg, app.theme.selection_background);
    }
    assert!(screen.contains("Daily notes"));
    assert!(screen.contains("Tasks"));
    assert!(screen.contains("Browse tags"));
    assert!(screen.contains("Find notes"));

    let todo_index = WorkspaceView::index_of(CenterView::Todo).unwrap();
    let todo = app
        .workspace_view_hitboxes
        .iter()
        .find(|hitbox| hitbox.index == todo_index)
        .unwrap()
        .area;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: todo.x,
        row: todo.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.center_view, CenterView::Todo);
    assert_eq!(app.focus, Focus::Center);

    app.focus = Focus::Views;
    app.workspace_view_index = WorkspaceView::index_of(CenterView::Chat).unwrap();
    let terminal = render(&mut app, 170, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Agent"));
    assert!(screen.contains("AI conversation"));
}

#[test]
fn center_view_interface_assigns_the_visible_sidebar_cursor() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Project.md");
    fs::write(&path, "project").unwrap();
    app.reload_files();
    app.focus = Focus::Center;

    let daily = render(&mut app, 170, 24);
    let daily_file = app
        .file_hitboxes
        .iter()
        .find(|hitbox| hitbox.path == path)
        .expect("project file")
        .area;
    let daily_index = WorkspaceView::index_of(CenterView::Daily).unwrap();
    let daily_view = app
        .workspace_view_hitboxes
        .iter()
        .find(|hitbox| hitbox.index == daily_index)
        .unwrap()
        .area;
    assert_ne!(
        daily.backend().buffer()[(daily_file.x, daily_file.y)].symbol(),
        "▌"
    );
    assert_eq!(
        daily.backend().buffer()[(daily_view.x, daily_view.y)].symbol(),
        "▌"
    );

    app.open_files();
    app.file_index = app
        .note_files
        .iter()
        .position(|file| file.path == path)
        .unwrap();
    app.selected_file = Some(path.clone());
    app.file_row = app
        .visible_file_rows()
        .iter()
        .position(|row| matches!(row, FileListRow::File(index) if *index == app.file_index))
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let document = render(&mut app, 170, 24);
    let document_file = app
        .file_hitboxes
        .iter()
        .find(|hitbox| hitbox.path == path)
        .expect("project file")
        .area;
    let document_view = app.workspace_view_hitboxes.first().unwrap().area;
    assert_eq!(
        document.backend().buffer()[(document_file.x, document_file.y)].symbol(),
        "▌"
    );
    assert_ne!(
        document.backend().buffer()[(document_view.x, document_view.y)].symbol(),
        "▌"
    );
}

#[test]
fn footer_uses_full_terminal_width() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.status = "saved-at-left".to_string();
    let terminal = render(&mut app, 220, 12);
    let buffer = terminal.backend().buffer();
    let footer: String = (0..220)
        .map(|x| buffer[(x, 11)].symbol().to_string())
        .collect();
    assert!(footer.starts_with("  DAILY "));
    assert_eq!(buffer[(0, 11)].bg, app.theme.ui_shortcut);
    assert!(footer.contains("saved-at-left"));
    assert!(footer.trim_end().ends_with("? help"));

    app.mouse_captured = false;
    let terminal = render(&mut app, 220, 12);
    assert_eq!(
        terminal.backend().buffer()[(0, 11)].bg,
        app.theme.ui_warning
    );
}

#[test]
fn footer_animates_background_export_status() {
    let (mut app, _directory) = make_app();
    app.export_in_progress = true;
    app.status = "Exporting as PDF to /tmp/out.pdf".to_string();
    app.animation_tick = 0;
    let first = render(&mut app, 170, 12);
    let first_footer = buffer_string(&first).lines().last().unwrap().to_string();
    assert!(first_footer.contains(&format!(
        "{} Exporting as PDF",
        crate::ui::agent::spinner_frame(0)
    )));

    app.animation_tick = 1;
    let second = render(&mut app, 170, 12);
    let second_footer = buffer_string(&second).lines().last().unwrap().to_string();
    assert!(second_footer.contains(&format!(
        "{} Exporting as PDF",
        crate::ui::agent::spinner_frame(1)
    )));
    assert_ne!(first_footer, second_footer);
}

#[test]
fn archived_document_footer_offers_restore_instead_of_archive() {
    let (mut app, _directory) = make_app();
    let path = app.storage.archives_dir.join("Archived.md");
    fs::write(&path, "archived").unwrap();
    app.reload_files();
    app.center_view = CenterView::Document;
    app.focus = Focus::Center;
    app.document = Some(Document {
        kind: DocumentKind::File(path),
        title: "Archived".to_string(),
        source: "archived".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });

    assert!(footer_hint(&app, 120).contains("u restore"));
    assert!(!footer_hint(&app, 120).contains("a archive"));
    assert!(footer_hint(&app, 54).contains("u restore"));
    assert!(!footer_hint(&app, 54).contains("a archive"));
}

#[test]
fn running_agent_animates_its_border_and_current_activity_only() {
    let (mut app, _directory) = make_app();
    app.ai_running = true;
    app.agent_panel = vec![
        Arc::new(AgentPanelEntry::Prompt {
            text: "Analyze this".to_string(),
            muted: false,
        }),
        Arc::new(AgentPanelEntry::Tool {
            text: "Completed Read File.".to_string(),
            active: false,
            preview: None,
        }),
        Arc::new(AgentPanelEntry::Tool {
            text: "Fetching Web...".to_string(),
            active: true,
            preview: None,
        }),
        Arc::new(AgentPanelEntry::Assistant {
            text: "I will compare **multiple sources**.".to_string(),
            streaming: false,
            final_output: false,
        }),
    ];
    app.status = "AI is working".to_string();
    app.animation_tick = 0;
    let first = render(&mut app, 170, 40);
    let agent = app.layout.agent.unwrap();
    let first_corner = first.backend().buffer()[(agent.x, agent.y)].fg;
    let top_colors = (agent.x..agent.x + agent.width)
        .map(|x| first.backend().buffer()[(x, agent.y)].fg)
        .collect::<Vec<_>>();
    assert!(top_colors
        .iter()
        .all(|color| matches!(color, Color::Rgb(..))));
    assert!(top_colors.windows(2).any(|colors| colors[0] != colors[1]));
    let first_footer = buffer_string(&first).lines().last().unwrap().to_string();
    let first_footer_colors = (0..170)
        .map(|x| first.backend().buffer()[(x, 39)].fg)
        .collect::<Vec<_>>();
    let first_activity_colors = (agent.y + 1..agent.y + agent.height - 1)
        .flat_map(|y| (agent.x + 1..agent.x + agent.width - 1).map(move |x| (x, y)))
        .map(|(x, y)| first.backend().buffer()[(x, y)].fg)
        .collect::<Vec<_>>();
    let first_screen = buffer_string(&first);

    let completed = first_screen.find("• Completed Read File.").unwrap();
    let active = first_screen.find("⣷ Fetching Web...").unwrap();
    let intermediate = first_screen
        .find("I will compare multiple sources.")
        .unwrap();
    assert!(completed < active && active < intermediate);

    app.animation_tick = 1;
    let second = render(&mut app, 170, 40);
    let second_corner = second.backend().buffer()[(agent.x, agent.y)].fg;
    let second_footer = buffer_string(&second).lines().last().unwrap().to_string();
    let second_footer_colors = (0..170)
        .map(|x| second.backend().buffer()[(x, 39)].fg)
        .collect::<Vec<_>>();
    let second_activity_colors = (agent.y + 1..agent.y + agent.height - 1)
        .flat_map(|y| (agent.x + 1..agent.x + agent.width - 1).map(move |x| (x, y)))
        .map(|(x, y)| second.backend().buffer()[(x, y)].fg)
        .collect::<Vec<_>>();
    assert_ne!(first_corner, second_corner);
    assert_eq!(first_footer, second_footer);
    assert_eq!(first_footer_colors, second_footer_colors);
    assert_ne!(first_activity_colors, second_activity_colors);

    app.ai_running = false;
    app.agent_panel.push(Arc::new(AgentPanelEntry::Assistant {
        text: "Final response".to_string(),
        streaming: false,
        final_output: true,
    }));
    app.agent_scroll = u16::MAX;
    let final_frame = render(&mut app, 170, 40);
    let final_screen = buffer_string(&final_frame);
    assert!(!final_screen.contains("Response"));
    assert!(final_screen.contains("Final response"));
    for retained in [
        "Completed Read File.",
        "Fetching Web...",
        "multiple sources",
    ] {
        assert!(final_screen.contains(retained));
    }
}

#[test]
fn animated_activity_respects_terminal_cell_width() {
    let lines = animated_activity_lines("正在调用工具", 19, 4);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].to_string(), " ⢿ 正在调用工具");
    assert_eq!(lines[0].width(), 15);
    assert_eq!(
        lines[0]
            .spans
            .iter()
            .skip(1)
            .filter(|span| span.style.fg != Some(ctp::OVERLAY_0))
            .count(),
        6
    );
    assert!(lines[0]
        .spans
        .iter()
        .filter(|span| span.style.fg != Some(ctp::OVERLAY_0))
        .all(|span| span.style.add_modifier.contains(Modifier::BOLD)));
}

#[test]
fn tool_activity_places_detail_on_a_connected_second_line() {
    let text = "Calling Read File...\ndata/a very long project filename.md";
    let static_lines = activity_lines(text, 24);
    let animated_lines = animated_activity_lines(text, 24, 4);
    for lines in [&static_lines, &animated_lines] {
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].width(), 24);
        assert!(lines[1].to_string().starts_with("   └─ "));
        assert!(lines[1].to_string().ends_with('…'));
    }
    assert_eq!(static_lines[0].to_string(), " • Calling Read File...");
    assert_eq!(animated_lines[0].to_string(), " ⢿ Calling Read File...");

    assert_eq!(activity_lines("tool", 1)[0].width(), 1);
    assert_eq!(activity_lines("tool", 2)[0].width(), 2);
    assert_eq!(activity_lines("tool\ndetail", 1)[1].to_string(), "└");
    assert_eq!(activity_lines("tool\ndetail", 2)[1].to_string(), "└─");
}

#[test]
fn yolo_mode_animates_in_the_footer_and_daily_advertises_commands() {
    let (mut app, _directory) = make_app();
    let approve = render(&mut app, 170, 24);
    let approve_screen = buffer_string(&approve);
    let approve_footer = approve_screen.lines().last().unwrap();
    assert!(!approve_footer.contains("DAILY · APPROVE"));
    assert!(approve_footer.contains("DAILY  APPROVE"));
    let surface_x = approve_footer.find("DAILY").unwrap() as u16;
    let approve_byte = approve_footer.find("APPROVE").unwrap();
    let approve_x = approve_footer[..approve_byte].width() as u16;
    assert_eq!(approve.backend().buffer()[(surface_x, 23)].bg, ctp::BLUE);
    assert_eq!(
        approve.backend().buffer()[(approve_x, 23)].bg,
        ctp::SAPPHIRE
    );

    app.permission_mode = PermissionMode::Yolo;
    app.animation_tick = 0;
    let first = render(&mut app, 170, 24);
    let footer_y = 23;
    let first_screen = buffer_string(&first);
    let first_footer = first_screen.lines().last().unwrap();
    let yolo_byte = first_footer.find("YOLO").unwrap();
    let yolo_x = first_footer[..yolo_byte].width() as u16;
    let first_colors = (yolo_x..yolo_x + "YOLO".width() as u16)
        .map(|x| first.backend().buffer()[(x, footer_y)].fg)
        .collect::<Vec<_>>();
    assert!(first_footer.contains("Ctrl+P commands"));
    assert!(first_colors
        .iter()
        .all(|color| matches!(color, Color::Rgb(..))));
    assert!((yolo_x..yolo_x + "YOLO".width() as u16)
        .all(|x| first.backend().buffer()[(x, footer_y)].bg == ctp::CRUST));

    app.animation_tick = 1;
    let second = render(&mut app, 170, 24);
    let second_colors = (yolo_x..yolo_x + "YOLO".width() as u16)
        .map(|x| second.backend().buffer()[(x, footer_y)].fg)
        .collect::<Vec<_>>();
    assert_ne!(first_colors, second_colors);
}

#[test]
fn virtual_line_window_clones_only_visible_rows() {
    let lines = (0..10_000)
        .map(|index| Line::from(format!("row {index}")))
        .collect::<Vec<_>>();
    let visible = visible_line_window(&lines, 9_990, 5);

    assert_eq!(visible.len(), 5);
    assert_eq!(visible.first().unwrap().to_string(), "row 9990");
    assert_eq!(visible.last().unwrap().to_string(), "row 9994");
    assert!(visible_line_window(&lines, lines.len(), 5).is_empty());
}

#[test]
fn narrow_files_and_todo_page_each_use_the_full_body_without_duplicates() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
    app.reload_files();
    app.focus = Focus::Files;
    let terminal = render(&mut app, 80, 18);
    assert_eq!(app.layout.files, Some(Rect::new(0, 0, 80, 17)));
    assert!(app.layout.center.is_none());
    assert!(app.layout.views.is_none());
    assert_eq!(buffer_string(&terminal).matches("NólëBase").count(), 1);
    assert!(!app.file_hitboxes.is_empty());
    assert!(app
        .file_hitboxes
        .iter()
        .all(|hitbox| contains(app.layout.files.unwrap(), hitbox.area)));

    app.focus = Focus::Center;
    app.center_view = CenterView::Todo;
    app.todo_items = vec![TodoItem {
        checked: false,
        text: "buy milk".to_string(),
    }];
    let terminal = render(&mut app, 60, 18);
    assert_eq!(app.layout.center, Some(Rect::new(0, 0, 60, 17)));
    assert!(app.layout.files.is_none());
    assert!(app.layout.views.is_none());
    let screen = buffer_string(&terminal);
    assert_eq!(screen.matches("Todo").count(), 1);
    assert!(screen.contains("buy milk"));
    assert_eq!(app.todo_hitboxes.len(), 1);
}

#[test]
fn sidebars_use_mantle_background_with_square_ui_borders() {
    let (mut app, _directory) = make_app();
    let terminal = render(&mut app, 170, 24);
    let buffer = terminal.backend().buffer();
    let files = app.layout.files.expect("files panel");
    let views = app.layout.views.expect("views panel");
    let agent = app.layout.agent.expect("agent panel");
    let center = app.layout.center.expect("center region");

    for area in [files, views, agent] {
        assert_eq!(buffer[(area.x, area.y)].symbol(), "┌");
        assert_eq!(buffer[(area.x + 2, area.y + 1)].bg, ctp::MANTLE);
    }
    assert_eq!(buffer[(center.x, center.y)].bg, Color::Reset);
}

#[test]
fn selected_file_background_covers_name_and_modified_time_rows() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
    app.reload_files();
    app.focus = Focus::Files;
    let modified: DateTime<Local> = app.note_files[app.file_index].modified.into();
    let expected_timestamp = modified.format("%y/%m/%d %H:%M").to_string();

    let terminal = render(&mut app, 170, 18);
    assert!(buffer_string(&terminal).contains(&expected_timestamp));
    let selected_path = app.note_files[app.file_index].path.clone();
    let selected_area = app
        .file_hitboxes
        .iter()
        .find(|hitbox| hitbox.path == selected_path)
        .expect("selected file hitbox")
        .area;
    assert_eq!(selected_area.height, 2);
    let buffer = terminal.backend().buffer();
    for y in selected_area.y..selected_area.y + selected_area.height {
        for x in selected_area.x..selected_area.x + selected_area.width {
            assert_eq!(buffer[(x, y)].bg, ctp::SURFACE_1);
        }
    }
    assert_eq!(
        buffer[(selected_area.x + 1, selected_area.y + 1)].fg,
        app.theme.selection_foreground,
        "modified time must remain legible on the selected background"
    );
}

#[test]
fn archived_file_metadata_does_not_repeat_its_group() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.archives_dir.join("Old.md"), "old").unwrap();
    app.reload_files();
    app.archives_expanded = true;

    let terminal = render(&mut app, 170, 18);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Old"));
    assert!(!screen.contains("Archived ·"));
}

#[test]
fn file_selection_includes_both_shared_spacing_lines() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("First.md"), "first").unwrap();
    fs::write(app.storage.data_dir.join("Second.md"), "second").unwrap();
    app.reload_files();
    app.focus = Focus::Files;

    let terminal = render(&mut app, 170, 22);
    let mut files = app.file_hitboxes.clone();
    files.sort_by_key(|hitbox| hitbox.area.y);
    let notes = app
        .file_group_hitboxes
        .iter()
        .find(|hitbox| hitbox.group == FileGroup::Notes)
        .expect("Notes group")
        .area;
    let archives = app
        .file_group_hitboxes
        .iter()
        .find(|hitbox| hitbox.group == FileGroup::Archives)
        .expect("Archives group")
        .area;

    assert_eq!(files.len(), 2);
    assert_eq!(files[0].area.y, notes.y + 2);
    assert_eq!(files[1].area.y, files[0].area.y + 3);
    assert_eq!(archives.y, files[1].area.y + 3);
    assert_eq!(files[0].area.height, 2);
    assert_eq!(files[1].area.height, 2);
    let buffer = terminal.backend().buffer();
    let selected = files
        .iter()
        .find(|hitbox| Some(&hitbox.path) == app.selected_file.as_ref())
        .expect("selected file");
    assert_eq!(
        buffer[(selected.area.x, selected.area.y.saturating_sub(1))].bg,
        ctp::SURFACE_1,
        "selected background must include the upper shared spacing"
    );
    assert_eq!(
        buffer[(selected.area.x, selected.area.y + 2)].bg,
        ctp::SURFACE_1,
        "selected background must include the lower shared spacing"
    );
    for rail_y in selected.area.y.saturating_sub(1)..=selected.area.y + 2 {
        let cell = &buffer[(selected.area.x, rail_y)];
        assert_eq!(cell.symbol(), "▌");
        assert_eq!(cell.fg, ctp::MAUVE);
        assert!(!cell.modifier.contains(Modifier::BOLD));
        assert!(!cell.modifier.contains(Modifier::DIM));
    }
    assert_ne!(buffer[(notes.x, notes.y)].symbol(), "▌");
    assert_ne!(buffer[(archives.x, archives.y)].symbol(), "▌");
}

#[test]
fn file_groups_use_teal_markers_and_muted_counts() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
    app.reload_files();
    app.focus = Focus::Files;

    let terminal = render(&mut app, 170, 18);
    let notes = app
        .file_group_hitboxes
        .iter()
        .find(|hitbox| hitbox.group == FileGroup::Notes)
        .expect("Notes group hitbox")
        .area;
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(notes.x, notes.y)].fg, ctp::TEAL);
    assert_eq!(
        buffer[(notes.x + notes.width - 1, notes.y)].fg,
        ctp::OVERLAY_0
    );
}

#[test]
fn file_name_inputs_render_as_modals_while_search_stays_inline() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.files_context = FilesContext::NewTarget;
    app.new_file_input = "Project".to_string();
    app.new_file_cursor = app.new_file_input.chars().count();
    let terminal = render(&mut app, 80, 16);
    assert_eq!(app.layout.files, Some(Rect::new(0, 0, 80, 15)));
    assert!(app.layout.center.is_none());
    let screen = buffer_string(&terminal);
    assert!(screen.contains("New file · Enter create"));
    assert!(screen.contains("Name  Project"));
    assert!(app.layout.overlay.is_some());
    assert!(app.file_hitboxes.is_empty());
    let modal = app.layout.overlay.unwrap();
    assert_eq!(
        terminal.backend().buffer()[(modal.x + 1, modal.y + 1)].bg,
        ctp::MANTLE,
        "modal padding should have an opaque background"
    );

    app.files_context = FilesContext::Rename;
    app.rename_input = "Renamed".to_string();
    app.rename_cursor = app.rename_input.chars().count();
    let terminal = render(&mut app, 80, 16);
    assert!(buffer_string(&terminal).contains("Name  Renamed"));
    assert!(app.layout.overlay.is_some());

    app.files_context = FilesContext::Search;
    app.file_query = "work".to_string();
    let terminal = render(&mut app, 80, 16);
    assert!(buffer_string(&terminal).contains("/ work"));
    let files = app.layout.files.unwrap();
    let underline = &terminal.backend().buffer()[(files.x + 2, files.y + 2)];
    assert_eq!(underline.symbol(), "─");
    assert_eq!(underline.fg, ctp::OVERLAY_0);
    assert!(app.layout.overlay.is_none());

    app.open_dialog(DialogState::new(
        "Rename #old",
        "New tag  #",
        DialogMode::SingleLine,
        DialogPurpose::TagRenameTarget,
        Vec::new(),
    ));
    app.dialog.as_mut().unwrap().input = "new/tag".to_string();
    app.dialog.as_mut().unwrap().cursor = 7;
    let terminal = render(&mut app, 80, 16);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Rename #old"));
    assert!(screen.contains("New tag  #new/tag"));
    assert!(!screen.contains("Optional prompt"));
    assert_eq!(app.layout.overlay.unwrap().height, 5);
}

#[test]
fn narrow_center_renders_each_center_view_in_place() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.center_view = CenterView::Document;
    app.document = Some(Document {
        kind: DocumentKind::Daily(NaiveDate::from_ymd_opt(2026, 7, 27).unwrap()),
        title: "Preview".to_string(),
        source: "# Heading".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    let terminal = render(&mut app, 80, 18);
    assert!(buffer_string(&terminal).contains("Heading"));

    app.center_view = CenterView::Search;
    app.search_query = "needle".to_string();
    app.search_results = vec![SearchHit::FileLine {
        path: PathBuf::from("2026-07-27.md"),
        line_no: 1,
        text: format!("needle result {}", "x".repeat(100)),
    }];
    let terminal = render(&mut app, 80, 18);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Searcher · 1"));
    assert!(screen.contains("2026-07-27:1"));
    assert!(screen.contains("needle result"));
    assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), " ");
    assert_ne!(
        terminal.backend().buffer()[(4, 0)].symbol(),
        " ",
        "only the centered searcher should have a border"
    );
    assert_eq!(app.search_hitboxes.len(), 1);
    let result = app.search_hitboxes[0].area;
    assert_eq!(result.height, SELECT_OPTION_HEIGHT);
    for y in result.y - 1..result.y + result.height {
        assert_eq!(
            terminal.backend().buffer()[(result.x + result.width - 1, y)].bg,
            app.theme.selection_background,
            "search selections should include both full-width shared blank rows"
        );
        assert_eq!(terminal.backend().buffer()[(result.x, y)].symbol(), "▌");
        assert_eq!(
            terminal.backend().buffer()[(result.x, y)].fg,
            app.theme.selection_indicator
        );
    }
    assert_eq!(
        terminal.backend().buffer()[(result.x + result.width - 2, result.y)].symbol(),
        " "
    );

    app.center_view = CenterView::DocumentSearch;
    let terminal = render(&mut app, 80, 18);
    assert!(buffer_string(&terminal).contains("Search in Note · 1"));
    assert_eq!(
        terminal.backend().buffer()[(result.x + result.width - 1, result.y)].symbol(),
        " "
    );

    app.center_view = CenterView::Tags;
    app.tag_results = vec![
        crate::workspace_index::TagSummary {
            name: "rust".to_string(),
            documents: 2,
            mentions: 3,
        },
        crate::workspace_index::TagSummary {
            name: "design/system".to_string(),
            documents: 1,
            mentions: 4,
        },
    ];
    let terminal = render(&mut app, 80, 18);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Tags · 2"));
    assert!(screen.contains("#rust"));
    assert!(screen.contains("2 documents · 3 mentions"));
    assert_eq!(app.tag_hitboxes.len(), 2);
    assert_eq!(app.tag_hitboxes[0].area.height, SELECT_OPTION_HEIGHT);
}

#[test]
fn active_tag_renders_full_notes_as_chronological_cards() {
    let (mut app, directory) = make_app();
    let older = directory.path().join("data/Older.md");
    let newer = directory.path().join("archives/Newer.md");
    app.center_view = CenterView::Tags;
    app.active_tag = Some("rust".to_string());
    app.tag_notes = vec![
        crate::model::TagNote {
            path: older,
            title: "Older".to_string(),
            body: "# Older body\n\nComplete first note with #nested.".to_string(),
            modified: std::time::UNIX_EPOCH + std::time::Duration::from_secs(1),
        },
        crate::model::TagNote {
            path: newer,
            title: "Newer".to_string(),
            body: "# Newer body\n\nComplete second note.".to_string(),
            modified: std::time::UNIX_EPOCH + std::time::Duration::from_secs(2),
        },
    ];

    let terminal = render(&mut app, 120, 40);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("#rust · 2 notes"));
    assert!(screen.contains("Complete first note"));
    assert!(screen.contains("Complete second note"));
    assert!(screen.find("Older").unwrap() < screen.find("Newer").unwrap());
    assert!(!screen.contains("Older.md"));
    assert_eq!(app.tag_note_hitboxes.len(), 2);
    assert!(app
        .tag_hitboxes
        .iter()
        .any(|hitbox| hitbox.name == "nested"));
    assert!(app
        .tag_note_vlist
        .items
        .iter()
        .all(|item| item.cache.is_some()));
}

#[test]
fn tag_card_matches_daily_card_except_for_buttons() {
    let title = "2026-07-01";
    let body = "# Heading\n\nThe [same body](https://example.com) and spacing.";
    let daily = crate::model::DailyNote {
        date: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
        body: body.to_string(),
    };
    let tag = crate::model::TagNote {
        path: PathBuf::from("data/Note.md"),
        title: title.to_string(),
        body: body.to_string(),
        modified: std::time::UNIX_EPOCH,
    };
    let daily_card = render_daily_note(&daily, title.to_string(), 80);
    let tag_card = render_tag_note_card(&tag, 80, Theme::default());
    assert_eq!(tag_card.lines.len(), daily_card.lines.len());
    for (index, (tag_line, daily_line)) in tag_card.lines.iter().zip(&daily_card.lines).enumerate()
    {
        if index != daily_card.button_line {
            assert_eq!(tag_line, daily_line, "layout differs on row {index}");
        }
    }
    assert!(tag_card.lines[daily_card.button_line]
        .spans
        .iter()
        .all(|span| span.content.trim().is_empty()));
    let mut long_title = tag.clone();
    long_title.title = "A very long note title that must not move the body".to_string();
    let long_card = render_tag_note_card(&long_title, 80, Theme::default());
    assert_eq!(
        tag_card.links[0].column, long_card.links[0].column,
        "note title width must not change card margins"
    );
}
#[test]
fn tag_note_vlist_only_materializes_visible_cards() {
    let (mut app, directory) = make_app();
    app.active_tag = Some("many".to_string());
    app.tag_notes = (0..40)
        .map(|index| crate::model::TagNote {
            path: directory.path().join(format!("data/Note-{index}.md")),
            title: format!("Note-{index}"),
            body: format!("# Note {index}\n\nFull body for #many."),
            modified: std::time::UNIX_EPOCH + std::time::Duration::from_secs(index as u64),
        })
        .collect();

    sync_tag_note_vlist(&mut app, 72);
    assert!(app
        .tag_note_vlist
        .items
        .iter()
        .all(|item| item.cache.is_none()));
    let scroll = measure_visible_tag_note_cards(&mut app, 0, 8);
    assert_eq!(scroll, 0);
    let rendered = app
        .tag_note_vlist
        .items
        .iter()
        .filter(|item| item.cache.is_some())
        .count();
    assert!(rendered > 0 && rendered < app.tag_notes.len());
    assert!(app.tag_note_vlist.items[20].cache.is_none());
}

#[test]
fn attachments_browser_keeps_blank_row_and_full_area_selection_invariants() {
    let (mut app, _directory) = make_app();
    for (name, bytes) in [
        ("report.pdf", b"pdf-bytes".as_slice()),
        ("photo.png", b"png-bytes".as_slice()),
        ("notes.txt", b"text-bytes".as_slice()),
    ] {
        app.attachment_store
            .import_bytes(bytes, Some(name))
            .unwrap();
    }
    let uri = app
        .attachment_store
        .list(&crate::attachment::AttachmentQuery::default())
        .unwrap()
        .items[0]
        .uri()
        .to_string();
    fs::write(
        app.storage.data_dir.join("Note.md"),
        format!("[a]({uri})\n"),
    )
    .unwrap();
    app.apply_attachment_index(
        0,
        crate::attachment_index::AttachmentReferenceIndex::build(&app.storage),
    );
    app.open_attachments();

    let terminal = render(&mut app, 80, 18);
    let buffer = terminal.backend().buffer();
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Attachments · 3"), "{screen}");
    assert!(screen.contains("report.pdf"), "{screen}");
    assert!(screen.contains("photo.png"), "{screen}");
    assert!(screen.contains("1 note"), "{screen}");
    assert_eq!(app.attachment_hitboxes.len(), 3);

    let first = app.attachment_hitboxes[0].area;
    assert_eq!(first.height, SELECT_OPTION_HEIGHT);
    assert!(first.y >= 1, "first item starts below the filter header");
    assert_eq!(
        buffer[(first.x + 2, first.y - 1)].symbol(),
        " ",
        "the row above the first item is a shared blank row"
    );
    for y in first.y - 1..first.y + first.height {
        assert_eq!(
            buffer[(first.x + first.width - 1, y)].bg,
            app.theme.selection_background,
            "selection spans the full list width including the shared blank row"
        );
        assert_eq!(buffer[(first.x, y)].symbol(), "▌");
        assert_eq!(buffer[(first.x, y)].fg, app.theme.selection_indicator);
    }
}

#[test]
fn search_input_renders_the_shared_cursor_position() {
    let (mut app, _directory) = make_app();
    app.open_search();
    app.handle_paste("abc");
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();

    let mut end = None;
    terminal.draw(|frame| end = draw(frame, &mut app)).unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    let mut moved = None;
    terminal
        .draw(|frame| moved = draw(frame, &mut app))
        .unwrap();

    assert_eq!(moved.unwrap().x + 1, end.unwrap().x);
}

#[test]
fn multiline_input_cursor_follows_greedy_mixed_width_wrapping() {
    let mut terminal = Terminal::new(TestBackend::new(8, 3)).unwrap();
    let mut cursor = None;

    terminal
        .draw(|frame| {
            cursor = draw_multiline_input(
                frame,
                Rect::new(1, 0, 6, 3),
                "abc中中",
                5,
                "",
                true,
                Theme::default(),
            );
        })
        .unwrap();

    assert_eq!(cursor, Some(Position::new(3, 1)));
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 1)].symbol(), "中");
    assert_eq!(buffer[(2, 1)].symbol(), " ");
}

#[test]
fn todo_filter_renders_cursor_no_matches_and_filtered_selection_geometry() {
    let (mut app, _directory) = make_app();
    app.center_view = CenterView::Todo;
    app.focus = Focus::Center;
    app.todo_items = vec![
        TodoItem {
            checked: false,
            text: "buy milk".to_string(),
        },
        TodoItem {
            checked: false,
            text: "write docs".to_string(),
        },
    ];
    app.handle_paste("milk");
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();

    let mut end = None;
    terminal.draw(|frame| end = draw(frame, &mut app)).unwrap();
    let screen = buffer_string(&terminal);
    assert!(screen.contains("/ milk"));
    assert!(screen.contains("buy milk"));
    assert!(!screen.contains("write docs"));
    assert_eq!(app.todo_hitboxes.len(), 1);
    assert_eq!(app.todo_hitboxes[0].index, 0);
    let item = app.todo_hitboxes[0].area;
    assert_eq!(item.y, app.layout.center.unwrap().y + 5);
    for y in item.y.saturating_sub(1)..item.y + item.height {
        assert_eq!(terminal.backend().buffer()[(item.x, y)].symbol(), "▌");
    }

    app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    let mut moved = None;
    terminal
        .draw(|frame| moved = draw(frame, &mut app))
        .unwrap();
    assert_eq!(moved.unwrap().x + 1, end.unwrap().x);

    app.todo_query = "missing".to_string();
    app.todo_cursor = app.todo_query.chars().count();
    let terminal = render(&mut app, 80, 18);
    assert!(buffer_string(&terminal).contains("No matches"));
    assert!(app.todo_hitboxes.is_empty());
}

#[test]
fn chat_compose_and_button_hitboxes_stay_inside_visible_center_viewport() {
    for width in [60, 80, 120, 169, 170, 171, 220] {
        let (mut app, _directory) = make_app();
        for index in 0..30 {
            app.storage
                .append_to_today(&format!("message {index}"))
                .unwrap();
        }
        app.reload();
        app.selected = app.daily_notes.len() - 1;
        app.focus = Focus::Center;
        app.scroll = u16::MAX;
        render(&mut app, width, 24);
        let center = app.layout.center.unwrap();
        let compose = app.layout.compose.unwrap();
        assert!(compose.width <= CENTER_MAX_WIDTH, "width {width}");
        assert!(contains(center, compose), "width {width}");
        assert!(!app.hitboxes.is_empty(), "width {width}");
        for hitbox in &app.hitboxes {
            assert!(contains(center, hitbox.area), "width {width}");
            assert!(
                hitbox.area.y < compose.y.saturating_sub(1),
                "button behind compose at width {width}: {:?}",
                hitbox.area
            );
        }
    }
}

#[test]
fn overlay_records_geometry_and_disables_all_background_hitboxes() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
    app.reload_files();
    app.storage.append_to_today("hello").unwrap();
    app.reload();
    app.todo_items = vec![TodoItem {
        checked: false,
        text: "task".to_string(),
    }];
    app.focus = Focus::Center;
    app.set_overlay(Overlay::Help);
    let terminal = render(&mut app, 220, 40);
    assert!(buffer_string(&terminal).contains("Ctrl+Alt+V"));
    assert!(app.layout.overlay.is_some());
    assert!(app.hitboxes.is_empty());
    assert!(app.link_hitboxes.is_empty());
    assert!(app.tag_hitboxes.is_empty());
    assert!(app.file_hitboxes.is_empty());
    assert!(app.todo_hitboxes.is_empty());
    assert!(app.search_hitboxes.is_empty());
}

#[test]
fn links_are_clickable_in_daily_documents_and_agent_output() {
    let (mut app, _directory) = make_app();
    app.storage
        .append_to_today("Open [site](https://example.test)")
        .unwrap();
    app.reload();
    render(&mut app, 170, 24);
    assert!(app.link_hitboxes.iter().any(|hitbox| {
        hitbox.target == LinkTarget::External("https://example.test".to_string())
    }));

    app.document = Some(Document {
        kind: DocumentKind::Daily(NaiveDate::from_ymd_opt(2026, 7, 27).unwrap()),
        title: "Preview".to_string(),
        source: "See [[Project]]".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    app.center_view = CenterView::Document;
    render(&mut app, 170, 24);
    assert!(app
        .link_hitboxes
        .iter()
        .any(|hitbox| { hitbox.target == LinkTarget::WikiLink("Project".to_string()) }));

    app.agent_panel = vec![Arc::new(AgentPanelEntry::Assistant {
        text: "[result](https://agent.example)".to_string(),
        streaming: false,
        final_output: true,
    })];
    render(&mut app, 170, 40);
    assert!(app.link_hitboxes.iter().any(|hitbox| {
        hitbox.target == LinkTarget::External("https://agent.example".to_string())
    }));
}

#[test]
fn local_link_hitboxes_resolve_against_each_content_base() {
    let (mut app, _directory) = make_app();
    app.storage
        .append_to_today("[Daily report](daily-report.pdf) and ![[daily-attachment.pdf]]")
        .unwrap();
    app.reload();
    render(&mut app, 170, 24);
    for filename in ["daily-report.pdf", "daily-attachment.pdf"] {
        assert!(app.link_hitboxes.iter().any(|hitbox| {
            hitbox.target == LinkTarget::LocalFile(app.storage.daily_dir.join(filename))
        }));
    }

    let note = app.storage.data_dir.join("Article.md");
    app.document = Some(Document {
        kind: DocumentKind::File(note),
        title: "Preview".to_string(),
        source: "[Article report](article-report.pdf) and ![[article-attachment.pdf]]".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    app.center_view = CenterView::Document;
    render(&mut app, 170, 24);
    for filename in ["article-report.pdf", "article-attachment.pdf"] {
        assert!(app.link_hitboxes.iter().any(|hitbox| {
            hitbox.target == LinkTarget::LocalFile(app.storage.data_dir.join(filename))
        }));
    }

    app.agent_panel = vec![Arc::new(AgentPanelEntry::Assistant {
        text: "[Agent report](agent-report.pdf) and ![[agent-attachment.pdf]]".to_string(),
        streaming: false,
        final_output: true,
    })];
    render(&mut app, 170, 40);
    for filename in ["agent-report.pdf", "agent-attachment.pdf"] {
        assert!(app.link_hitboxes.iter().any(|hitbox| {
            hitbox.target == LinkTarget::LocalFile(app.storage.root.join(filename))
        }));
    }
}

#[test]
fn hashtags_are_clickable_in_daily_and_document_views() {
    let (mut app, _directory) = make_app();
    app.storage.append_to_today("Daily #rust").unwrap();
    app.reload();
    render(&mut app, 170, 24);
    let daily = app
        .tag_hitboxes
        .iter()
        .find(|hitbox| hitbox.name == "rust")
        .expect("Daily Hashtag hitbox")
        .area;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: daily.x,
        row: daily.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.center_view, CenterView::Tags);
    assert_eq!(app.active_tag.as_deref(), Some("rust"));

    app.document = Some(Document {
        kind: DocumentKind::Daily(NaiveDate::from_ymd_opt(2026, 7, 27).unwrap()),
        title: "Preview".to_string(),
        source: "Document #design".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    app.center_view = CenterView::Document;
    render(&mut app, 170, 24);
    assert!(app
        .tag_hitboxes
        .iter()
        .any(|hitbox| hitbox.name == "design"));
}

#[test]
fn wikilink_choice_marks_location_and_file_format_as_muted_metadata() {
    let (mut app, directory) = make_app();
    app.wiki_link_target = Some("Project".to_string());
    app.wiki_link_candidates = vec![
        WikiLinkCandidate {
            path: directory.path().join("daily/2026-08-02.md"),
            location: WikiLinkLocation::Daily,
        },
        WikiLinkCandidate {
            path: directory.path().join("data/Project.md"),
            location: WikiLinkLocation::Notes,
        },
        WikiLinkCandidate {
            path: directory.path().join("archives/Project.mb"),
            location: WikiLinkLocation::Archives,
        },
    ];
    app.set_overlay(Overlay::WikiLinkChoice);
    let terminal = render(&mut app, 100, 20);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("2026-08-02.md"));
    assert!(screen.contains("Daily"));
    assert!(screen.contains("Project.md"));
    assert!(screen.contains("MD"));
    assert!(screen.contains("Archived"));
    assert!(screen.contains("MB"));
    assert_eq!(app.wiki_link_hitboxes.len(), 3);
}

#[test]
fn document_view_renders_backlink_section_after_body() {
    let (mut app, _directory) = make_app();
    let target = app.storage.data_dir.join("Target.md");
    let source = app.storage.data_dir.join("Source.md");
    fs::write(&target, "body line one\n").unwrap();
    fs::write(&source, "see [[Target]]\n").unwrap();
    app.document = Some(Document {
        kind: DocumentKind::File(target.clone()),
        title: "Target.md".to_string(),
        source: "body line one\n".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    app.center_view = CenterView::Document;
    app.focus = Focus::Center;
    app.apply_wiki_link_index(crate::wiki_link_index::WikiLinkIndex::build(&app.storage));
    assert_eq!(app.document_backlinks, vec![source.clone()]);

    let terminal = render(&mut app, 170, 24);
    let screen = buffer_string(&terminal);
    let body = screen.find("body line one").unwrap();
    let heading = screen.find("Backlinks").unwrap();
    let entry = screen[heading..]
        .find("Source.md")
        .map(|offset| heading + offset)
        .unwrap();
    assert!(body < heading, "backlink heading renders after the body");
    assert!(heading < entry, "backlink entries render after the heading");
    assert!(app
        .backlink_hitboxes
        .iter()
        .any(|hitbox| hitbox.path == source));
    // Layout: body, two blank rows, heading, blank row, indented entry. The
    // entry sits two rows below the heading and starts at column 3 (" • ").
    let body_row = screen[..body].matches('\n').count();
    let heading_row = screen[..heading].matches('\n').count();
    let entry_row = screen[..entry].matches('\n').count();
    assert_eq!(
        heading_row,
        body_row + 3,
        "two blank rows before the heading"
    );
    assert_eq!(entry_row, heading_row + 2, "blank row after the heading");
    let heading_line_start = screen[..heading]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let entry_line_start = screen[..entry].rfind('\n').map_or(0, |newline| newline + 1);
    let heading_column = UnicodeWidthStr::width(&screen[heading_line_start..heading]);
    let entry_column = UnicodeWidthStr::width(&screen[entry_line_start..entry]);
    assert_eq!(
        entry_column.saturating_sub(heading_column),
        3,
        "entry is indented one space under the heading"
    );
    let hitbox = app
        .backlink_hitboxes
        .iter()
        .find(|hitbox| hitbox.path == source)
        .unwrap();
    assert_eq!(usize::from(hitbox.area.y), entry_row);
    assert_eq!(usize::from(hitbox.area.x), entry_column);
}

#[test]
fn backlink_hitbox_width_matches_the_rendered_truncated_name() {
    let (mut app, _directory) = make_app();
    let target = app.storage.data_dir.join("Target.md");
    let source = app
        .storage
        .data_dir
        .join("Source-With-A-Very-Long-Name-That-Forces-Truncation.md");
    fs::write(&target, "body line one\n").unwrap();
    fs::write(&source, "see [[Target]]\n").unwrap();
    app.document = Some(Document {
        kind: DocumentKind::File(target.clone()),
        title: "Target.md".to_string(),
        source: "body line one\n".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    app.center_view = CenterView::Document;
    app.focus = Focus::Center;
    app.apply_wiki_link_index(crate::wiki_link_index::WikiLinkIndex::build(&app.storage));
    assert_eq!(app.document_backlinks, vec![source.clone()]);

    // Narrow enough that the long name is ellipsized; the hitbox must cover
    // exactly the rendered name, not bleed into the page padding.
    let terminal = render(&mut app, 60, 24);
    let screen = buffer_string(&terminal);
    let heading = screen.find("Backlinks").unwrap();
    let entry = screen[heading..]
        .find("Source")
        .map(|offset| heading + offset)
        .unwrap();
    let hitbox = app
        .backlink_hitboxes
        .iter()
        .find(|hitbox| hitbox.path == source)
        .unwrap();
    let entry_line_start = screen[..entry].rfind('\n').map_or(0, |newline| newline + 1);
    let entry_column = UnicodeWidthStr::width(&screen[entry_line_start..entry]);
    // The rendered name ends at the first blank cell after it.
    let name_end = screen[entry..].find(' ').unwrap();
    let name_width = UnicodeWidthStr::width(&screen[entry..entry + name_end]);
    assert!(
        name_width < UnicodeWidthStr::width(source.to_string_lossy().as_ref()),
        "the long name must be truncated at this width"
    );
    assert_eq!(usize::from(hitbox.area.x), entry_column);
    assert_eq!(usize::from(hitbox.area.width), name_width);
    assert!(usize::from(hitbox.area.x) + usize::from(hitbox.area.width) <= 60);
}

#[test]
fn document_view_hides_backlink_section_when_nothing_links_to_the_note() {
    let (mut app, _directory) = make_app();
    let target = app.storage.data_dir.join("Target.md");
    fs::write(&target, "body").unwrap();
    app.document = Some(Document {
        kind: DocumentKind::File(target),
        title: "Target.md".to_string(),
        source: "body".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    app.center_view = CenterView::Document;
    app.focus = Focus::Center;
    app.apply_wiki_link_index(crate::wiki_link_index::WikiLinkIndex::build(&app.storage));
    assert!(app.document_backlinks.is_empty());

    let terminal = render(&mut app, 170, 24);
    assert!(!buffer_string(&terminal).contains("Backlinks"));
    assert!(app.backlink_hitboxes.is_empty());
}

#[test]
fn backlink_section_scrolls_with_the_document_and_stays_clickable() {
    let (mut app, _directory) = make_app();
    let target = app.storage.data_dir.join("Target.md");
    let source = app.storage.data_dir.join("Source.md");
    // A tall body pushes the Backlinks section below the fold.
    let body = (0..60)
        .map(|line| format!("filler line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&target, &body).unwrap();
    fs::write(&source, "see [[Target]]\n").unwrap();
    app.document = Some(Document {
        kind: DocumentKind::File(target),
        title: "Target.md".to_string(),
        source: body,
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    app.center_view = CenterView::Document;
    app.focus = Focus::Center;
    app.apply_wiki_link_index(crate::wiki_link_index::WikiLinkIndex::build(&app.storage));

    // Unscrolled: section is below the fold, no hitbox visible.
    let _terminal = render(&mut app, 170, 24);
    assert!(app
        .backlink_hitboxes
        .iter()
        .all(|hitbox| hitbox.area.y >= 24));

    // Scroll to the end: the section scrolls into view and is clickable.
    app.document.as_mut().unwrap().scroll = 200;
    let _terminal = render(&mut app, 170, 24);
    assert!(app
        .backlink_hitboxes
        .iter()
        .any(|hitbox| hitbox.area.y < 24 && hitbox.path == source));
    let hitbox = app
        .backlink_hitboxes
        .iter()
        .find(|hitbox| hitbox.path == source)
        .unwrap();
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hitbox.area.x.saturating_add(1),
        row: hitbox.area.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(source))
    );
}

#[test]
fn approval_panel_with_empty_diff_keeps_body_and_footer() {
    let (mut app, _directory) = make_app();
    app.approval_request = Some(ApprovalRequest {
        title: "Delete data/Empty.md".to_string(),
        message: String::new(),
        kind: ApprovalKind::Diff,
    });
    app.set_overlay(Overlay::Approval);
    let terminal = render(&mut app, 100, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Delete data/Empty.md"));
    assert!(screen.contains("No changes to display"));
    assert!(screen.contains("Enter/Y approve"));
    let overlay = app.layout.overlay.expect("approval overlay");
    assert!(
        overlay.height >= 4,
        "approval panel must keep room for the footer row"
    );

    let wide = render(&mut app, 180, 24);
    assert!(buffer_string(&wide).contains("No changes to display"));
}

#[test]
fn command_approval_keeps_the_chat_width_pty_monitor_above_it() {
    let (mut app, _directory) = make_app();
    app.center_view = CenterView::Chat;
    app.agent_terminal
        .set_monitor_snapshot_for_test(AgentTerminalSnapshot {
            title: "ssh build-host".to_string(),
            status: AgentTerminalStatus::Running,
            terminal: TerminalSnapshot::from_bytes(
                24,
                80,
                b"zero\r\none\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven\r\neight\r\nnine\r\nten\r\neleven\r\ntwelve\r\nthirteen\r\nfourteen\r\n    15\tfifteen",
            ),
        });
    app.approval_request = Some(ApprovalRequest {
        title: "Run shell command".to_string(),
        message: String::new(),
        kind: ApprovalKind::Command(CommandApproval {
            purpose: "Run Markdown format checks and report problems".to_string(),
            label: "Cmd".to_string(),
            code: "markdownlint data/note.md\nprintf '%s\\n' done".to_string(),
        }),
    });
    app.set_overlay(Overlay::Approval);

    let terminal = render(&mut app, 220, 32);
    let screen = buffer_string(&terminal);
    let center = app.layout.center.expect("center panel");
    let compose = app.layout.compose.expect("Agent compose");
    let chat = inset_horizontal(center_content_axis(center), 2);
    let overlay = app.layout.overlay.expect("approval overlay");
    assert_eq!(overlay.width, DIALOG_WIDTH);
    assert_eq!(compose.x, chat.x + 2);
    assert_eq!(compose.width + 4, chat.width);
    assert!(screen.contains("PTY · ssh build-host · running"));
    assert!(!screen.contains("zero"));
    for value in [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve", "thirteen", "fourteen", "fifteen",
    ] {
        assert!(screen.contains(value), "missing monitor row {value}");
    }
    assert_eq!(
        terminal.backend().buffer()[(chat.x, center.y + AGENT_TERMINAL_MONITOR_ROWS - 1)].symbol(),
        "└"
    );
    assert_eq!(
        terminal.backend().buffer()[(
            chat.x + chat.width - 1,
            center.y + AGENT_TERMINAL_MONITOR_ROWS - 1,
        )]
            .symbol(),
        "┘"
    );
    assert!(
        (center.x..center.x + center.width).all(|x| terminal.backend().buffer()
            [(x, center.y + AGENT_TERMINAL_MONITOR_ROWS)]
            .symbol()
            == " "),
        "PTY monitor needs a blank row below its border"
    );
    assert!(
        overlay.y >= center.y + AGENT_TERMINAL_RESERVED_ROWS,
        "approval must start below the PTY monitor and its trailing blank row"
    );
    assert!(screen.contains("Agent: Run Markdown format checks and report problems"));
    assert!(screen.contains("Cmd:"));
    assert!(screen.contains("markdownlint data/note.md"));
    assert!(screen.contains("printf '%s\\n' done"));
    assert!(screen.contains("Enter/Y approve"));
    let (command_y, command_x) = screen
        .lines()
        .enumerate()
        .find_map(|(y, line)| {
            line.find("markdownlint")
                .map(|byte| (y as u16, UnicodeWidthStr::width(&line[..byte]) as u16))
        })
        .expect("highlighted command");
    let command_cell = &terminal.backend().buffer()[(command_x, command_y)];
    assert_eq!(command_cell.fg, app.theme.markdown_link);
    assert_eq!(command_cell.bg, app.theme.markdown_code_block_background);
    assert_eq!(
        app.agent_terminal
            .monitor_snapshot(80)
            .expect("monitor snapshot")
            .terminal
            .size(),
        (24, 80),
        "the 15-row monitor must not resize the PTY"
    );
    assert!(animations_active(&app, true));
}

#[test]
fn command_approval_wraps_long_commands_with_spaced_sections() {
    let (mut app, _directory) = make_app();
    let long_argument = "x".repeat(120);
    app.approval_request = Some(ApprovalRequest {
        title: "Run shell command".to_string(),
        message: String::new(),
        kind: ApprovalKind::Command(CommandApproval {
            purpose: "Inspect a generated artifact".to_string(),
            label: "Cmd".to_string(),
            code: format!("printf {long_argument}"),
        }),
    });
    app.set_overlay(Overlay::Approval);

    let terminal = render(&mut app, 170, 30);
    let overlay = app.layout.overlay.expect("command approval overlay");
    let screen = buffer_string(&terminal);
    let rows = screen.lines().collect::<Vec<_>>();
    let agent_y = rows
        .iter()
        .position(|row| row.contains("Agent: Inspect a generated artifact"))
        .expect("purpose row");
    let label_y = rows
        .iter()
        .position(|row| row.contains("Cmd:"))
        .expect("command label row");
    let command_y = rows
        .iter()
        .position(|row| row.contains("printf "))
        .expect("first command row");
    assert_eq!(overlay.width, DIALOG_WIDTH);
    assert_eq!(label_y, agent_y + 2, "purpose needs a trailing blank row");
    assert_eq!(command_y, label_y + 2, "label needs a trailing blank row");

    let buffer = terminal.backend().buffer();
    let argument_cells = (overlay.y..overlay.y + overlay.height)
        .flat_map(|y| (overlay.x..overlay.x + overlay.width).map(move |x| (x, y)))
        .filter(|&(x, y)| buffer[(x, y)].symbol() == "x")
        .count();
    assert_eq!(argument_cells, long_argument.len());
    let footer_y = overlay.y + overlay.height - 2;
    assert!(
        (overlay.x + 2..overlay.x + overlay.width - 2)
            .all(|x| buffer[(x, footer_y - 1)].symbol() == " "),
        "command needs a blank row before the footer"
    );
}

#[test]
fn command_highlighting_treats_embedded_hashes_as_word_content() {
    let spans = shell_highlight_line("printf %s foo#bar", Theme::default());
    assert_eq!(
        spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "printf %s foo#bar"
    );
}

#[test]
fn agent_prompt_and_diff_approval_render_as_opaque_overlays() {
    let (mut app, _directory) = make_app();
    app.ai_prompt_input = "summarize this".to_string();
    app.ai_prompt_cursor = app.ai_prompt_input.chars().count();
    app.set_overlay(Overlay::AiPrompt);
    let terminal = render(&mut app, 100, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Agent prompt"));
    assert!(screen.contains("summarize this"));

    app.approval_request = Some(ApprovalRequest {
        title: "Update data/note.md".to_string(),
        message: "--- old\n+++ new\n@@ -1 +1 @@\n-old value\n+new value\n".to_string(),
        kind: ApprovalKind::Diff,
    });
    app.set_overlay(Overlay::Approval);
    let terminal = render(&mut app, 100, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Update data/note.md"));
    assert!(screen.contains("-old value"));
    assert!(screen.contains("+new value"));
    assert!(screen.contains("Tab mode"));
}

#[test]
fn confirm_approvals_distinguish_regular_actions_from_destructive_actions() {
    let (mut app, _directory) = make_app();
    app.approval_request = Some(ApprovalRequest {
        title: "Export file".to_string(),
        message: "Export data/Note.md as PDF to Note.pdf?".to_string(),
        kind: ApprovalKind::Confirm,
    });
    app.set_overlay(Overlay::Approval);
    let terminal = render(&mut app, 100, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Export file"));
    assert!(screen.contains("Enter/Y confirm · N/Esc cancel"));
    let overlay = app.layout.overlay.expect("confirm overlay");
    let corner_fg = terminal.backend().buffer()[(overlay.x, overlay.y)].fg;
    assert_eq!(corner_fg, app.theme.ui_warning);
    assert_ne!(corner_fg, app.theme.ui_error);

    app.approval_request = Some(ApprovalRequest {
        title: "Delete file".to_string(),
        message: "Delete data/Empty.md?".to_string(),
        kind: ApprovalKind::DestructiveConfirm,
    });
    app.set_overlay(Overlay::Approval);
    let terminal = render(&mut app, 100, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Delete file"));
    assert!(screen.contains("Delete data/Empty.md?"));
    assert!(screen.contains("Enter/Y confirm · N/Esc cancel"));
    assert!(
        !screen.contains("Tab mode"),
        "confirm approvals must not use the diff panel"
    );
    let overlay = app.layout.overlay.expect("confirm overlay");
    assert_eq!(overlay.height, 5);
    let corner_fg = terminal.backend().buffer()[(overlay.x, overlay.y)].fg;
    assert_eq!(
        corner_fg, app.theme.ui_error,
        "delete confirmation should use the destructive border"
    );
}

#[test]
fn agent_diff_approval_switches_layout_with_terminal_width() {
    let (mut app, _directory) = make_app();
    app.approval_request = Some(ApprovalRequest {
        title: "Update data/note.md".to_string(),
        message: "--- old\n+++ new\n@@ -9,1 +9,1 @@\n-old value\n+new value\n".to_string(),
        kind: ApprovalKind::Diff,
    });
    app.set_overlay(Overlay::Approval);

    let narrow = render(&mut app, 130, 24);
    let narrow_screen = buffer_string(&narrow);
    let narrow_overlay = app.layout.overlay.unwrap();
    assert_eq!(narrow_overlay.width, APPROVAL_UNIFIED_WIDTH);
    assert_eq!(narrow_overlay.height, 8);
    assert!(!narrow_screen
        .lines()
        .any(|line| line.contains("-old value") && line.contains("+new value")));

    let wide = render(&mut app, 180, 24);
    let wide_screen = buffer_string(&wide);
    let wide_overlay = app.layout.overlay.unwrap();
    assert_eq!(wide_overlay.width, APPROVAL_SIDE_BY_SIDE_WIDTH);
    assert_eq!(wide_overlay.height, 6);
    let changed_line = wide_screen
        .lines()
        .find(|line| line.contains("-old value") && line.contains("+new value"))
        .expect("side-by-side change row");
    assert!(changed_line.contains("-old value"));
    assert!(changed_line.contains("│   9  9 │ +new value"));
    assert!(!changed_line.contains('┃'));

    let hunk_line = wide_screen
        .lines()
        .find(|line| line.contains("@@ -9,1 +9,1 @@"))
        .expect("hunk row");
    assert!(!hunk_line.contains('┃'));

    let buffer = wide.backend().buffer();
    let changed_y = (0..buffer.area.height)
        .find(|&y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .contains("-old value")
        })
        .expect("changed row");
    let content_start = wide_overlay.x + 2;
    let column_boundary = content_start + (wide_overlay.width - 4) / 2;
    assert!((content_start..column_boundary).all(|x| matches!(
        buffer[(x, changed_y)].bg,
        color if color == app.theme.diff_deletion_background || color == app.theme.ui_error
    )));
    assert!(
        (content_start..column_boundary).any(|x| buffer[(x, changed_y)].bg == app.theme.ui_error)
    );
    assert!(
        (column_boundary..wide_overlay.x + wide_overlay.width - 2).all(|x| matches!(
            buffer[(x, changed_y)].bg,
            color if color == app.theme.diff_addition_background || color == app.theme.ui_task_done
        ))
    );
    assert!((column_boundary..wide_overlay.x + wide_overlay.width - 2)
        .any(|x| buffer[(x, changed_y)].bg == app.theme.ui_task_done));
}

#[test]
fn unified_diff_uses_full_width_backgrounds_with_intraline_emphasis() {
    let theme = Theme::default();
    let lines = unified_diff_lines("@@ -1 +1 @@\n-old\n+new\n", 12, theme);

    assert_eq!(lines.len(), 3);
    assert_eq!(
        lines[1]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(theme.ui_error))
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "old"
    );
    assert_eq!(lines[1].width(), 12);
    assert_eq!(
        lines[2]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(theme.ui_task_done))
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "new"
    );
    assert_eq!(lines[2].width(), 12);
    assert_eq!(
        lines[1].spans.last().unwrap().style.bg,
        Some(theme.diff_deletion_background)
    );
    assert_eq!(
        lines[2].spans.last().unwrap().style.bg,
        Some(theme.diff_addition_background)
    );
}

#[test]
fn diff_views_emphasize_the_exact_unicode_replacement() {
    let theme = Theme::default();
    let diff = "@@ -7 +7 @@\n-let title = \"你好世界\";\n+let title = \"你好 Rust\";\n";

    let unified = unified_diff_lines(diff, 50, theme);
    assert_eq!(
        unified[1]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(theme.ui_error))
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "世界"
    );
    assert_eq!(
        unified[2]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(theme.ui_task_done))
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        " Rust"
    );

    let wrapped = unified_diff_lines(diff, 12, theme);
    assert_eq!(
        wrapped
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.style.bg == Some(theme.ui_error))
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "世界"
    );
    assert_eq!(
        wrapped
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.style.bg == Some(theme.ui_task_done))
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        " Rust"
    );

    let side_by_side = side_by_side_diff_lines(diff, 100, theme);
    assert_eq!(
        side_by_side[1]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(theme.ui_error))
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "世界"
    );
    assert_eq!(
        side_by_side[1]
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(theme.ui_task_done))
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        " Rust"
    );
}

#[test]
fn diff_alignment_pairs_the_most_similar_lines_and_leaves_insertions_unemphasized() {
    let rows = side_by_side_diff_rows(
        "@@ -4,2 +4 @@\n-obsolete setting\n-let value = old\n+let value = new\n",
    );
    assert_eq!(
        rows[1],
        SideBySideDiffRow::Columns {
            before: Some(SideBySideDiffCell::new(
                "-obsolete setting",
                DiffLineKind::Deletion,
                Some(4),
            )),
            after: None,
        }
    );
    assert_eq!(
        rows[2],
        SideBySideDiffRow::Columns {
            before: Some(SideBySideDiffCell::new(
                "-let value = old",
                DiffLineKind::Deletion,
                Some(5),
            )),
            after: Some(SideBySideDiffCell::new(
                "+let value = new",
                DiffLineKind::Addition,
                Some(4),
            )),
        }
    );

    let theme = Theme::default();
    let insertion = unified_diff_lines("@@ -0,0 +1 @@\n+entirely new\n", 24, theme);
    assert!(insertion[1]
        .spans
        .iter()
        .all(|span| span.style.bg == Some(theme.diff_addition_background)));

    let long_old = "a".repeat(MAX_INTRALINE_BYTES + 1);
    let long_new = "b".repeat(MAX_INTRALINE_BYTES + 1);
    let oversized = unified_diff_lines(
        &format!("@@ -1 +1 @@\n-{long_old}\n+{long_new}\n"),
        80,
        theme,
    );
    assert!(oversized
        .iter()
        .flat_map(|line| &line.spans)
        .all(|span| span.style.bg != Some(theme.ui_error)
            && span.style.bg != Some(theme.ui_task_done)));
}

#[test]
fn long_approval_diff_keeps_the_bounded_height() {
    let (mut app, _directory) = make_app();
    app.approval_request = Some(ApprovalRequest {
        title: "Update data/note.md".to_string(),
        message: (0..50).map(|line| format!("+line {line}\n")).collect(),
        kind: ApprovalKind::Diff,
    });
    app.set_overlay(Overlay::Approval);

    render(&mut app, 130, 50);

    assert_eq!(app.layout.overlay.unwrap().height, 36);
}

#[test]
fn side_by_side_diff_pairs_change_blocks_and_repeats_context() {
    let rows = side_by_side_diff_rows(
        "--- old\n+++ new\n@@ -1,3 +1,3 @@\n same\n-old one\n-old two\n+new one\n tail\n",
    );

    assert_eq!(
        rows,
        vec![
            SideBySideDiffRow::Columns {
                before: Some(SideBySideDiffCell::new(
                    "--- old",
                    DiffLineKind::Header,
                    None,
                )),
                after: Some(SideBySideDiffCell::new(
                    "+++ new",
                    DiffLineKind::Header,
                    None,
                )),
            },
            SideBySideDiffRow::Full("@@ -1,3 +1,3 @@", DiffLineKind::Hunk),
            SideBySideDiffRow::Columns {
                before: Some(SideBySideDiffCell::new(
                    " same",
                    DiffLineKind::Context,
                    Some(1),
                )),
                after: Some(SideBySideDiffCell::new(
                    " same",
                    DiffLineKind::Context,
                    Some(1),
                )),
            },
            SideBySideDiffRow::Columns {
                before: Some(SideBySideDiffCell::new(
                    "-old one",
                    DiffLineKind::Deletion,
                    Some(2),
                )),
                after: Some(SideBySideDiffCell::new(
                    "+new one",
                    DiffLineKind::Addition,
                    Some(2),
                )),
            },
            SideBySideDiffRow::Columns {
                before: Some(SideBySideDiffCell::new(
                    "-old two",
                    DiffLineKind::Deletion,
                    Some(3),
                )),
                after: None,
            },
            SideBySideDiffRow::Columns {
                before: Some(SideBySideDiffCell::new(
                    " tail",
                    DiffLineKind::Context,
                    Some(4),
                )),
                after: Some(SideBySideDiffCell::new(
                    " tail",
                    DiffLineKind::Context,
                    Some(3),
                )),
            },
        ]
    );
}

#[test]
fn side_by_side_diff_does_not_treat_changed_prefixes_as_file_headers() {
    let rows = side_by_side_diff_rows(
        "--- note.md\n+++ note.md\n@@ -1 +1 @@\n--- old heading\n+++ new heading\n",
    );

    assert_eq!(
        rows[2],
        SideBySideDiffRow::Columns {
            before: Some(SideBySideDiffCell::new(
                "--- old heading",
                DiffLineKind::Deletion,
                Some(1),
            )),
            after: Some(SideBySideDiffCell::new(
                "+++ new heading",
                DiffLineKind::Addition,
                Some(1),
            )),
        }
    );
}

#[test]
fn side_by_side_diff_tracks_line_numbers_across_multiple_hunks() {
    let rows = side_by_side_diff_rows(
        "@@ -8,2 +10,3 @@\n old\n-removed\n+added\n+extra\n@@ -40 +50 @@\n-last\n+next\n",
    );

    let numbered = rows
        .into_iter()
        .filter_map(|row| match row {
            SideBySideDiffRow::Columns { before, after } => Some((
                before.and_then(|cell| cell.line_number),
                after.and_then(|cell| cell.line_number),
            )),
            SideBySideDiffRow::Full(_, _) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        numbered,
        [
            (Some(8), Some(10)),
            (Some(9), Some(11)),
            (None, Some(12)),
            (Some(40), Some(50)),
        ]
    );
}

#[test]
fn side_by_side_diff_aligns_context_insertions_and_deletions() {
    let diff = "--- old\n+++ new\n@@ -1,3 +1,3 @@\n 测试文本\n+插入一行\n 第二行\n-第三行\n";
    let rows = side_by_side_diff_rows(diff);

    assert_eq!(
        rows[3],
        SideBySideDiffRow::Columns {
            before: None,
            after: Some(SideBySideDiffCell::new(
                "+插入一行",
                DiffLineKind::Addition,
                Some(2),
            )),
        }
    );
    assert_eq!(
        rows[5],
        SideBySideDiffRow::Columns {
            before: Some(SideBySideDiffCell::new(
                "-第三行",
                DiffLineKind::Deletion,
                Some(3),
            )),
            after: None,
        }
    );

    let rendered = side_by_side_diff_lines(diff, 40, Theme::default())
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content)
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(rendered[3].contains("  2 │ +插入一行"));
    assert_eq!(rendered[3].matches('│').count(), 1);
    assert!(rendered[5].contains("-第三行"));
    assert!(rendered[5].contains("│   3"));
    assert_eq!(rendered[5].matches('│').count(), 1);
}

#[test]
fn ask_user_overlay_renders_choices_and_free_text_input() {
    let (mut app, _directory) = make_app();
    app.ask_user_request = Some(crate::agent::AskUserRequest {
        kind: AskUserKind::Tool,
        question: "Which output format should be used?".to_string(),
        options: vec!["Markdown".to_string(), "MBDown".to_string()],
    });
    app.ask_user_option = 0;
    app.set_overlay(Overlay::AskUser);
    let terminal = render(&mut app, 100, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Agent question"));
    assert!(screen.contains("Which output format should be used?"));
    assert!(screen.contains("Markdown"));
    assert!(screen.contains("MBDown"));
    assert!(screen.contains("Other answer"));
    assert!(screen.contains("Your answer"));
    assert!(!screen.contains("> Markdown"));
    assert!(!screen.contains("> Other answer"));
    let overlay = app.layout.overlay.expect("ask-user overlay");
    assert_eq!(overlay.width, DIALOG_WIDTH);
    assert_eq!(overlay.height, 15);
    assert_eq!(app.dialog_hitboxes.len(), 3);
    let selected = app
        .dialog_hitboxes
        .iter()
        .find(|hitbox| hitbox.index == 0)
        .expect("selected Markdown option");
    let mbdown = app
        .dialog_hitboxes
        .iter()
        .find(|hitbox| hitbox.index == 1)
        .expect("MBDown option");
    let other = app
        .dialog_hitboxes
        .iter()
        .find(|hitbox| hitbox.index == 2)
        .expect("Other answer option");
    assert_eq!(selected.area.height, SELECT_OPTION_HEIGHT);
    assert_eq!(mbdown.area.height, SELECT_OPTION_HEIGHT);
    assert_eq!(other.area.height, SELECT_OPTION_HEIGHT);
    assert_eq!(mbdown.area.y, selected.area.y + SELECT_OPTION_HEIGHT);
    assert_eq!(other.area.y, mbdown.area.y + SELECT_OPTION_HEIGHT);
    let buffer = terminal.backend().buffer();
    for y in selected.area.y.saturating_sub(1)..selected.area.y + selected.area.height {
        assert_eq!(
            buffer[(selected.area.x + selected.area.width - 1, y)].bg,
            app.theme.selection_background,
            "selection must include both shared blank rows across the full list width"
        );
        assert_eq!(buffer[(selected.area.x, y)].symbol(), "▌");
        assert_eq!(
            buffer[(selected.area.x, y)].fg,
            app.theme.selection_indicator
        );
    }
    assert!(app.hitboxes.is_empty());
}

#[test]
fn private_terminal_input_renders_only_a_mask() {
    let (mut app, _directory) = make_app();
    app.private_terminal_input_request = Some(PrivateTerminalInputRequest {
        session_id: "terminal-7".to_string(),
        purpose: "Authenticate the deployment".to_string(),
        prompt: "Enter the SSH passphrase".to_string(),
    });
    app.set_overlay(Overlay::PrivateTerminalInput);
    app.handle_paste("never-render-this");

    let terminal = render(&mut app, 100, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Private terminal input"));
    assert!(screen.contains("Authenticate the deployment"));
    assert!(screen.contains("Enter the SSH passphrase"));
    assert!(screen.contains("terminal-7"));
    assert!(screen.contains(&"•".repeat("never-render-this".chars().count())));
    assert!(!screen.contains("never-render-this"));
    assert!(screen.contains("Enter submit · Esc cancel"));
}

#[test]
fn round_limit_dialog_only_offers_continue_or_stop() {
    let (mut app, _directory) = make_app();
    app.ask_user_request = Some(crate::agent::AskUserRequest {
        kind: AskUserKind::RoundLimit,
        question: "Continue for up to 25 more rounds?".to_string(),
        options: vec!["Continue".to_string(), "Stop".to_string()],
    });
    app.set_overlay(Overlay::AskUser);

    let terminal = render(&mut app, 100, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Agent round limit"));
    assert!(screen.contains("Continue"));
    assert!(screen.contains("Stop"));
    assert!(!screen.contains("Other answer"));
    assert!(!screen.contains("Your answer"));
    assert!(screen.contains("Esc stop"));
}

#[test]
fn agent_panel_shows_user_before_agent_with_source_backgrounds() {
    let (mut app, _directory) = make_app();
    app.agent_panel = vec![
        Arc::new(AgentPanelEntry::Prompt {
            text: "Explain the selected note".to_string(),
            muted: false,
        }),
        Arc::new(AgentPanelEntry::Assistant {
            text: "Here is the explanation".to_string(),
            streaming: false,
            final_output: true,
        }),
    ];
    app.focus = Focus::Agent;

    let terminal = render(&mut app, 170, 40);
    let screen = buffer_string(&terminal);
    let prompt = screen.find("Explain the selected note").unwrap();
    let response = screen.find("Here is the explanation").unwrap();
    assert!(prompt < response);

    let (user_lines, _, _) = render_agent_entry(&app.agent_panel[0], 40, 0, false);
    let (agent_lines, _, _) = render_agent_entry(&app.agent_panel[1], 40, 0, false);
    assert_eq!(user_lines[1].to_string().trim_end(), "User");
    assert_eq!(agent_lines[1].to_string().trim_end(), "Agent");
    assert!(user_lines.first().unwrap().to_string().trim().is_empty());
    assert!(user_lines.last().unwrap().to_string().trim().is_empty());
    assert!(agent_lines.first().unwrap().to_string().trim().is_empty());
    assert!(agent_lines.last().unwrap().to_string().trim().is_empty());
    // Every row except the trailing shared blank carries the source background.
    assert!(user_lines.iter().take(user_lines.len() - 1).all(|line| {
        UnicodeWidthStr::width(line.to_string().as_str()) == 40
            && line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(ctp::SURFACE_0))
    }));
    assert!(agent_lines.iter().take(agent_lines.len() - 1).all(|line| {
        UnicodeWidthStr::width(line.to_string().as_str()) == 40
            && line
                .spans
                .iter()
                .all(|span| span.style.bg == Some(ctp::BASE))
    }));

    let agent_area = app.layout.agent.unwrap();
    let rows = screen.lines().collect::<Vec<_>>();
    let user_text_row = rows
        .iter()
        .position(|line| line.contains("Explain the selected note"))
        .unwrap();
    let agent_text_row = rows
        .iter()
        .position(|line| line.contains("Here is the explanation"))
        .unwrap();
    assert_eq!(agent_text_row - user_text_row, 6);
    for (needle, background) in [
        ("Explain the selected note", ctp::SURFACE_0),
        ("Here is the explanation", ctp::BASE),
    ] {
        let y = rows.iter().position(|line| line.contains(needle)).unwrap() as u16;
        for x in agent_area.x + 1..agent_area.x + agent_area.width - 1 {
            assert_eq!(terminal.backend().buffer()[(x, y)].bg, background);
        }
    }
}

#[test]
fn daily_and_agent_render_caches_offset_markdown_images() {
    let date = NaiveDate::from_ymd_opt(2026, 7, 27).unwrap();
    let daily = crate::model::DailyNote {
        date,
        body: "![Daily diagram](../data/diagram.png)".to_string(),
    };
    let cached = render_daily_note(&daily, "2026-07-27".to_string(), 100);
    assert_eq!(cached.images.len(), 1);
    assert!(cached.images[0].row >= 4);
    assert!(cached.images[0].column > 0);
    assert_eq!(cached.images[0].height, 12);

    let entry = AgentPanelEntry::Assistant {
        text: "![Agent diagram](https://example.com/diagram.png)".to_string(),
        streaming: false,
        final_output: true,
    };
    let (lines, _, images) = render_agent_entry(&entry, 40, 0, false);
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].row, 3);
    assert_eq!(images[0].width, 40);
    assert_eq!(lines.len(), 17);
}

#[test]
fn document_keeps_compose_visible_and_notification_uses_top_right() {
    let (mut app, _directory) = make_app();
    app.center_view = CenterView::Document;
    app.focus = Focus::Center;
    app.document = Some(Document {
        kind: DocumentKind::File(app.storage.archives_dir.join("2026-07-27.md")),
        title: "Article".to_string(),
        source: "# Reading\n\nUseful paragraph".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    app.notifications.notify("Recorded in Daily");
    let terminal = render(&mut app, 120, 24);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("Reading"));
    assert!(screen.contains("Compose"));
    assert!(screen.contains("Notification"));
    assert!(screen.contains("Recorded in Daily"));
    let compose = app.layout.compose.expect("document compose");
    assert!(compose.y > 12);
}

#[test]
fn focused_compose_sits_below_the_document_with_an_animated_border() {
    let (mut app, _directory) = make_app();
    app.center_view = CenterView::Document;
    app.focus = Focus::Compose;
    app.animation_tick = 3;
    app.document = Some(Document {
        kind: DocumentKind::File(app.storage.data_dir.join("Article.md")),
        title: "Article".to_string(),
        source: (0..40)
            .map(|line| format!("paragraph {line}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
        scroll: u16::MAX,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });

    let terminal = render(&mut app, 120, 30);
    let compose = app.layout.compose.expect("document compose");
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(compose.x, compose.y)].symbol(), "┌");
    assert_eq!(
        buffer[(compose.x, compose.y)].fg,
        animated_color(0, app.animation_tick)
    );
    assert_eq!(
        buffer[(compose.x + 1, compose.y)].fg,
        animated_color(1, app.animation_tick)
    );

    let center = app.layout.center.expect("center area");
    let content = inset_horizontal(center_content_axis(center), 2);
    assert!(compose.x > content.x);
    assert_eq!(buffer[(content.x, compose.y - 3)].bg, ctp::MANTLE);
    assert_eq!(buffer[(content.x, compose.y - 2)].bg, Color::Reset);
    assert_eq!(buffer[(content.x, compose.y - 1)].bg, Color::Reset);
    assert_eq!(buffer[(content.x, compose.y + 1)].bg, Color::Reset);
    assert_eq!(buffer[(compose.x + 1, compose.y + 1)].bg, ctp::SURFACE_0);
    let last_paragraph_y = (0..buffer.area().height)
        .find(|y| {
            (0..buffer.area().width)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .contains("paragraph 39")
        })
        .expect("the final paragraph should remain visible above Compose");
    assert!(last_paragraph_y < compose.y.saturating_sub(1));
}

#[test]
fn document_paper_scrolls_behind_the_compose_while_content_remains() {
    let (mut app, _directory) = make_app();
    app.center_view = CenterView::Document;
    app.document = Some(Document {
        kind: DocumentKind::File(app.storage.data_dir.join("Article.md")),
        title: "Article".to_string(),
        source: (0..40)
            .map(|line| format!("paper line {line}"))
            .collect::<Vec<_>>()
            .join("\n\n"),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });

    let terminal = render(&mut app, 120, 30);
    let center = app.layout.center.expect("center area");
    let content = inset_horizontal(center_content_axis(center), 2);
    let compose = app.layout.compose.expect("document compose");
    let paper_y = content.y + 2;
    let buffer_row = |buffer: &Buffer, y| {
        (0..buffer.area().width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
    };
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(content.x, compose.y + 1)].bg, ctp::MANTLE);
    assert_eq!(buffer[(compose.x + 1, compose.y + 1)].bg, ctp::SURFACE_0);
    assert!(!buffer_row(buffer, paper_y).contains("paper line 0"));
    assert!(buffer_row(buffer, paper_y + 2).contains("paper line 0"));

    app.document.as_mut().unwrap().scroll = 1;
    let terminal = render(&mut app, 120, 30);
    assert!(buffer_row(terminal.backend().buffer(), paper_y + 1).contains("paper line 0"));

    app.document.as_mut().unwrap().scroll = 2;
    let terminal = render(&mut app, 120, 30);
    assert!(buffer_row(terminal.backend().buffer(), paper_y).contains("paper line 0"));
}

#[test]
fn tiny_terminals_and_requested_widths_do_not_panic() {
    for (width, height) in [
        (1, 1),
        (2, 2),
        (5, 3),
        (20, 4),
        (60, 8),
        (80, 8),
        (120, 8),
        (169, 8),
        (170, 8),
        (171, 8),
        (220, 8),
    ] {
        let (mut app, _directory) = make_app();
        app.input = "wide 字\nsecond line".to_string();
        app.input_cursor = app.input.chars().count();
        render(&mut app, width, height);
    }
}

#[test]
fn multiline_chat_and_compose_content_render() {
    let (mut app, _directory) = make_app();
    app.storage.append_to_today("alpha\nbeta **bold**").unwrap();
    app.reload();
    app.focus = Focus::Compose;
    app.input = "first\nsecond".to_string();
    app.input_cursor = app.input.chars().count();
    let terminal = render(&mut app, 120, 24);
    let screen = buffer_string(&terminal);
    for expected in ["alpha", "beta bold", "first", "second"] {
        assert!(screen.contains(expected), "missing {expected}");
    }
}

#[test]
fn todo_items_wrap_and_keep_the_whole_item_clickable() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.center_view = CenterView::Todo;
    app.todo_items = vec![TodoItem {
        checked: false,
        text: "a todo item whose content is deliberately longer than the panel".to_string(),
    }];
    let terminal = render(&mut app, 170, 18);
    let screen = buffer_string(&terminal);
    assert!(screen.contains("a todo item whose content"));
    assert!(screen.contains("longer"));
    assert!(screen.contains("panel"));
    assert_eq!(app.todo_hitboxes.len(), 1);
    assert!(app.todo_hitboxes[0].area.height > 1);
}

#[test]
fn todo_items_share_a_blank_row_included_in_selection_and_hitbox() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.center_view = CenterView::Todo;
    app.todo_items = vec![
        TodoItem {
            checked: false,
            text: "first task".to_string(),
        },
        TodoItem {
            checked: true,
            text: "second task".to_string(),
        },
    ];
    app.todo_index = 1;

    let terminal = render(&mut app, 170, 24);
    let screen = buffer_string(&terminal);
    let lines = screen.lines().collect::<Vec<_>>();
    let first_row = lines
        .iter()
        .position(|line| line.contains("first task"))
        .unwrap();
    let second_row = lines
        .iter()
        .position(|line| line.contains("second task"))
        .unwrap();
    assert_eq!(second_row, first_row + 2);
    assert_eq!(app.todo_hitboxes.len(), 2);
    assert_eq!(app.todo_hitboxes[0].area.height, 2);
    let todo = app.layout.center.unwrap();
    let first = app.todo_hitboxes[0].area;
    let last = app.todo_hitboxes[1].area;
    assert_eq!(
        first.y,
        todo.y + 5,
        "the first todo needs a blank row below the page header"
    );
    assert_eq!(
        terminal.backend().buffer()[(first.x, first.y + first.height - 1)].bg,
        ctp::SURFACE_1,
        "the selected background should include the shared blank row"
    );
    assert_eq!(
        terminal.backend().buffer()[(first.x + 2, first.y)].symbol(),
        "["
    );
    assert_eq!(
        terminal.backend().buffer()[(last.x + 2, last.y)].symbol(),
        "["
    );
    let last_margin = &terminal.backend().buffer()[(last.x + 1, last.y + last.height - 1)];
    assert_eq!(last_margin.symbol(), " ");
    assert_eq!(last_margin.bg, ctp::SURFACE_1);
    assert!(!last_margin.modifier.contains(Modifier::CROSSED_OUT));
    assert!(!terminal.backend().buffer()[(last.x, last.y - 1)]
        .modifier
        .contains(Modifier::CROSSED_OUT));
    assert!((last.x..last.x + last.width).any(|x| {
        let cell = &terminal.backend().buffer()[(x, last.y)];
        cell.modifier.contains(Modifier::CROSSED_OUT) && cell.symbol() != " "
    }));
    for y in last.y.saturating_sub(1)..last.y + last.height {
        let cell = &terminal.backend().buffer()[(last.x, y)];
        assert_eq!(cell.symbol(), "▌");
        assert_eq!(cell.fg, app.theme.selection_indicator);
    }
}

#[test]
fn todo_display_groups_open_items_before_completed_items() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.center_view = CenterView::Todo;
    app.todo_items = vec![
        TodoItem {
            checked: true,
            text: "finished task".to_string(),
        },
        TodoItem {
            checked: false,
            text: "open task".to_string(),
        },
    ];
    app.todo_index = 1;
    let terminal = render(&mut app, 170, 18);
    let screen = buffer_string(&terminal);
    assert!(screen.find("open task") < screen.find("finished task"));
    assert_eq!(
        app.todo_hitboxes
            .iter()
            .map(|hitbox| hitbox.index)
            .collect::<Vec<_>>(),
        vec![1, 0],
        "hitboxes must retain daily task source indices"
    );
}

#[test]
fn chat_renders_block_markdown_on_colored_cards() {
    let (mut app, _directory) = make_app();
    app.storage
        .append_to_today(concat!(
            "# Heading\n\n- first\n- second\n\n`code`\n\n",
            "[columns gap=2]\n",
            "[column]Left[/column]\n",
            "[column]Right[/column]\n",
            "[/columns]\n\n",
            "[bg=196]colored[/bg]"
        ))
        .unwrap();
    app.reload();
    let date = app.daily_notes[0].date.format("%Y-%m-%d").to_string();
    let terminal = render(&mut app, 170, 40);
    let screen = buffer_string(&terminal);
    let screen_lines = screen.lines().collect::<Vec<_>>();
    let date_row = screen_lines
        .iter()
        .position(|line| line.contains(&date))
        .expect("missing DailyNote date");
    let heading_row = screen_lines
        .iter()
        .position(|line| line.contains("Heading"))
        .expect("missing body heading");
    assert_eq!(
        heading_row,
        date_row + 2,
        "date and body need one blank row"
    );
    assert!(
        screen_lines[heading_row].find("Heading").unwrap()
            >= screen_lines[date_row].find(&date).unwrap(),
        "date and body should use the same centered content axis"
    );
    let buffer = terminal.backend().buffer();
    assert!(buffer.content().iter().any(|cell| {
        cell.symbol() == date.chars().next().unwrap().to_string()
            && cell.modifier.contains(Modifier::BOLD)
            && cell.modifier.contains(Modifier::UNDERLINED)
    }));
    for expected in ["Heading", "• first", "• second", "code"] {
        assert!(screen.contains(expected), "missing {expected}");
    }
    assert!(screen
        .lines()
        .any(|line| line.contains("Left") && line.contains("Right")));
    assert!(buffer
        .content()
        .iter()
        .any(|cell| cell.symbol() == "c" && cell.bg == Color::Indexed(196)));
    assert!(!screen.contains("[view]"));
    assert!(
        app.hitboxes
            .iter()
            .all(|hitbox| hitbox.action != Action::View),
        "DailyNotes no longer need a preview button"
    );
    assert!(
        buffer.content().iter().any(|cell| {
            cell.symbol() == "H" && cell.modifier.contains(Modifier::BOLD) && cell.bg == ctp::MANTLE
        }),
        "selection should not alter the Markdown body background"
    );
    let ai = app
        .hitboxes
        .iter()
        .find(|hitbox| hitbox.action == Action::Ai)
        .expect("AI button");
    let center = app.layout.center.expect("center");
    let daily_area = inset_horizontal(center_content_axis(center), 2);
    assert_eq!(
        ai.area.x + ai.area.width,
        daily_area.x + daily_area.width - PAGE_PADDING_X as u16,
        "AI button should share the body content axis"
    );
}

#[test]
fn selected_daily_card_uses_an_animated_gradient_border() {
    let (mut app, _directory) = make_app();
    app.storage.append_to_today("A daily note").unwrap();
    app.reload();
    app.animation_tick = 0;

    let first = render(&mut app, 170, 30);
    let center = app.layout.center.unwrap();
    let daily = inset_horizontal(center_content_axis(center), 2);
    let top = daily.y + 2;
    let first_buffer = first.backend().buffer();
    assert_eq!(first_buffer[(daily.x, top)].symbol(), "┌");
    assert_eq!(first_buffer[(daily.x, top)].fg, animated_color(0, 0));
    assert_eq!(first_buffer[(daily.x + 1, top)].fg, animated_color(1, 0));
    assert_ne!(
        first_buffer[(daily.x, top)].fg,
        first_buffer[(daily.x + 1, top)].fg
    );

    app.animation_tick = 1;
    let second = render(&mut app, 170, 30);
    assert_eq!(
        second.backend().buffer()[(daily.x, top)].fg,
        animated_color(0, 1)
    );
    assert_ne!(
        second.backend().buffer()[(daily.x, top)].fg,
        first_buffer[(daily.x, top)].fg
    );
}

#[test]
fn daily_body_axis_has_symmetric_gutters() {
    let width = 100;
    let metadata_and_gap = DAILY_PADDING_X + UnicodeWidthStr::width("2026-07-27") + 2;
    let (start, body_width) = centered_daily_body_axis(width, metadata_and_gap);
    let trailing = width - start - body_width;

    assert_eq!(start, metadata_and_gap);
    assert_eq!(trailing, start);
    assert_eq!(body_width, 74);
    assert_eq!(PAGE_PADDING_X, metadata_and_gap);
}

#[test]
fn oversized_selected_card_keeps_a_stable_scroll_position() {
    let (mut app, _directory) = make_app();
    let body = (0..80)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.storage.append_to_today(&body).unwrap();
    app.reload();
    app.selected = app.daily_notes.len() - 1;
    app.scroll = u16::MAX;
    app.focus = Focus::Center;

    let first = render(&mut app, 80, 24);
    let first_scroll = app.scroll;
    let first_screen = buffer_string(&first);
    assert!(first_scroll > 0);

    let second = render(&mut app, 80, 24);
    assert_eq!(app.scroll, first_scroll);
    assert_eq!(buffer_string(&second), first_screen);
}

#[test]
fn oversized_card_scroll_can_rest_anywhere_inside_the_card() {
    assert_eq!(stable_card_scroll(10, 10, 100, 20), 10);
    assert_eq!(stable_card_scroll(50, 10, 100, 20), 50);
    assert_eq!(stable_card_scroll(100, 10, 100, 20), 81);
    assert_eq!(stable_card_scroll(81, 10, 100, 20), 81);
}

#[test]
fn selection_viewports_move_the_cursor_before_scrolling_up() {
    let mut start = 0;
    for selected in 0..=3 {
        start = selection_viewport_start(start, selected, 3, 10);
    }
    assert_eq!(start, 1, "moving below the viewport should scroll down");
    assert_eq!(selection_viewport_start(start, 2, 3, 10), 1);
    assert_eq!(selection_viewport_start(start, 1, 3, 10), 1);
    assert_eq!(
        selection_viewport_start(start, 0, 3, 10),
        0,
        "scrolling up should begin only after the cursor reaches the top"
    );

    let heights = [2, 2, 2, 2, 2];
    let start = variable_selection_viewport_start(0, 3, &heights, 6);
    assert_eq!(start, 1);
    assert_eq!(variable_selection_viewport_start(start, 2, &heights, 6), 1);
    assert_eq!(variable_selection_viewport_start(start, 1, &heights, 6), 1);
    assert_eq!(variable_selection_viewport_start(start, 0, &heights, 6), 0);
}

#[test]
fn manual_scroll_can_cross_an_oversized_selected_card_boundary() {
    let (mut app, _directory) = make_app();
    app.storage
        .append_daily("2026-07-26", "previous day")
        .unwrap();
    let long_body = (0..80)
        .map(|line| format!("long line {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.storage.append_daily("2026-07-27", &long_body).unwrap();
    app.reload();
    app.selected = 1;
    app.scroll = u16::MAX;
    app.reveal_selected_daily = true;
    render(&mut app, 80, 24);
    assert!(app.scroll > 0);

    app.scroll = 0;
    app.reveal_selected_daily = false;
    let terminal = render(&mut app, 80, 24);
    assert_eq!(app.scroll, 0);
    assert!(buffer_string(&terminal).contains("previous day"));
    assert_eq!(app.selected, 1);
}

#[test]
fn single_line_daily_card_spaces_date_body_and_buttons() {
    let (mut app, _directory) = make_app();
    app.storage.append_to_today("one line").unwrap();
    app.reload();
    let date = app.daily_notes[0].date.format(DATE_FMT).to_string();

    let terminal = render(&mut app, 170, 30);
    let screen = buffer_string(&terminal);
    let rows = screen.lines().collect::<Vec<_>>();
    let date_row = rows
        .iter()
        .position(|line| line.contains(&date))
        .expect("date row");
    assert!(rows[date_row + 2].contains("one line"));
    let button_row = app
        .hitboxes
        .iter()
        .find(|hitbox| hitbox.action == Action::Delete)
        .expect("delete button")
        .area
        .y as usize;
    assert_eq!(button_row, date_row + 4);
    let buffer = terminal.backend().buffer();
    let center = app.layout.center.expect("center area");
    let sample_x = center.x + center.width / 2;
    assert!(date_row >= 2);
    assert_eq!(buffer[(sample_x, date_row as u16 - 1)].bg, ctp::MANTLE);
    assert_eq!(buffer[(sample_x, date_row as u16 - 2)].bg, ctp::MANTLE);
    assert_eq!(buffer[(sample_x, button_row as u16 + 1)].bg, ctp::MANTLE);
    assert_eq!(buffer[(sample_x, button_row as u16 + 2)].bg, ctp::MANTLE);
    let card_left = inset_horizontal(center_content_axis(center), 2).x;
    assert_eq!(buffer[(card_left, date_row as u16 - 2)].symbol(), "┌");
    assert_eq!(buffer[(card_left, date_row as u16)].symbol(), "│");
    assert_eq!(buffer[(card_left, button_row as u16 + 2)].symbol(), "└");
}

#[test]
fn document_view_uses_a_scrollable_page_margin_without_an_outer_border() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.center_view = CenterView::Document;
    app.document = Some(Document {
        kind: DocumentKind::File(app.storage.archives_dir.join("2026-07-27.md")),
        title: "Archive".to_string(),
        source: "# Heading\n\nintro\n\nneedle".to_string(),
        scroll: 0,
        target_line: Some(5),
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    let terminal = render(&mut app, 80, 30);
    let buffer = terminal.backend().buffer();
    let header: String = (0..80)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    assert!(header.contains("Archive"));
    assert!(!header.contains("Esc back"));
    assert_eq!(buffer[(0, 0)].symbol(), " ");
    assert!(buffer_string(&terminal).contains("Compose"));
    assert!(buffer_string(&terminal).contains("  Archive"));
    assert_eq!(app.document.as_ref().unwrap().scroll, 0);
    assert_eq!(app.document.as_ref().unwrap().target_line, None);
    let first_document_row: String = (0..80)
        .map(|x| buffer[(x, 4)].symbol().to_string())
        .collect();
    assert!(first_document_row.contains("Heading"));
    assert_eq!(buffer[(2, 2)].bg, ctp::MANTLE);
    assert_eq!(buffer[(2, 3)].bg, ctp::MANTLE);
    let heading_x = first_document_row.find("Heading").unwrap() as u16;
    assert!(heading_x >= 2 + PAGE_PADDING_X as u16);
    assert!(buffer_string(&terminal).contains("needle"));
}

#[test]
fn document_scroll_overwrites_box_borders_from_the_previous_frame() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.center_view = CenterView::Document;
    let boxed = (0..40)
        .map(|line| format!("boxed {line} ☀️"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let plain = (0..40)
        .map(|line| format!("plain {line}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    app.document = Some(Document {
        kind: DocumentKind::File(app.storage.data_dir.join("scroll.md")),
        title: "Scroll".to_string(),
        source: format!(
            "{plain}\n\n[box width=full border=single border-color=#df7f3f bg=16]\n{boxed}\n[/box]"
        ),
        scroll: u16::MAX,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });

    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    {
        let completed = terminal
            .draw(|frame| {
                let _ = draw(frame, &mut app);
            })
            .unwrap();
        let box_buffer = completed.buffer;
        assert!(box_buffer
            .content()
            .iter()
            .any(|cell| cell.symbol().contains('\u{fe0f}')));
    }
    assert!(terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .any(|cell| cell.symbol() == "│"));

    app.document.as_mut().unwrap().scroll = 0;
    terminal
        .draw(|frame| {
            let _ = draw(frame, &mut app);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let page = app.layout.center.unwrap();
    let border = Color::Rgb(223, 127, 63);
    assert!(buffer_string(&terminal).contains("plain 0"));
    assert!((page.y..page.y + page.height).all(|y| {
        (page.x..page.x + page.width)
            .all(|x| buffer[(x, y)].symbol() != "│" || buffer[(x, y)].fg != border)
    }));
}

#[test]
fn document_code_block_renders_and_exposes_a_clickable_copy_button() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.center_view = CenterView::Document;
    app.document = Some(Document {
        kind: DocumentKind::File(app.storage.archives_dir.join("2026-07-27.md")),
        title: "Code".to_string(),
        source: "```rust\nfn main() {\n    println!(\"hello\");\n}\n```".to_string(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });

    let terminal = render(&mut app, 80, 30);
    let buffer = terminal.backend().buffer();
    let background = mbtui::Theme::default()
        .code_block
        .bg
        .expect("the default code block theme has a background");
    let compose = app.layout.compose.expect("document compose");
    let rows = (0..compose.y)
        .filter(|y| (0..buffer.area().width).any(|x| buffer[(x, *y)].bg == background))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 7);
    assert!(rows.windows(2).all(|pair| pair[1] == pair[0] + 1));
    let copy = app
        .link_hitboxes
        .iter()
        .find(|hitbox| matches!(hitbox.target, LinkTarget::CopyCode(_)))
        .expect("visible code copy button")
        .clone();
    assert_eq!(
        (copy.area.x..copy.area.x + copy.area.width)
            .map(|x| buffer[(x, copy.area.y)].symbol())
            .collect::<String>(),
        " Copy "
    );
    assert!(buffer_string(&terminal)
        .lines()
        .nth(copy.area.y as usize)
        .is_some_and(|row| row.contains("rust") && row.contains(" Copy ")));
    let copied_source = "fn main() {\n    println!(\"hello\");\n}\n";
    assert_eq!(
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: copy.area.x,
            row: copy.area.y,
            modifiers: KeyModifiers::NONE,
        }),
        Some(Command::CopyText(copied_source.to_string()))
    );

    app.complete_code_copy(copied_source, Instant::now());
    assert!(animations_active(&app, true));
    let copied = render(&mut app, 80, 30);
    let copied_buffer = copied.backend().buffer();
    assert_eq!(
        (copy.area.x..copy.area.x + copy.area.width)
            .map(|x| copied_buffer[(x, copy.area.y)].symbol())
            .collect::<String>(),
        "Copied"
    );

    app.begin_code_copy(copy.area);
    app.complete_code_copy(
        copied_source,
        Instant::now() - CODE_COPY_FEEDBACK_TTL - Duration::from_millis(1),
    );
    let restored = render(&mut app, 80, 30);
    let restored_buffer = restored.backend().buffer();
    assert_eq!(
        (copy.area.x..copy.area.x + copy.area.width)
            .map(|x| restored_buffer[(x, copy.area.y)].symbol())
            .collect::<String>(),
        " Copy "
    );
    assert!(!animations_active(&app, true));
}

#[test]
fn daily_vlist_only_renders_visible_cards_and_invalidates_changed_content() {
    let (mut app, _directory) = make_app();
    let first_date = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
    app.daily_notes = (0..40)
        .map(|day| crate::model::DailyNote {
            date: first_date + chrono::Days::new(day),
            body: format!("# DailyNote {day}\n\nA DailyNote that may be off screen."),
        })
        .collect();

    sync_daily_vlist(&mut app, 80);
    assert!(app
        .daily_vlist
        .items
        .iter()
        .all(|item| item.cache.is_none()));
    let scroll = measure_visible_daily_cards(&mut app, 0, 8, 8, false);
    assert_eq!(scroll, 0);
    let rendered = app
        .daily_vlist
        .items
        .iter()
        .filter(|item| item.cache.is_some())
        .count();
    assert!(rendered > 0 && rendered < app.daily_notes.len());
    assert!(app.daily_vlist.items[20].cache.is_none());
    let original = app.daily_vlist.items[0].cache.clone();

    measure_visible_daily_cards(&mut app, 0, 8, 8, false);
    assert_eq!(app.daily_vlist.items[0].cache, original);

    app.daily_notes[0].body.push_str("\n\nChanged");
    sync_daily_vlist(&mut app, 80);
    assert!(app.daily_vlist.items[0].cache.is_none());
    assert!(!app.daily_vlist.geometry.is_measured(0));

    sync_daily_vlist(&mut app, 72);
    assert!(app
        .daily_vlist
        .items
        .iter()
        .all(|item| item.cache.is_none()));
    assert_eq!(app.daily_vlist.width, 72);
}

#[test]
fn agent_vlist_only_renders_visible_entries_and_keeps_animation_out_of_cache() {
    let (mut app, _directory) = make_app();
    app.agent_panel.push(Arc::new(AgentPanelEntry::Tool {
        text: "Fetching Web...".to_string(),
        active: true,
        preview: None,
    }));
    app.agent_panel.extend((1..40).map(|index| {
        Arc::new(AgentPanelEntry::Prompt {
            text: format!("Prompt {index}"),
            muted: false,
        })
    }));

    sync_agent_vlist(&mut app, 40);
    assert!(app.agent_vlist.caches.iter().all(Option::is_none));
    let scroll = measure_visible_agent_entries(&mut app, 0, 6, false);
    assert_eq!(scroll, 0);
    let rendered = app
        .agent_vlist
        .caches
        .iter()
        .filter(|cache| cache.is_some())
        .count();
    assert!(rendered > 0 && rendered < app.agent_panel.len());
    assert!(app.agent_vlist.caches[20].is_none());
    let original = app.agent_vlist.caches[0].clone();
    let (visible, _, _) = visible_agent_lines(&mut app, 0, 6);
    assert_eq!(visible.len(), 6);

    app.animation_tick = 10;
    sync_agent_vlist(&mut app, 40);
    assert_eq!(app.agent_vlist.caches[0], original);

    if let AgentPanelEntry::Tool { text, .. } = Arc::make_mut(&mut app.agent_panel[0]) {
        text.push_str(" now");
    }
    sync_agent_vlist(&mut app, 40);
    assert!(app.agent_vlist.caches[0].is_none());
    assert!(!app.agent_vlist.geometry.is_measured(0));
}
