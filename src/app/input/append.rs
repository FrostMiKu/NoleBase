//! Keyboard and mouse input: append.

use super::super::*;

impl App {
    pub(in crate::app) fn send_message(&mut self) {
        let original_input = self.input.clone();
        let body = self.input.trim().to_string();
        if body.is_empty() {
            return;
        }
        let document_kind = self
            .document
            .as_ref()
            .filter(|_| self.center_view == CenterView::Document)
            .map(|document| document.kind.clone());
        let result = match document_kind {
            Some(DocumentKind::File(path)) => {
                self.append_to_open_note(&path, &body, &original_input)
            }
            Some(DocumentKind::Daily(date)) => {
                self.append_to_open_daily(&date.to_string(), &body, &original_input)
            }
            Some(DocumentKind::Skill(path)) => {
                self.append_to_open_skill(&path, &body, &original_input)
            }
            None => self.append_to_today(&body, &original_input),
        };
        if let Err(error) = result {
            self.set_error(format!("Error: {error}"));
        }
    }

    pub(in crate::app) fn append_to_open_note(
        &mut self,
        path: &Path,
        body: &str,
        original_input: &str,
    ) -> anyhow::Result<()> {
        let receipt = self.storage.append_document_tracked(path, body)?;
        self.record_undo(UndoOp::Append {
            receipt,
            input: original_input.to_string(),
        });
        let source = self.storage.read_document_file(path)?;
        if let Some(document) = self.document.as_mut() {
            document.replace_source(source);
            document.scroll = u16::MAX;
            document.target_line = None;
        }
        self.clear_compose_input();
        self.reload_files();
        self.status.clear();
        Ok(())
    }

    pub(in crate::app) fn append_to_open_skill(
        &mut self,
        path: &Path,
        body: &str,
        original_input: &str,
    ) -> anyhow::Result<()> {
        let receipt = self.storage.append_document_tracked(path, body)?;
        self.record_undo(UndoOp::Append {
            receipt,
            input: original_input.to_string(),
        });
        let skill = self.storage.read_skill(path)?;
        if let Some(document) = self.document.as_mut() {
            document.replace_source(skill.body);
            document.scroll = u16::MAX;
            document.target_line = None;
        }
        self.clear_compose_input();
        self.set_status("Appended to skill");
        Ok(())
    }

    pub(in crate::app) fn append_to_open_daily(
        &mut self,
        date: &str,
        body: &str,
        original_input: &str,
    ) -> anyhow::Result<()> {
        let (note, receipt) = self.storage.append_daily_tracked(date, body)?;
        self.record_undo(UndoOp::Append {
            receipt,
            input: original_input.to_string(),
        });
        if let Some(document) = self.document.as_mut() {
            document.replace_source(note.body);
        }
        self.clear_compose_input();
        self.reload();
        self.reload_todos();
        self.notifications
            .notify(format!("Appended to Daily {date}"));
        self.set_status("Appended without leaving the document");
        Ok(())
    }

    pub(in crate::app) fn append_to_today(
        &mut self,
        body: &str,
        original_input: &str,
    ) -> anyhow::Result<()> {
        let (_, receipt) = self.storage.append_to_today_tracked(body)?;
        self.record_undo(UndoOp::Append {
            receipt,
            input: original_input.to_string(),
        });
        self.clear_compose_input();
        self.reload();
        self.reload_todos();
        self.selected = self.daily_notes.len().saturating_sub(1);
        self.scroll = u16::MAX;
        self.reveal_selected_daily = true;
        self.set_status("Saved");
        Ok(())
    }

    pub(in crate::app) fn daily_note_clone(&self, date: NaiveDate) -> Option<DailyNote> {
        self.daily_notes
            .iter()
            .find(|note| note.date == date)
            .cloned()
    }

    pub(in crate::app) fn record_undo(&mut self, operation: UndoOp) {
        const CAPACITY: usize = 50;
        if self.undo_stack.len() == CAPACITY {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(operation);
    }

    pub(in crate::app) fn recall_last_append(&mut self) {
        let Some(operation) = self.undo_stack.pop() else {
            self.set_status("Nothing to recall");
            return;
        };
        let UndoOp::Append { receipt, input } = operation else {
            self.undo_stack.push(operation);
            self.set_status("Nothing to recall");
            return;
        };

        match self.storage.undo_append(&receipt) {
            Ok(()) => {
                self.restore_recalled_input(input);
                self.reload_workspace();
                self.selected = self.daily_notes.len().saturating_sub(1);
                self.scroll = u16::MAX;
                self.set_status("Recalled last append");
            }
            Err(error) => {
                self.undo_stack.push(UndoOp::Append { receipt, input });
                self.set_error(format!("Recall error: {error}"));
            }
        }
    }

    pub(in crate::app) fn restore_recalled_input(&mut self, recalled: String) {
        if self.input.is_empty() {
            self.input = recalled;
        } else {
            let current = std::mem::take(&mut self.input);
            self.input = recalled;
            if !self.input.ends_with('\n') && !current.starts_with('\n') {
                self.input.push('\n');
            }
            self.input.push_str(&current);
        }
        self.input_cursor = self.input.chars().count();
        self.refresh_wiki_completion();
    }

    pub(in crate::app) fn undo(&mut self) {
        let Some(operation) = self.undo_stack.pop() else {
            self.set_status("Nothing to undo");
            return;
        };
        let status = match operation {
            UndoOp::Append { receipt, input } => match self.storage.undo_append(&receipt) {
                Ok(()) => {
                    self.restore_recalled_input(input);
                    "Recalled last append".to_string()
                }
                Err(error) => format!("Undo error: {error}"),
            },
            UndoOp::Delete(note) => match self.storage.restore_daily(&note) {
                Ok(()) => "Undid delete".to_string(),
                Err(error) => format!("Undo error: {error}"),
            },
            UndoOp::Move {
                daily_note,
                target,
                appended,
            } => match self.storage.restore_daily(&daily_note) {
                Ok(()) => {
                    let name = target
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    match self.storage.remove_first_occurrence(&target, &appended) {
                        Ok(true) => format!("Undid move to {name}"),
                        Ok(false) => format!("Undid move; cleanup skipped for {name}"),
                        Err(error) => {
                            format!("Undo error: restored the daily note; cleaning {name}: {error}")
                        }
                    }
                }
                Err(error) => format!("Undo error: {error}"),
            },
        };
        if status.starts_with("Undo error:") {
            self.set_error(status);
        } else {
            self.set_status(status);
        }
        self.reload_workspace();
        self.selected = self.daily_notes.len().saturating_sub(1);
        self.scroll = u16::MAX;
    }
}
