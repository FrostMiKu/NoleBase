//! Crossterm backend that commits each Ratatui frame in one terminal write.

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// Buffers commands produced during a frame until [`Backend::flush`].
///
/// Crossterm's cursor methods use `execute!`, which flushes their writer. Ratatui
/// calls those methods after drawing the buffer diff, so forwarding those flushes
/// would briefly expose the drawing cursor before the final input cursor move.
#[derive(Debug)]
pub struct FrameBackend<W: Write> {
    inner: CrosstermBackend<FrameWriter<W>>,
    writer: FrameWriter<W>,
}

impl<W: Write> FrameBackend<W> {
    pub fn new(writer: W) -> Self {
        let writer = FrameWriter::new(writer);
        Self {
            inner: CrosstermBackend::new(writer.clone()),
            writer,
        }
    }
}

impl<W: Write> Write for FrameBackend<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.writer.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.commit()
    }
}

impl<W: Write> Backend for FrameBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, count: u16) -> io::Result<()> {
        self.inner.append_lines(count)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.commit()
    }
}

#[derive(Debug)]
struct FrameWriter<W: Write> {
    state: Rc<RefCell<FrameWriterState<W>>>,
}

impl<W: Write> Clone for FrameWriter<W> {
    fn clone(&self) -> Self {
        Self {
            state: Rc::clone(&self.state),
        }
    }
}

#[derive(Debug)]
struct FrameWriterState<W: Write> {
    inner: W,
    pending: Vec<u8>,
}

impl<W: Write> FrameWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            state: Rc::new(RefCell::new(FrameWriterState {
                inner,
                pending: Vec::new(),
            })),
        }
    }

    fn commit(&mut self) -> io::Result<()> {
        let mut state = self.state.borrow_mut();
        let pending = std::mem::take(&mut state.pending);
        if let Err(error) = state.inner.write_all(&pending) {
            state.pending = pending;
            return Err(error);
        }
        state.inner.flush()?;
        Ok(())
    }
}

impl<W: Write> Write for FrameWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.state.borrow_mut().pending.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Debug, Default)]
    struct RecordingWriter(Rc<RefCell<RecordingState>>);

    #[derive(Debug, Default)]
    struct RecordingState {
        writes: Vec<Vec<u8>>,
        flushes: usize,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().writes.push(buffer.to_vec());
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.borrow_mut().flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn frame_flush_commits_draw_and_cursor_commands_together() {
        let writer = RecordingWriter::default();
        let state = Rc::clone(&writer.0);
        let mut backend = FrameBackend::new(writer);
        let cell = Cell::new("A");

        backend.draw([(8, 1, &cell)].into_iter()).unwrap();
        backend.show_cursor().unwrap();
        backend.set_cursor_position(Position::new(2, 3)).unwrap();

        assert!(state.borrow().writes.is_empty());
        assert_eq!(state.borrow().flushes, 0);

        Backend::flush(&mut backend).unwrap();

        let state = state.borrow();
        assert_eq!(state.writes.len(), 1);
        assert_eq!(state.flushes, 1);
        let output = String::from_utf8_lossy(&state.writes[0]);
        let draw = output.find("\x1b[2;9HA").expect("draw command");
        let show = output.find("\x1b[?25h").expect("show cursor command");
        let cursor = output.find("\x1b[4;3H").expect("final cursor command");
        assert!(draw < show && show < cursor, "{output:?}");
    }

    #[test]
    fn direct_backend_writes_still_flush_immediately() {
        let writer = RecordingWriter::default();
        let state = Rc::clone(&writer.0);
        let mut backend = FrameBackend::new(writer);

        backend.write_all(b"external command").unwrap();
        Write::flush(&mut backend).unwrap();

        let state = state.borrow();
        assert_eq!(state.writes, [b"external command".to_vec()]);
        assert_eq!(state.flushes, 1);
    }
}
