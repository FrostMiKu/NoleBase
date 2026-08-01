//! Workspace terminal session: spawn, poll, IO, and overlay save/restore.

use super::*;

impl App {
    pub fn toggle_terminal(&mut self) {
        if self.overlay == Some(Overlay::Terminal) {
            self.restore_terminal_return_overlay();
            return;
        }
        if self.terminal.is_none() {
            let shell = match self.storage.terminal_shell() {
                Ok(shell) => shell,
                Err(error) => {
                    self.set_error(format!("Terminal settings error: {error}"));
                    return;
                }
            };
            match EmbeddedTerminal::spawn(&self.storage.root, shell.as_deref()) {
                Ok(terminal) => self.terminal = Some(terminal),
                Err(error) => {
                    self.set_error(format!("Terminal error: {error}"));
                    return;
                }
            }
        }
        self.terminal_return_overlay = self.overlay.take();
        self.terminal_return_dialog = self.dialog.take();
        self.overlay = Some(Overlay::Terminal);
    }

    pub fn poll_terminal(&mut self) {
        let result = self.terminal.as_mut().map(EmbeddedTerminal::try_wait);
        match result {
            Some(Ok(Some(_))) => {
                self.terminal = None;
                if self.overlay == Some(Overlay::Terminal) {
                    self.restore_terminal_return_overlay();
                }
                self.set_status("Terminal session ended");
            }
            Some(Err(error)) => self.close_terminal_with_error(error),
            _ => {}
        }
    }

    pub(crate) fn terminal_snapshot(&mut self, rows: u16, cols: u16) -> Option<TerminalSnapshot> {
        let resize = self
            .terminal
            .as_mut()
            .map(|terminal| terminal.resize(rows, cols));
        if let Some(Err(error)) = resize {
            self.close_terminal_with_error(error);
            return None;
        }
        self.terminal.as_ref().map(EmbeddedTerminal::snapshot)
    }

    pub(super) fn write_terminal_key(&mut self, key: KeyEvent) {
        let result = self
            .terminal
            .as_mut()
            .map(|terminal| terminal.write_key(key));
        if let Some(Err(error)) = result {
            self.close_terminal_with_error(error);
        }
    }

    pub(super) fn write_terminal_paste(&mut self, text: &str) {
        let result = self
            .terminal
            .as_mut()
            .map(|terminal| terminal.write_paste(text));
        if let Some(Err(error)) = result {
            self.close_terminal_with_error(error);
        }
    }

    fn close_terminal_with_error(&mut self, error: impl std::fmt::Display) {
        self.terminal = None;
        if self.overlay == Some(Overlay::Terminal) {
            self.restore_terminal_return_overlay();
        }
        self.set_error(format!("Terminal error: {error}"));
    }

    #[cfg(test)]
    pub(super) fn terminal_process_id(&self) -> Option<u32> {
        self.terminal
            .as_ref()
            .and_then(EmbeddedTerminal::process_id)
    }

    fn restore_terminal_return_overlay(&mut self) {
        self.overlay = self.terminal_return_overlay.take();
        self.dialog = self.terminal_return_dialog.take();
    }

    pub(super) fn discard_terminal_return_overlay(&mut self) {
        self.terminal_return_overlay = None;
        self.terminal_return_dialog = None;
    }
}
