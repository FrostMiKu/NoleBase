//! A single PTY-backed terminal session embedded in the Nole UI.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

#[cfg(test)]
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, CursorShape, NamedColor};
use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

const INITIAL_ROWS: u16 = 24;
const INITIAL_COLS: u16 = 80;
const SCROLLBACK_ROWS: usize = 10_000;
const WHEEL_SCROLL_ROWS: usize = 3;

type PtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;
type TerminalCore = Term<PtyEventListener>;

#[derive(Clone)]
struct PtyEventListener {
    writer: PtyWriter,
}

impl EventListener for PtyEventListener {
    fn send_event(&self, event: Event) {
        let Event::PtyWrite(text) = event else {
            return;
        };
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = writer.write_all(text.as_bytes());
        let _ = writer.flush();
    }
}

#[derive(Clone, Copy)]
struct TerminalSize {
    rows: usize,
    cols: usize,
}

impl TerminalSize {
    fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows: rows as usize,
            cols: cols as usize,
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TerminalColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Debug, Default)]
pub struct TerminalCell {
    contents: String,
    foreground: TerminalColor,
    background: TerminalColor,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    inverse: bool,
    wide_continuation: bool,
}

impl TerminalCell {
    fn from_cell(cell: &Cell) -> Self {
        let mut contents = if cell.flags.contains(Flags::HIDDEN) {
            " ".to_string()
        } else {
            cell.c.to_string()
        };
        if let Some(zerowidth) = cell.zerowidth() {
            contents.extend(zerowidth);
        }
        Self {
            contents,
            foreground: terminal_color(cell.fg),
            background: terminal_color(cell.bg),
            bold: cell.flags.contains(Flags::BOLD),
            dim: cell.flags.contains(Flags::DIM),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            inverse: cell.flags.contains(Flags::INVERSE),
            wide_continuation: cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
        }
    }

    pub fn contents(&self) -> &str {
        &self.contents
    }

    pub fn foreground(&self) -> TerminalColor {
        self.foreground
    }

    pub fn background(&self) -> TerminalColor {
        self.background
    }

    pub fn bold(&self) -> bool {
        self.bold
    }

    pub fn dim(&self) -> bool {
        self.dim
    }

    pub fn italic(&self) -> bool {
        self.italic
    }

    pub fn underline(&self) -> bool {
        self.underline
    }

    pub fn inverse(&self) -> bool {
        self.inverse
    }

    pub fn is_wide_continuation(&self) -> bool {
        self.wide_continuation
    }
}

fn terminal_color(color: Color) -> TerminalColor {
    match color {
        Color::Spec(color) => TerminalColor::Rgb(color.r, color.g, color.b),
        Color::Indexed(index) => TerminalColor::Indexed(index),
        Color::Named(named) if named <= NamedColor::BrightWhite => {
            TerminalColor::Indexed(named as u8)
        }
        Color::Named(named) if (NamedColor::DimBlack..=NamedColor::DimWhite).contains(&named) => {
            let index = usize::from(named as u16) - usize::from(NamedColor::DimBlack as u16);
            TerminalColor::Indexed(u8::try_from(index).expect("dim ANSI color index fits in u8"))
        }
        Color::Named(_) => TerminalColor::Default,
    }
}

#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    rows: u16,
    cols: u16,
    cells: Vec<TerminalCell>,
    cursor: (u16, u16),
    hide_cursor: bool,
}

impl TerminalSnapshot {
    fn from_terminal<T: EventListener>(terminal: &Term<T>) -> Self {
        let rows = terminal.screen_lines() as u16;
        let cols = terminal.columns() as u16;
        let content = terminal.renderable_content();
        let display_offset = content.display_offset;
        let cursor_row = content.cursor.point.line.0 + display_offset as i32;
        let cursor_col = content.cursor.point.column.0;
        let mut cells = Vec::with_capacity(rows as usize * cols as usize);
        cells.extend(
            content
                .display_iter
                .map(|cell| TerminalCell::from_cell(&cell)),
        );
        Self {
            rows,
            cols,
            cells,
            cursor: (
                cursor_row.clamp(0, i32::from(rows.saturating_sub(1))) as u16,
                cursor_col.min(cols.saturating_sub(1) as usize) as u16,
            ),
            hide_cursor: content.cursor.shape == CursorShape::Hidden || display_offset > 0,
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<&TerminalCell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells
            .get(row as usize * self.cols as usize + col as usize)
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.cursor
    }

    pub fn hide_cursor(&self) -> bool {
        self.hide_cursor
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(rows: u16, cols: u16, bytes: &[u8]) -> Self {
        let size = TerminalSize::new(rows, cols);
        let mut terminal = Term::new(Config::default(), &size, VoidListener);
        let mut parser: ansi::Processor = ansi::Processor::new();
        parser.advance(&mut terminal, bytes);
        Self::from_terminal(&terminal)
    }

    #[cfg(test)]
    fn contents(&self) -> String {
        let mut contents = String::new();
        for row in 0..self.rows {
            for col in 0..self.cols {
                let cell = &self.cells[row as usize * self.cols as usize + col as usize];
                if !cell.is_wide_continuation() {
                    contents.push_str(cell.contents());
                }
            }
            contents.push('\n');
        }
        contents
    }
}

pub struct EmbeddedTerminal {
    master: Box<dyn MasterPty + Send>,
    writer: PtyWriter,
    child: Box<dyn Child + Send + Sync>,
    exited: bool,
    terminal: Arc<Mutex<TerminalCore>>,
    size: PtySize,
}

impl EmbeddedTerminal {
    pub fn spawn(root: &Path, shell: Option<&str>) -> Result<Self> {
        let mut command = match shell {
            Some(shell) => CommandBuilder::new(shell),
            None => CommandBuilder::new_default_prog(),
        };
        if let Some(shell) = shell {
            command.env("SHELL", shell);
        }
        Self::spawn_command(root, command)
    }

    fn spawn_command(root: &Path, mut command: CommandBuilder) -> Result<Self> {
        let size = PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .context("opening pseudo-terminal")?;
        command.cwd(root);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "nole");
        command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));

        let mut child = pair
            .slave
            .spawn_command(command)
            .context("starting shell")?;
        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                terminate_child(child.as_mut());
                return Err(error).context("opening pseudo-terminal reader");
            }
        };
        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                terminate_child(child.as_mut());
                return Err(error).context("opening pseudo-terminal writer");
            }
        };
        drop(pair.slave);

        let writer = Arc::new(Mutex::new(writer));
        let terminal_size = TerminalSize::new(size.rows, size.cols);
        let config = Config {
            scrolling_history: SCROLLBACK_ROWS,
            // Nole currently emits traditional xterm key sequences rather than CSI-u.
            kitty_keyboard: false,
            ..Config::default()
        };
        let event_listener = PtyEventListener {
            writer: Arc::clone(&writer),
        };
        let terminal = Arc::new(Mutex::new(Term::new(
            config,
            &terminal_size,
            event_listener,
        )));
        let reader_terminal = Arc::clone(&terminal);
        thread::spawn(move || {
            let mut buffer = [0_u8; 16 * 1024];
            let mut parser: ansi::Processor = ansi::Processor::new();
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let mut terminal = reader_terminal
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        parser.advance(&mut *terminal, &buffer[..count]);
                    }
                }
            }
        });

        Ok(Self {
            master: pair.master,
            writer,
            child,
            exited: false,
            terminal,
            size,
        })
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if self.size.rows == rows && self.size.cols == cols {
            return Ok(());
        }
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let mut terminal = self
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        terminal.resize(TerminalSize::new(rows, cols));
        if let Err(error) = self.master.resize(size) {
            terminal.resize(TerminalSize::new(self.size.rows, self.size.cols));
            return Err(error).context("resizing pseudo-terminal");
        }
        self.size = size;
        Ok(())
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        let terminal = self
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        TerminalSnapshot::from_terminal(&terminal)
    }

    pub fn write_key(&mut self, key: KeyEvent) -> Result<()> {
        let application_cursor = {
            let mut terminal = self
                .terminal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            terminal.scroll_display(Scroll::Bottom);
            terminal.mode().contains(TermMode::APP_CURSOR)
        };
        if let Some(bytes) = key_bytes(key, application_cursor) {
            self.write_bytes(&bytes)?;
        }
        Ok(())
    }

    pub fn write_paste(&mut self, text: &str) -> Result<()> {
        let bracketed = {
            let mut terminal = self
                .terminal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            terminal.scroll_display(Scroll::Bottom);
            terminal.mode().contains(TermMode::BRACKETED_PASTE)
        };
        if bracketed {
            let mut bytes = Vec::with_capacity(text.len() + 12);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.write_bytes(&bytes)
        } else {
            self.write_bytes(text.as_bytes())
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
        let status = self.child.try_wait()?;
        self.exited = status.is_some();
        Ok(status)
    }

    pub fn scroll(&mut self, delta: i32) {
        let mut terminal = self
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let rows = delta.saturating_mul(WHEEL_SCROLL_ROWS as i32);
        terminal.scroll_display(Scroll::Delta(rows.saturating_neg()));
    }

    #[cfg(test)]
    pub(crate) fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer
            .write_all(bytes)
            .context("writing to pseudo-terminal")?;
        writer.flush().context("flushing pseudo-terminal")
    }
}

impl Drop for EmbeddedTerminal {
    fn drop(&mut self) {
        if !self.exited {
            terminate_child(self.child.as_mut());
        }
    }
}

fn terminate_child(child: &mut dyn Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub fn is_terminal_toggle(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('`')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META)
}

fn key_bytes(key: KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let modifiers = key.modifiers;
    let alt = modifiers.contains(KeyModifiers::ALT);
    let control = modifiers.contains(KeyModifiers::CONTROL);

    let mut bytes = match key.code {
        KeyCode::Char(character) if control => vec![control_byte(character)?],
        KeyCode::Char(character) => character.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Null => vec![0],
        KeyCode::Up => navigation_sequence('A', modifiers, application_cursor),
        KeyCode::Down => navigation_sequence('B', modifiers, application_cursor),
        KeyCode::Right => navigation_sequence('C', modifiers, application_cursor),
        KeyCode::Left => navigation_sequence('D', modifiers, application_cursor),
        KeyCode::Home => navigation_sequence('H', modifiers, application_cursor),
        KeyCode::End => navigation_sequence('F', modifiers, application_cursor),
        KeyCode::Insert => tilde_sequence(2, modifiers),
        KeyCode::Delete => tilde_sequence(3, modifiers),
        KeyCode::PageUp => tilde_sequence(5, modifiers),
        KeyCode::PageDown => tilde_sequence(6, modifiers),
        KeyCode::F(number) => function_sequence(number, modifiers)?,
        _ => return None,
    };
    if alt && matches!(key.code, KeyCode::Char(_)) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn control_byte(character: char) -> Option<u8> {
    match character {
        'a'..='z' => Some(character as u8 - b'a' + 1),
        'A'..='Z' => Some(character as u8 - b'A' + 1),
        '@' | '`' | ' ' => Some(0),
        '[' | '{' => Some(0x1b),
        '\\' | '|' => Some(0x1c),
        ']' | '}' => Some(0x1d),
        '^' | '~' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn xterm_modifier(modifiers: KeyModifiers) -> u8 {
    1 + u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(modifiers.contains(KeyModifiers::ALT))
        + 4 * u8::from(modifiers.contains(KeyModifiers::CONTROL))
}

fn navigation_sequence(
    final_byte: char,
    modifiers: KeyModifiers,
    application_cursor: bool,
) -> Vec<u8> {
    let relevant = modifiers & (KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
    if relevant.is_empty() {
        let prefix = if application_cursor { "\x1bO" } else { "\x1b[" };
        format!("{prefix}{final_byte}").into_bytes()
    } else {
        format!("\x1b[1;{}{final_byte}", xterm_modifier(relevant)).into_bytes()
    }
}

fn tilde_sequence(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    let relevant = modifiers & (KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
    if relevant.is_empty() {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{}~", xterm_modifier(relevant)).into_bytes()
    }
}

fn function_sequence(number: u8, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    let final_byte = match number {
        1 => Some('P'),
        2 => Some('Q'),
        3 => Some('R'),
        4 => Some('S'),
        _ => None,
    };
    let relevant = modifiers & (KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
    if let Some(final_byte) = final_byte {
        return Some(if relevant.is_empty() {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[1;{}{final_byte}", xterm_modifier(relevant)).into_bytes()
        });
    }
    let number = match number {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        _ => return None,
    };
    Some(tilde_sequence(number, relevant))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn encodes_text_controls_and_navigation() {
        assert_eq!(
            key_bytes(key(KeyCode::Char('界'), KeyModifiers::NONE), false),
            Some("界".as_bytes().to_vec())
        );
        assert_eq!(
            key_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Some(vec![3])
        );
        assert_eq!(
            key_bytes(key(KeyCode::Enter, KeyModifiers::NONE), false),
            Some(vec![b'\r'])
        );
        assert_eq!(
            key_bytes(key(KeyCode::Null, KeyModifiers::NONE), false),
            Some(vec![0])
        );
        assert_eq!(
            key_bytes(key(KeyCode::Up, KeyModifiers::NONE), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_bytes(key(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_bytes(
                key(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                false
            ),
            Some(b"\x1b[1;6D".to_vec())
        );
        assert_eq!(
            key_bytes(key(KeyCode::Delete, KeyModifiers::ALT), false),
            Some(b"\x1b[3;3~".to_vec())
        );
        assert_eq!(
            key_bytes(key(KeyCode::F(12), KeyModifiers::NONE), false),
            Some(b"\x1b[24~".to_vec())
        );
    }

    #[test]
    fn alt_prefixes_character_input() {
        assert_eq!(
            key_bytes(key(KeyCode::Char('x'), KeyModifiers::ALT), false),
            Some(b"\x1bx".to_vec())
        );
        assert_eq!(
            key_bytes(
                key(
                    KeyCode::Char('c'),
                    KeyModifiers::ALT | KeyModifiers::CONTROL
                ),
                false
            ),
            Some(vec![0x1b, 3])
        );
    }

    #[test]
    fn toggle_is_only_ctrl_backtick() {
        assert!(is_terminal_toggle(key(
            KeyCode::Char('`'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_terminal_toggle(key(
            KeyCode::Char('`'),
            KeyModifiers::NONE
        )));
        assert!(!is_terminal_toggle(key(
            KeyCode::Char('`'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
        assert!(!is_terminal_toggle(key(
            KeyCode::Char('~'),
            KeyModifiers::CONTROL
        )));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_core_responses_are_written_back_to_the_pty() {
        let directory = tempfile::tempdir().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command.arg(
            r#"stty raw -echo; printf '\033[0c'; \
             response=$(dd bs=1 count=5 2>/dev/null | od -An -tx1 | tr -d ' \n'); \
             stty sane; printf 'RESPONSE:%s\n' "$response""#,
        );
        let mut terminal = EmbeddedTerminal::spawn_command(directory.path(), command).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let contents = terminal.snapshot().contents();
            if contents.contains("RESPONSE:1b5b3f3663") {
                break;
            }
            assert!(Instant::now() < deadline, "PTY output was {contents:?}");
            let _ = terminal.try_wait();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn configured_shell_is_spawned_and_exported() {
        let directory = tempfile::tempdir().unwrap();
        let mut terminal = EmbeddedTerminal::spawn(directory.path(), Some("/bin/sh")).unwrap();
        terminal
            .write_bytes(b"printf 'SHELL:%s\\n' \"$SHELL\"; exit\r")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let contents = terminal.snapshot().contents();
            if contents.contains("SHELL:/bin/sh") {
                break;
            }
            assert!(Instant::now() < deadline, "PTY output was {contents:?}");
            let _ = terminal.try_wait();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn child_process_uses_the_requested_working_directory() {
        let directory = tempfile::tempdir().unwrap();
        let mut terminal =
            EmbeddedTerminal::spawn_command(directory.path(), CommandBuilder::new("/bin/pwd"))
                .unwrap();
        let expected = directory.path().to_string_lossy();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let contents = terminal.snapshot().contents();
            if contents.contains(expected.as_ref()) {
                break;
            }
            assert!(Instant::now() < deadline, "PTY output was {contents:?}");
            let _ = terminal.try_wait();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
