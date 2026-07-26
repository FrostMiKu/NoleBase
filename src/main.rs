//! Entry point: terminal lifecycle, event loop, and `$EDITOR` integration.

mod app;
mod markdown;
mod model;
mod storage;
mod ui;

use std::io::{self, Stdout};
use std::process::Command as ProcCommand;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, Command};

type Tui = Terminal<CrosstermBackend<Stdout>>;
type WatchEvents = Receiver<notify::Result<notify::Event>>;

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

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

/// Suspend the TUI, open `path` in `$EDITOR`/`$VISUAL` (fallback `vi`),
/// then resume the TUI. Inheriting stdio lets the editor take over the tty.
fn run_editor(path: &std::path::Path, terminal: &mut Tui) -> Result<()> {
    leave_tui()?;
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
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
            if let Err(e) = run_editor(&path, terminal) {
                app.status = format!("Editor error: {e}");
            }
            app.reload_workspace();
            Ok(false)
        }
        None => Ok(false),
    }
}

fn watch_workspace(path: &std::path::Path) -> Result<(RecommendedWatcher, WatchEvents)> {
    let (sender, receiver) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })
    .context("creating note directory watcher")?;
    watcher
        .watch(path, RecursiveMode::NonRecursive)
        .with_context(|| format!("watching {}", path.display()))?;
    Ok((watcher, receiver))
}

fn process_workspace_events(events: &WatchEvents, app: &mut App) {
    let mut changed = false;
    let mut watcher_error = None;
    for event in events.try_iter() {
        match event {
            Ok(event)
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) && event.paths.iter().any(|path| {
                    path.extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                }) =>
            {
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
        app.status = format!("File watcher error: {error}");
    }
}

fn run(terminal: &mut Tui, app: &mut App, workspace_events: &WatchEvents) -> Result<()> {
    loop {
        process_workspace_events(workspace_events, app);
        terminal.draw(|f| ui::draw(f, app))?;
        if !event::poll(EVENT_POLL_INTERVAL)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if handle_command(app.handle_key(key), app, terminal)? {
                    break;
                }
            }
            // Ignore key release/repeat events (kitty protocol).
            Event::Key(_) => {}
            Event::Mouse(mouse) => {
                if handle_command(app.handle_mouse(mouse), app, terminal)? {
                    break;
                }
            }
            Event::Paste(text) => {
                app.handle_paste(&text);
            }
            Event::Resize(_, _) => {}
            Event::FocusGained => app.reload_workspace(),
            Event::FocusLost => {}
        }
    }
    Ok(())
}

fn resolve_storage() -> Result<storage::Storage> {
    // NOTE_DIR overrides the default ~/.note location — handy for testing or
    // keeping multiple notebooks without ever touching the real data dir.
    match std::env::var("NOTE_DIR") {
        Ok(dir) if !dir.trim().is_empty() => storage::Storage::new(dir.trim()),
        _ => storage::Storage::default_root(),
    }
}

fn main() -> Result<()> {
    let storage = resolve_storage()?;
    storage.ensure_files()?;
    let mut app = App::new(storage)?;
    let (_watcher, workspace_events) = watch_workspace(&app.storage.root)?;

    enter_tui()?;
    let _guard = TerminalGuard;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    run(&mut terminal, &mut app, &workspace_events).context("event loop failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use notify::event::ModifyKind;

    use super::*;
    use crate::app::{CenterView, Document, DocumentKind, DocumentReturn};

    #[test]
    fn markdown_change_events_reload_the_visible_workspace() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let document_path = directory.path().join("live.md");
        fs::write(&document_path, "before").unwrap();

        let mut app = App::new(storage).unwrap();
        app.center_view = CenterView::Document;
        app.document = Some(Document {
            kind: DocumentKind::File(document_path.clone()),
            title: "live".into(),
            source: "before".into(),
            scroll: 3,
            target_line: None,
            return_to: DocumentReturn::Chat,
        });

        app.storage.append_chat_message("external message").unwrap();
        fs::write(&app.storage.todo_path, "# TODO\n\n- [ ] external task\n").unwrap();
        fs::write(&document_path, "after").unwrap();

        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(
                notify::Event::new(EventKind::Modify(ModifyKind::Any)).add_path(document_path)
            ))
            .unwrap();
        process_workspace_events(&receiver, &mut app);

        assert_eq!(app.messages.len(), 1);
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
        app.storage.append_chat_message("not loaded yet").unwrap();

        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any))
                .add_path(directory.path().join("editor.tmp"))))
            .unwrap();
        process_workspace_events(&receiver, &mut app);

        assert!(app.messages.is_empty());
    }
}
