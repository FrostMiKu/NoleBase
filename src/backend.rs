//! Crossterm backend that positions every changed cell explicitly.

use std::io::{self, Write};

use crossterm::cursor::{position, Hide, MoveTo, Show};
use crossterm::style::{
    Attribute, Color as CrosstermColor, Colors, Print, SetAttribute, SetBackgroundColor, SetColors,
    SetForegroundColor, SetUnderlineColor,
};
use crossterm::terminal::{self, Clear};
use crossterm::{execute, queue};
use ratatui::backend::{Backend, ClearType, IntoCrossterm, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier};

/// A Crossterm backend that does not infer the real terminal cursor position.
///
/// Terminals and Unicode width tables can occasionally disagree about a
/// grapheme's width. Positioning each changed cell prevents that disagreement
/// from shifting later writes and leaving stale cells on screen.
#[derive(Debug)]
pub struct PositionedBackend<W: Write> {
    writer: W,
}

impl<W: Write> PositionedBackend<W> {
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: Write> Backend for PositionedBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let writer = &mut self.writer;
        let mut foreground = Color::Reset;
        let mut background = Color::Reset;
        let mut underline = Color::Reset;
        let mut modifier = Modifier::empty();

        for (x, y, cell) in content {
            queue!(writer, MoveTo(x, y))?;

            if cell.modifier != modifier {
                queue!(writer, SetAttribute(Attribute::Reset))?;
                queue_modifiers(writer, cell.modifier)?;
                modifier = cell.modifier;
                foreground = Color::Reset;
                background = Color::Reset;
                underline = Color::Reset;
            }
            if cell.fg != foreground || cell.bg != background {
                queue!(
                    writer,
                    SetColors(Colors::new(
                        cell.fg.into_crossterm(),
                        cell.bg.into_crossterm()
                    ))
                )?;
                foreground = cell.fg;
                background = cell.bg;
            }
            if cell.underline_color != underline {
                queue!(
                    writer,
                    SetUnderlineColor(cell.underline_color.into_crossterm())
                )?;
                underline = cell.underline_color;
            }

            queue!(writer, Print(cell.symbol()))?;
        }

        queue!(
            writer,
            SetForegroundColor(CrosstermColor::Reset),
            SetBackgroundColor(CrosstermColor::Reset),
            SetUnderlineColor(CrosstermColor::Reset),
            SetAttribute(Attribute::Reset),
        )
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        for _ in 0..n {
            queue!(self.writer, Print("\n"))?;
        }
        self.writer.flush()
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, Show)
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        position()
            .map(|(x, y)| Position { x, y })
            .map_err(io::Error::other)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let Position { x, y } = position.into();
        execute!(self.writer, MoveTo(x, y))
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_region(ClearType::All)
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        execute!(
            self.writer,
            Clear(match clear_type {
                ClearType::All => terminal::ClearType::All,
                ClearType::AfterCursor => terminal::ClearType::FromCursorDown,
                ClearType::BeforeCursor => terminal::ClearType::FromCursorUp,
                ClearType::CurrentLine => terminal::ClearType::CurrentLine,
                ClearType::UntilNewLine => terminal::ClearType::UntilNewLine,
            })
        )
    }

    fn size(&self) -> io::Result<Size> {
        let (width, height) = terminal::size()?;
        Ok(Size { width, height })
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let terminal::WindowSize {
            columns,
            rows,
            width,
            height,
        } = terminal::window_size()?;
        Ok(WindowSize {
            columns_rows: Size {
                width: columns,
                height: rows,
            },
            pixels: Size { width, height },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn queue_modifiers(writer: &mut impl Write, modifier: Modifier) -> io::Result<()> {
    const MODIFIERS: [(Modifier, Attribute); 9] = [
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::DIM, Attribute::Dim),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::SLOW_BLINK, Attribute::SlowBlink),
        (Modifier::RAPID_BLINK, Attribute::RapidBlink),
        (Modifier::REVERSED, Attribute::Reverse),
        (Modifier::HIDDEN, Attribute::Hidden),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
    ];
    for (flag, attribute) in MODIFIERS {
        if modifier.contains(flag) {
            queue!(writer, SetAttribute(attribute))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_positions_every_adjacent_cell() {
        let first = Cell::new("A");
        let second = Cell::new("B");
        let mut backend = PositionedBackend::new(Vec::new());

        backend
            .draw([(0, 0, &first), (1, 0, &second)].into_iter())
            .unwrap();

        let output = String::from_utf8(backend.writer).unwrap();
        assert!(output.contains("\x1b[1;1HA\x1b[1;2HB"), "{output:?}");
    }
}
