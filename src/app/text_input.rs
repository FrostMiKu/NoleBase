use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{delete_backward, delete_forward, insert_char, move_cursor, CursorMove};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum TextInputEdit {
    Ignored,
    CursorMoved,
    Changed,
}

impl TextInputEdit {
    pub(in crate::app) fn changed(self) -> bool {
        self == Self::Changed
    }

    pub(in crate::app) fn handled(self) -> bool {
        self != Self::Ignored
    }
}

pub(in crate::app) fn edit_single_line(
    value: &mut String,
    cursor: &mut usize,
    key: KeyEvent,
) -> TextInputEdit {
    *cursor = (*cursor).min(value.chars().count());
    match key.code {
        KeyCode::Backspace => {
            let before = value.len();
            delete_backward(value, cursor);
            if value.len() == before {
                TextInputEdit::Ignored
            } else {
                TextInputEdit::Changed
            }
        }
        KeyCode::Delete => {
            let before = value.len();
            delete_forward(value, cursor);
            if value.len() == before {
                TextInputEdit::Ignored
            } else {
                TextInputEdit::Changed
            }
        }
        KeyCode::Left => {
            let before = *cursor;
            *cursor = move_cursor(value, *cursor, CursorMove::Left);
            if *cursor == before {
                TextInputEdit::Ignored
            } else {
                TextInputEdit::CursorMoved
            }
        }
        KeyCode::Right => {
            let before = *cursor;
            *cursor = move_cursor(value, *cursor, CursorMove::Right);
            if *cursor == before {
                TextInputEdit::Ignored
            } else {
                TextInputEdit::CursorMoved
            }
        }
        KeyCode::Home => {
            let before = *cursor;
            *cursor = move_cursor(value, *cursor, CursorMove::LineStart);
            if *cursor == before {
                TextInputEdit::Ignored
            } else {
                TextInputEdit::CursorMoved
            }
        }
        KeyCode::End => {
            let before = *cursor;
            *cursor = move_cursor(value, *cursor, CursorMove::LineEnd);
            if *cursor == before {
                TextInputEdit::Ignored
            } else {
                TextInputEdit::CursorMoved
            }
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            insert_char(value, cursor, character);
            TextInputEdit::Changed
        }
        _ => TextInputEdit::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn single_line_edits_share_character_index_cursor_behavior() {
        let mut value = "文档ab".to_string();
        let mut cursor = value.chars().count();

        assert_eq!(
            edit_single_line(&mut value, &mut cursor, key(KeyCode::Left)),
            TextInputEdit::CursorMoved
        );
        assert_eq!(
            edit_single_line(&mut value, &mut cursor, key(KeyCode::Backspace)),
            TextInputEdit::Changed
        );
        edit_single_line(&mut value, &mut cursor, key(KeyCode::Char('新')));
        edit_single_line(&mut value, &mut cursor, key(KeyCode::Delete));

        assert_eq!(value, "文档新");
        assert_eq!(cursor, 3);
        edit_single_line(&mut value, &mut cursor, key(KeyCode::Home));
        assert_eq!(cursor, 0);
        edit_single_line(&mut value, &mut cursor, key(KeyCode::End));
        assert_eq!(cursor, value.chars().count());
    }
}
