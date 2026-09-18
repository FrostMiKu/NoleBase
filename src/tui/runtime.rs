//! Terminal lifecycle, event loop, and external-editor integration.

use std::io::{self, Stdout, Write};
use std::process::Command as ProcCommand;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, MouseEventKind,
};
#[cfg(not(windows))]
use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::Backend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use crate::core::agent;
use crate::core::storage;
use crate::core::workspace_watcher::WorkspaceEvent;
use crate::tui::app::{self, App, Command};
use crate::tui::backend::FrameBackend;
use crate::tui::clipboard;
use crate::tui::ui;

type Tui = Terminal<FrameBackend<Stdout>>;

const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(100);
const EVENT_BATCH_LIMIT: usize = 16_384;
const MAX_WHEEL_DELTA_PER_FRAME: i32 = 3;

fn animation_tick(epoch: Instant, now: Instant) -> u64 {
    let elapsed = now.saturating_duration_since(epoch);
    let tick = elapsed.as_nanos() / ANIMATION_FRAME_INTERVAL.as_nanos();
    u64::try_from(tick).unwrap_or(u64::MAX)
}

#[cfg(test)]
fn until_next_animation_frame(epoch: Instant, now: Instant) -> Duration {
    let elapsed = now.saturating_duration_since(epoch);
    let remainder = elapsed.as_nanos() % ANIMATION_FRAME_INTERVAL.as_nanos();
    Duration::from_nanos((ANIMATION_FRAME_INTERVAL.as_nanos() - remainder) as u64)
}

fn enter_tui() -> Result<()> {
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
    )?;
    // Crossterm's Windows event backend always uses the legacy Windows API,
    // where kitty keyboard progressive enhancement is unavailable. Windows
    // still receives normal key events; only modified-key disambiguation is
    // unavailable there.
    #[cfg(not(windows))]
    execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
    )?;
    Ok(())
}

fn leave_tui() -> Result<()> {
    #[cfg(not(windows))]
    execute!(io::stdout(), PopKeyboardEnhancementFlags)?;
    execute!(
        io::stdout(),
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

fn defer_quit_for_export(app: &mut App) -> bool {
    if !app.export_in_progress {
        return false;
    }
    app.set_status("Wait for the export to finish before quitting");
    true
}

fn handle_command(cmd: Option<Command>, app: &mut App, terminal: &mut Tui) -> Result<bool> {
    match cmd {
        Some(Command::Quit) if defer_quit_for_export(app) => Ok(false),
        Some(Command::Quit) => Ok(true),
        Some(Command::Edit(path)) => {
            // run_editor re-enters the TUI itself; the outer guard in main
            // restores the terminal if anything here panics.
            match app.storage().editor_command() {
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
        Some(Command::CopyText(text)) => {
            match clipboard::write_text(&text) {
                Ok(()) => {
                    app.complete_code_copy(&text, Instant::now());
                    app.set_status("Copied to clipboard");
                }
                Err(error) => {
                    app.cancel_code_copy();
                    app.set_error(format!("Clipboard error: {error:#}"));
                }
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

/// Apply domain workspace events to the app. Returns whether anything the
/// UI shows changed. Attachment events refresh the browser: mutable metadata
/// (e.g. display_name) can be edited externally. They leave the workspace
/// UI and note/attachment reference indexes unchanged; those indexes track
/// managed Markdown files only.
fn apply_workspace_events(app: &mut App, events: Vec<WorkspaceEvent>) -> bool {
    let mut ui_changed = false;
    for event in events {
        match event {
            WorkspaceEvent::ContentChanged { .. } => {
                app.reload_workspace();
                ui_changed = true;
            }
            WorkspaceEvent::AttachmentsChanged { paths } => {
                app.attachment_paths_changed(&paths);
                ui_changed = true;
            }
            WorkspaceEvent::AgentReadsDirty { paths } => app.mark_agent_reads_dirty(&paths),
            WorkspaceEvent::WatcherFailed(error) => {
                app.set_error(format!("File watcher error: {error}"));
                ui_changed = true;
            }
        }
    }
    ui_changed
}

fn event_wait_duration(app: &App, focused: bool) -> Duration {
    if ui::animations_active(app, focused) {
        ANIMATION_FRAME_INTERVAL
    } else {
        // Static screens remain quiet between redraws. Keep a bounded wait so
        // external file/indexer updates are noticed while crossterm handles
        // on both terminal input and the watcher channel simultaneously.
        Duration::from_secs(1)
    }
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

fn run(terminal: &mut Tui, app: &mut App, nole: &mut crate::core::nole::Nole) -> Result<()> {
    let animation_epoch = Instant::now();
    let mut focused = true;
    let mut needs_draw = true;
    loop {
        needs_draw |= apply_workspace_events(app, nole.poll_workspace());
        if app.workspace.poll_index_update() {
            app.on_index_update();
            needs_draw = true;
        }

        let was_ai_running = app.ai_running;
        let was_overlay = app.overlay;
        app.poll_agent();
        app.poll_terminal();
        app.poll_export();
        needs_draw |= was_ai_running != app.ai_running || was_overlay != app.overlay;
        needs_draw |= app.agent.terminal().poll_monitor_change();
        needs_draw |= app.agent.jobs().poll_change();
        let pending_bells = app.notifications.take_bells();
        if pending_bells > 0 {
            let mut output = io::stdout();
            for _ in 0..pending_bells {
                output.write_all(b"\x07")?;
            }
            output.flush()?;
        }

        let animation_active = ui::animations_active(app, focused);
        if animation_active || needs_draw {
            app.animation_tick = animation_tick(animation_epoch, Instant::now());
            if focused {
                draw_frame(terminal, app)?;
                needs_draw = false;
            }
        }

        if !event::poll(event_wait_duration(app, focused))? {
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
                needs_draw |= flush_wheel(&mut pending_wheel, app);
                if handle_command(app.handle_mouse(mouse), app, terminal)? {
                    quit = true;
                    break;
                }
                needs_draw = true;
                continue;
            }
            needs_draw |= flush_wheel(&mut pending_wheel, app);
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
                    needs_draw = true;
                }
                Event::Key(_) => {}
                Event::Mouse(_) => unreachable!("mouse events handled above"),
                Event::Paste(text) => {
                    app.handle_paste(&text);
                    needs_draw = true;
                }
                Event::Resize(width, height) => {
                    terminal.resize(Rect::new(0, 0, width, height))?;
                    needs_draw = true;
                }
                Event::FocusGained => {
                    focused = true;
                    needs_draw = true;
                }
                Event::FocusLost => focused = false,
            }
        }
        needs_draw |= flush_wheel(&mut pending_wheel, app);
        if quit {
            break;
        }
    }
    Ok(())
}

fn flush_wheel(pending: &mut Option<(u16, u16, i32)>, app: &mut App) -> bool {
    let Some((column, row, delta)) = pending.take() else {
        return false;
    };
    app.handle_wheel(
        column,
        row,
        delta.clamp(-MAX_WHEEL_DELTA_PER_FRAME, MAX_WHEEL_DELTA_PER_FRAME),
    );
    true
}

fn resolve_storage() -> Result<storage::Storage> {
    // NOLE_DIR overrides the default ~/.nole location - handy for testing or
    // keeping multiple notebooks while preserving the real data directory.
    match std::env::var("NOLE_DIR") {
        Ok(dir) if !dir.trim().is_empty() => storage::Storage::new(dir.trim()),
        _ => storage::Storage::default_root(),
    }
}

/// True when the first CLI argument is a version request. `nole --version`
/// prints the version before workspace or terminal setup, keeping this path
/// side-effect free.
fn wants_version<I>(mut args: I) -> bool
where
    I: Iterator<Item = String>,
{
    args.next(); // program name
    matches!(args.next().as_deref(), Some("--version" | "-V"))
}

fn should_query_image_capabilities(term_program: Option<&str>) -> bool {
    term_program != Some("nole")
}

fn image_picker() -> ratatui_image::picker::Picker {
    // Nole's embedded terminal identifies itself through TERM_PROGRAM and does
    // not implement any of the pixel-image protocols probed by ratatui-image.
    // Avoid querying stdin there: a PTY implementation that does not answer the
    // terminating status report would leave the query thread blocked on stdin.
    let term_program = std::env::var("TERM_PROGRAM").ok();
    if !should_query_image_capabilities(term_program.as_deref()) {
        return ratatui_image::picker::Picker::halfblocks();
    }

    ratatui_image::picker::Picker::from_query_stdio()
        .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks())
}

/// Binary entry: shell helper mode, version flag, then the TUI event loop.
pub fn run_app() -> Result<()> {
    if let Some(mode) = agent::shell_helper_mode(std::env::args()) {
        let code = agent::run_shell_helper(mode).unwrap_or_else(|error| {
            eprintln!("Nole shell helper error: {error:#}");
            1
        });
        std::process::exit(i32::from(code));
    }
    if wants_version(std::env::args()) {
        println!("nole {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let storage = resolve_storage()?;
    storage.ensure_files()?;
    let mut nole = crate::core::nole::Nole::open(storage)?;
    let mut app = App::new(nole.workspace().clone(), nole.agent().clone())?;

    enter_tui()?;
    let _guard = TerminalGuard;
    app.images.set_picker(image_picker());
    let backend = FrameBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    run(&mut terminal, &mut app, &mut nole).context("event loop failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::sync::Arc;

    use super::*;

    fn inject_watcher(
        storage: &storage::Storage,
    ) -> (
        WorkspaceWatcher,
        mpsc::Sender<notify::Result<notify::Event>>,
    ) {
        let (sender, receiver) = mpsc::channel();
        let watcher = WorkspaceWatcher::from_parts(
            crate::core::workspace_watcher::WatchContext::from_storage(storage),
            receiver,
        );
        (watcher, sender)
    }

    fn pump_watcher(watcher: &mut WorkspaceWatcher, app: &mut App) -> bool {
        apply_workspace_events(app, watcher.poll(&app.workspace))
    }

    #[test]
    fn embedded_nole_terminal_skips_image_capability_queries() {
        assert!(!should_query_image_capabilities(Some("nole")));
        assert!(should_query_image_capabilities(Some("iTerm.app")));
        assert!(should_query_image_capabilities(None));
    }
    use crate::core::workspace_watcher::WorkspaceWatcher;
    use crate::tui::app::{CenterView, Document, DocumentKind, DocumentReturn};
    use notify::event::{EventKind, ModifyKind};
    use ratatui::backend::TestBackend;

    #[test]
    fn version_flag_is_detected() {
        let args = |flags: &[&str]| {
            std::iter::once("nole")
                .chain(flags.iter().copied())
                .map(String::from)
                .collect::<Vec<_>>()
        };
        assert!(wants_version(args(&["--version"]).into_iter()));
        assert!(wants_version(args(&["-V"]).into_iter()));
        assert!(!wants_version(args(&[]).into_iter()));
        assert!(!wants_version(args(&["--verbose"]).into_iter()));
        assert!(!wants_version(args(&["-vv"]).into_iter()));
    }
    #[test]
    fn quit_is_deferred_while_export_is_running() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new_for_tests(storage).unwrap();
        assert!(!defer_quit_for_export(&mut app));
        app.export_in_progress = true;
        assert!(defer_quit_for_export(&mut app));
        assert_eq!(app.status, "Wait for the export to finish before quitting");
    }

    #[test]
    fn animation_phase_uses_elapsed_time_not_event_frequency() {
        let epoch = Instant::now();

        for _ in 0..10_000 {
            assert_eq!(animation_tick(epoch, epoch + Duration::from_millis(99)), 0);
        }
        for _ in 0..10_000 {
            assert_eq!(animation_tick(epoch, epoch + Duration::from_millis(100)), 1);
        }
        assert_eq!(animation_tick(epoch, epoch + Duration::from_millis(350)), 3);
        assert_eq!(animation_tick(epoch, epoch + Duration::from_millis(400)), 4);
        assert_eq!(
            until_next_animation_frame(epoch, epoch + Duration::from_millis(450)),
            Duration::from_millis(50)
        );
        assert_eq!(
            until_next_animation_frame(epoch, epoch + Duration::from_millis(500)),
            ANIMATION_FRAME_INTERVAL
        );
    }

    #[test]
    fn visible_gradient_borders_keep_animation_frames_scheduled() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new_for_tests(storage).unwrap();

        app.center_view = CenterView::Chat;
        app.focus = app::Focus::Center;
        assert!(!ui::animations_active(&app, true));

        app.center_view = CenterView::Daily;
        assert!(ui::animations_active(&app, true));
        assert!(!ui::animations_active(&app, false));

        app.center_view = CenterView::Chat;
        app.focus = app::Focus::Compose;
        assert!(ui::animations_active(&app, true));
    }

    #[test]
    fn draw_frame_tracks_chat_cursor_while_agent_animation_advances() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new_for_tests(storage).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        draw_frame(&mut terminal, &mut app).unwrap();
        assert!(!terminal.backend().cursor_visible());

        app.center_view = CenterView::Chat;
        app.focus = app::Focus::Compose;
        app.agent_panel.push(Arc::new(
            crate::core::agent_session::AgentPanelEntry::Assistant {
                text: "Streaming response".to_string(),
                streaming: true,
                final_output: false,
            },
        ));
        app.ai_running = true;
        draw_frame(&mut terminal, &mut app).unwrap();

        let compose = app.layout.compose.expect("compose layout");
        let cursor = terminal.get_cursor_position().unwrap();
        assert!(terminal.backend().cursor_visible());
        assert!(cursor.x > compose.x && cursor.x < compose.right() - 1);
        assert!(cursor.y > compose.y && cursor.y < compose.bottom() - 1);

        let Some(entry) = app.agent_panel.last_mut() else {
            panic!("streaming assistant entry");
        };
        if let crate::core::agent_session::AgentPanelEntry::Assistant { text, .. } =
            Arc::make_mut(entry)
        {
            text.push_str(" with another streamed chunk");
        }
        app.animation_tick = 1;
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

        let (mut watcher, sender) = inject_watcher(&storage);
        let mut app = App::new_for_tests(storage).unwrap();
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

        app.storage().append_to_today("external message").unwrap();
        app.storage()
            .append_to_today("- [ ] external task")
            .unwrap();
        fs::write(&document_path, "after").unwrap();

        sender
            .send(Ok(
                notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(document_path)
            ))
            .unwrap();
        pump_watcher(&mut watcher, &mut app);

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
        let (mut watcher, sender) = inject_watcher(&storage);
        let mut app = App::new_for_tests(storage).unwrap();
        app.storage().append_to_today("not loaded yet").unwrap();

        sender
            .send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(directory.path().join("editor.tmp"))))
            .unwrap();
        pump_watcher(&mut watcher, &mut app);

        assert!(app.daily_notes.is_empty());
    }

    #[test]
    fn workspace_events_do_not_reload_the_ui_or_feed_indexes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let (mut watcher, sender) = inject_watcher(&storage);
        let mut app = App::new_for_tests(storage).unwrap();
        app.storage().append_to_today("loaded").unwrap();
        app.reload();
        assert_eq!(app.daily_notes.len(), 1);

        let workspace_dir = app.storage().workspace_dir.clone();
        let session = workspace_dir.join("main").join("session.json");
        fs::create_dir_all(session.parent().unwrap()).unwrap();
        fs::write(&session, "{}").unwrap();
        let session_note = workspace_dir.join("main").join("scratch.md");
        fs::write(&session_note, "# scratch").unwrap();

        for path in [session, session_note] {
            sender
                .send(Ok(
                    notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(path)
                ))
                .unwrap();
        }
        let events = watcher.poll(&app.workspace);
        assert!(
            events.is_empty(),
            "workspace events must not reach the note/attachment indexes"
        );
        assert_eq!(
            app.daily_notes.len(),
            1,
            "workspace events must not reload the workspace UI"
        );
    }

    #[test]
    fn settings_and_theme_file_events_reload_the_theme() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let (mut watcher, sender) = inject_watcher(&storage);
        let mut app = App::new_for_tests(storage).unwrap();
        let custom = crate::core::storage::DEFAULT_THEME_TOML
            .replace("panel = \"#181825\"", "panel = \"#010203\"");
        let theme_path = app.storage().themes_dir.join("custom.toml");
        fs::write(&theme_path, custom).unwrap();
        app.storage().write_theme_selection("custom").unwrap();

        sender
            .send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(app.storage().settings_path.clone())))
            .unwrap();
        pump_watcher(&mut watcher, &mut app);

        assert_eq!(app.theme.surface_panel, ratatui::style::Color::Rgb(1, 2, 3));

        let updated = crate::core::storage::DEFAULT_THEME_TOML
            .replace("panel = \"#181825\"", "panel = \"#040506\"");
        fs::write(&theme_path, updated).unwrap();
        sender
            .send(Ok(
                notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(theme_path)
            ))
            .unwrap();
        pump_watcher(&mut watcher, &mut app);

        assert_eq!(app.theme.surface_panel, ratatui::style::Color::Rgb(4, 5, 6));
    }

    #[test]
    fn queued_wheel_events_are_limited_to_one_frame_step() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let mut app = App::new_for_tests(storage).unwrap();
        app.focus = crate::tui::app::Focus::Center;
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
