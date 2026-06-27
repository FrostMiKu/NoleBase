//! Entry point: terminal lifecycle, event loop, and `$EDITOR` integration.

mod app;
mod markdown;
mod model;
mod storage;
mod ui;

use std::io::{self, Stdout};
use std::process::Command as ProcCommand;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, Command};

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn enter_tui() -> Result<()> {
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
    )?;
    Ok(())
}

fn leave_tui() -> Result<()> {
    execute!(
        io::stdout(),
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
fn run_editor(path: &std::path::Path) -> Result<()> {
    leave_tui()?;
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    // Allow a multi-word editor command (e.g. "code -w").
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let args = parts;

    let status = ProcCommand::new(program).args(args).arg(path).status();
    let _guard = TerminalGuard; // ensure re-entry even on error
    enter_tui()?;
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => anyhow::bail!("editor exited with status {s}"),
        Err(e) => anyhow::bail!("failed to spawn editor {program:?}: {e}"),
    }
}

fn handle_command(cmd: Option<Command>, app: &mut App) -> Result<bool> {
    match cmd {
        Some(Command::Quit) => Ok(true),
        Some(Command::Edit(path)) => {
            // run_editor re-enters the TUI itself; errors propagate and the
            // outer guard restores the terminal.
            if let Err(e) = run_editor(&path) {
                app.status = format!("Editor error: {e}");
            }
            app.reload();
            Ok(false)
        }
        None => Ok(false),
    }
}

fn run(terminal: &mut Tui, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) => {
                if handle_command(app.handle_key(key), app)? {
                    break;
                }
            }
            Event::Mouse(mouse) => {
                if handle_command(app.handle_mouse(mouse), app)? {
                    break;
                }
            }
            Event::Paste(text) => {
                app.handle_paste(&text);
            }
            Event::Resize(_, _) => {}
            Event::FocusGained | Event::FocusLost => {}
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

    enter_tui()?;
    let _guard = TerminalGuard;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    run(&mut terminal, &mut app).context("event loop failed")?;
    Ok(())
}
