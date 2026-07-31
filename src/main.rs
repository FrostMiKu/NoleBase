//! Entry point: terminal lifecycle, event loop, and external-editor integration.

mod agent;
mod agent_session;
mod app;
mod embedded_terminal;
mod markdown;
mod media;
mod model;
mod notification;
mod observable;
mod provider;
mod skill;
mod storage;
mod theme;
mod ui;
mod vlist;
mod workspace_index;

use std::io::{self, Stdout, Write};
use std::process::Command as ProcCommand;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::Rect;
use ratatui::Terminal;

use app::{App, Command};
use workspace_index::WorkspaceIndexer;

type Tui = Terminal<CrosstermBackend<Stdout>>;
type WatchEvents = Receiver<notify::Result<notify::Event>>;

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const EVENT_BATCH_LIMIT: usize = 16_384;
const MAX_WHEEL_DELTA_PER_FRAME: i32 = 3;

fn enter_tui() -> Result<()> {
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        // Request the kitty keyboard protocol on supporting terminals so that
        // Shift+Enter / modified keys are reported distinctly (ignored by
        // terminals that don't implement it).
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    )?;
    Ok(())
}

fn leave_tui() -> Result<()> {
    execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableMouseCapture,
        Show,
        LeaveAlternateScreen,
    )?;
    disable_raw_mode()?;
    Ok(())
}

/// RAII guard that always restores the terminal, even on panic.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = leave_tui();
    }
}

/// Suspend the TUI, open `path` in the configured editor, then resume the TUI.
/// Inheriting stdio lets the editor take over the tty.
fn run_editor(path: &std::path::Path, editor: &str, terminal: &mut Tui) -> Result<()> {
    leave_tui()?;
    // Allow a multi-word editor command (e.g. "code -w").
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let args = parts;

    let status = ProcCommand::new(program).args(args).arg(path).status();
    // Always re-enter the TUI and force a full redraw: the screen was under the
    // editor's control, so ratatui's diff buffer is otherwise stale.
    enter_tui()?;
    terminal.clear()?;
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => anyhow::bail!("editor exited with status {s}"),
        Err(e) => anyhow::bail!("failed to spawn editor {program:?}: {e}"),
    }
}

fn handle_command(cmd: Option<Command>, app: &mut App, terminal: &mut Tui) -> Result<bool> {
    match cmd {
        Some(Command::Quit) => Ok(true),
        Some(Command::Edit(path)) => {
            // run_editor re-enters the TUI itself; the outer guard in main
            // restores the terminal if anything here panics.
            match app.storage.editor_command() {
                Ok(editor) => {
                    if let Err(e) = run_editor(&path, &editor, terminal) {
                        app.set_error(format!("Editor error: {e}"));
                    }
                }
                Err(error) => app.set_error(format!("Editor settings error: {error}")),
            }
            if let Err(error) = set_mouse_capture(terminal.backend_mut(), app.mouse_captured) {
                app.mouse_captured = true;
                app.set_error(format!("Mouse support error: {error}"));
            }
            app.reload_workspace();
            Ok(false)
        }
        Some(Command::OpenLink(target)) => {
            match open::that_detached(&target) {
                Ok(()) => app.set_status(format!("Opened {target}")),
                Err(error) => app.set_error(format!("Link error: {error}")),
            }
            Ok(false)
        }
        Some(Command::OpenPath(path)) => {
            match open::that_detached(&path) {
                Ok(()) => app.set_status(format!("Opened {}", path.display())),
                Err(error) => app.set_error(format!("Open error: {error}")),
            }
            Ok(false)
        }
        Some(Command::SetMouseCapture(enabled)) => {
            if let Err(error) = set_mouse_capture(terminal.backend_mut(), enabled) {
                app.mouse_captured = !enabled;
                app.set_error(format!("Mouse support error: {error}"));
            }
            Ok(false)
        }
        None => Ok(false),
    }
}

fn set_mouse_capture(output: &mut impl Write, enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(output, EnableMouseCapture)
    } else {
        execute!(output, DisableMouseCapture)
    }
}

fn watch_workspace(path: &std::path::Path) -> Result<(RecommendedWatcher, WatchEvents)> {
    let (sender, receiver) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .context("creating Nole directory watcher")?;
    watcher
        .watch(path, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", path.display()))?;
    Ok((watcher, receiver))
}

fn process_workspace_events(events: &WatchEvents, app: &mut App) -> Vec<std::path::PathBuf> {
    let mut changed = false;
    let mut indexed_paths = Vec::new();
    let mut watcher_error = None;
    for event in events.try_iter() {
        if let Ok(event) = &event {
            app.invalidate_agent_reads(&event.paths);
        }
        match event {
            Ok(event)
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) && event.paths.iter().any(|path| {
                    path == &app.storage.settings_path
                        || (path.parent() == Some(app.storage.themes_dir.as_path())
                            && path
                                .extension()
                                .and_then(|extension| extension.to_str())
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("toml")))
                        || path
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| {
                                extension.eq_ignore_ascii_case("md")
                                    || extension.eq_ignore_ascii_case("mb")
                            })
                }) =>
            {
                indexed_paths.extend(
                    event
                        .paths
                        .iter()
                        .filter(|path| {
                            path.extension()
                                .and_then(|extension| extension.to_str())
                                .is_some_and(|extension| {
                                    extension.eq_ignore_ascii_case("md")
                                        || extension.eq_ignore_ascii_case("mb")
                                })
                        })
                        .cloned(),
                );
                changed = true;
            }
            Ok(_) => {}
            Err(error) => watcher_error = Some(error),
        }
    }
    if changed {
        app.reload_workspace();
    }
    if let Some(error) = watcher_error {
        app.set_error(format!("File watcher error: {error}"));
    }
    indexed_paths
}

fn draw_frame<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<(), B::Error> {
    terminal
        .draw(|frame| {
            if let Some(position) = ui::draw(frame, app) {
                frame.set_cursor_position(position);
            }
        })
        .map(|_| ())
}

fn run(
    terminal: &mut Tui,
    app: &mut App,
    workspace_events: &WatchEvents,
    workspace_indexer: &WorkspaceIndexer,
) -> Result<()> {
    loop {
        let indexed_paths = process_workspace_events(workspace_events, app);
        workspace_indexer.paths_changed(indexed_paths);
        if let Some(index) = workspace_indexer.try_latest_update() {
            app.apply_workspace_index(index);
        }
        app.poll_agent();
        app.poll_terminal();
        let pending_bells = app.notifications.take_bells();
        if pending_bells > 0 {
            let mut output = io::stdout();
            for _ in 0..pending_bells {
                output.write_all(b"\x07")?;
            }
            output.flush()?;
        }
        app.advance_animation();
        draw_frame(terminal, app)?;
        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        let mut events = vec![event::read()?];
        while events.len() < EVENT_BATCH_LIMIT && event::poll(Duration::ZERO)? {
            events.push(event::read()?);
        }
        let mut pending_wheel = None;
        let mut quit = false;
        for event in events {
            if let Event::Mouse(mouse) = &event {
                let mouse = *mouse;
                let delta = match mouse.kind {
                    MouseEventKind::ScrollDown => Some(1),
                    MouseEventKind::ScrollUp => Some(-1),
                    _ => None,
                };
                if let Some(delta) = delta {
                    let (_, _, accumulated) =
                        pending_wheel.get_or_insert((mouse.column, mouse.row, 0));
                    *accumulated += delta;
                    continue;
                }
                flush_wheel(&mut pending_wheel, app);
                if handle_command(app.handle_mouse(mouse), app, terminal)? {
                    quit = true;
                    break;
                }
                continue;
            }
            flush_wheel(&mut pending_wheel, app);
            match event {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        || (key.kind == KeyEventKind::Repeat
                            && app.overlay == Some(app::Overlay::Terminal)) =>
                {
                    if handle_command(app.handle_key(key), app, terminal)? {
                        quit = true;
                        break;
                    }
                }
                // Base Nole interactions ignore key repeat. The embedded
                // terminal accepts it so shell input behaves normally.
                Event::Key(_) => {}
                Event::Mouse(_) => unreachable!("mouse events handled above"),
                Event::Paste(text) => app.handle_paste(&text),
                Event::Resize(width, height) => {
                    // Kitty can report the old grid size briefly after an alternate-screen
                    // transition. The resize event carries the authoritative dimensions.
                    terminal.resize(Rect::new(0, 0, width, height))?;
                }
                Event::FocusGained => app.reload_workspace(),
                Event::FocusLost => {}
            }
        }
        flush_wheel(&mut pending_wheel, app);
        if quit {
            break;
        }
    }
    Ok(())
}

fn flush_wheel(pending: &mut Option<(u16, u16, i32)>, app: &mut App) {
    if let Some((column, row, delta)) = pending.take() {
        app.handle_wheel(
            column,
            row,
            delta.clamp(-MAX_WHEEL_DELTA_PER_FRAME, MAX_WHEEL_DELTA_PER_FRAME),
        );
    }
}

fn resolve_storage() -> Result<storage::Storage> {
    // NOLE_DIR overrides the default ~/.nole location - handy for testing or
    // keeping multiple notebooks without ever touching the real data dir.
    match std::env::var("NOLE_DIR") {
        Ok(dir) if !dir.trim().is_empty() => storage::Storage::new(dir.trim()),
        _ => storage::Storage::default_root(),
    }
}

fn main() -> Result<()> {
    let storage = resolve_storage()?;
    storage.ensure_files()?;
    let (_watcher, workspace_events) = watch_workspace(&storage.root)?;
    let workspace_indexer = WorkspaceIndexer::spawn(storage.clone());
    let mut app = App::new(storage)?;

    enter_tui()?;
    let _guard = TerminalGuard;
    app.images.set_picker(
        ratatui_image::picker::Picker::from_query_stdio()
            .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks()),
    );
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    run(
        &mut terminal,
        &mut app,
        &workspace_events,
        &workspace_indexer,
    )
    .context("event loop failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::app::{CenterView, Document, DocumentKind, DocumentReturn};
    use notify::event::ModifyKind;
    use ratatui::backend::TestBackend;

    #[test]
    fn draw_frame_tracks_chat_cursor_while_agent_animation_advances() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new(storage).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        draw_frame(&mut terminal, &mut app).unwrap();
        assert!(!terminal.backend().cursor_visible());

        app.center_view = CenterView::Chat;
        app.focus = app::Focus::Compose;
        app.agent_panel
            .push(crate::agent_session::AgentPanelEntry::Assistant {
                text: "Streaming response".to_string(),
                streaming: true,
                final_output: false,
            });
        app.ai_running = true;
        draw_frame(&mut terminal, &mut app).unwrap();

        let compose = app.layout.compose.expect("compose layout");
        let cursor = terminal.get_cursor_position().unwrap();
        assert!(terminal.backend().cursor_visible());
        assert!(cursor.x > compose.x && cursor.x < compose.right() - 1);
        assert!(cursor.y > compose.y && cursor.y < compose.bottom() - 1);

        app.advance_animation();
        assert_eq!(app.animation_tick, 1);
        draw_frame(&mut terminal, &mut app).unwrap();
        assert!(terminal.backend().cursor_visible());
        assert_eq!(terminal.get_cursor_position().unwrap(), cursor);
    }

    #[test]
    fn markdown_change_events_reload_the_visible_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let document_path = storage.data_dir.join("live.mb");
        fs::write(&document_path, "before").unwrap();

        let mut app = App::new(storage).unwrap();
        app.center_view = CenterView::Document;
        app.document = Some(Document {
            kind: DocumentKind::File(document_path.clone()),
            title: "live".into(),
            source: "before".into(),
            scroll: 3,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });

        app.storage.append_to_today("external message").unwrap();
        app.storage.append_to_today("- [ ] external task").unwrap();
        fs::write(&document_path, "after").unwrap();

        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(
                notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(document_path)
            ))
            .unwrap();
        process_workspace_events(&receiver, &mut app);

        assert_eq!(app.daily_notes.len(), 1);
        assert_eq!(app.todo_items.len(), 1);
        assert_eq!(app.document.as_ref().unwrap().source, "after");
        assert_eq!(app.document.as_ref().unwrap().scroll, 3);
    }

    #[test]
    fn non_markdown_events_do_not_reload_the_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new(storage).unwrap();
        app.storage.append_to_today("not loaded yet").unwrap();

        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(directory.path().join("editor.tmp"))))
            .unwrap();
        process_workspace_events(&receiver, &mut app);

        assert!(app.daily_notes.is_empty());
    }

    #[test]
    fn settings_and_theme_file_events_reload_the_theme() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new(storage).unwrap();
        let custom =
            crate::theme::DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#010203\"");
        let theme_path = app.storage.themes_dir.join("custom.toml");
        fs::write(&theme_path, custom).unwrap();
        app.storage.write_theme_selection("custom").unwrap();

        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(app.storage.settings_path.clone())))
            .unwrap();
        process_workspace_events(&receiver, &mut app);

        assert_eq!(app.theme.surface_panel, ratatui::style::Color::Rgb(1, 2, 3));

        let updated =
            crate::theme::DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#040506\"");
        fs::write(&theme_path, updated).unwrap();
        sender
            .send(Ok(
                notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(theme_path)
            ))
            .unwrap();
        process_workspace_events(&receiver, &mut app);

        assert_eq!(app.theme.surface_panel, ratatui::style::Color::Rgb(4, 5, 6));
    }

    #[test]
    fn queued_wheel_events_are_limited_to_one_frame_step() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new(storage).unwrap();
        app.focus = crate::app::Focus::Center;
        app.center_view = CenterView::Document;
        app.layout.center = Some(Rect::new(0, 0, 80, 20));
        app.document = Some(Document {
            kind: DocumentKind::File(directory.path().join("long.md")),
            title: "long".into(),
            source: "line\n".repeat(100),
            scroll: 10,
            target_line: None,
            return_to: DocumentReturn::Daily,
            render_cache: None,
        });

        let mut down = Some((10, 10, 200));
        flush_wheel(&mut down, &mut app);
        assert_eq!(app.document.as_ref().unwrap().scroll, 13);

        let mut up = Some((10, 10, -200));
        flush_wheel(&mut up, &mut app);
        assert_eq!(app.document.as_ref().unwrap().scroll, 10);
    }
}
