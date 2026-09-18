//! Crossterm key-event encoding for the embedded terminal.
//!
//! The PTY emulator lives in [`crate::core::embedded_terminal`]; translating
//! terminal-UI key events into terminal byte sequences is input glue that
//! belongs to this presentation layer.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::core::embedded_terminal::EmbeddedTerminal;

/// Write `key` to the terminal as its escape-sequence encoding.
pub fn write_key(terminal: &mut EmbeddedTerminal, key: KeyEvent) -> Result<()> {
    let application_cursor = terminal.application_cursor_mode();
    if let Some(bytes) = key_bytes(key, application_cursor) {
        terminal.write_raw(&bytes)?;
    }
    Ok(())
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
}
