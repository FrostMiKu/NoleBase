//! Dialog functionality: delete.

use crate::attachment::AttachmentId;

use super::super::*;

impl App {
    pub(crate) fn handle_delete_daily_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(date) = self.pending_daily_date.take() {
                    let note = self.daily_note_clone(date);
                    match self.storage.remove_daily(&date.to_string()) {
                        Ok(true) => {
                            if let Some(note) = note {
                                self.record_undo(UndoOp::Delete(note));
                            }
                            self.set_status("Deleted");
                            self.reload();
                            self.reload_todos();
                        }
                        Ok(false) => self.set_status("Daily note not found"),
                        Err(error) => self.set_error(format!("Error: {error}")),
                    }
                }
                self.overlay = None;
                self.dialog = None;
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_daily_date = None;
                self.overlay = None;
                self.dialog = None;
                None
            }
            _ => None,
        }
    }

    pub(crate) fn handle_delete_attachment_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                let pending = self.pending_attachment.take();
                self.overlay = None;
                self.dialog = None;
                if let Some(id) = pending {
                    self.trash_attachment(id);
                }
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_attachment = None;
                self.overlay = None;
                self.dialog = None;
                None
            }
            _ => None,
        }
    }

    /// Move an attachment to trash after confirmation. Re-checks the reference
    /// index in case notes changed while the dialog was open: a referenced
    /// attachment is refused and its locations are reported.
    fn trash_attachment(&mut self, id: AttachmentId) {
        let uri = crate::attachment::AttachmentUri::from_id(id).to_string();
        if self.attachment_refs.is_referenced(&uri) {
            let locations = self.attachment_refs.locations(&uri);
            let names = locations
                .iter()
                .map(|path| {
                    path.strip_prefix(&self.storage.root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.set_error(format!(
                "Refusing to trash attachment: still referenced by {}: {}",
                if locations.len() == 1 {
                    "1 note".to_string()
                } else {
                    format!("{} notes", locations.len())
                },
                names
            ));
            return;
        }
        let name = self
            .attachment_store
            .metadata(id)
            .ok()
            .map(|metadata| metadata.source)
            .unwrap_or_else(|| "attachment".to_string());
        match self.attachment_store.remove(id) {
            Ok(true) => {
                self.set_status(format!("Moved {name} to trash"));
                self.recompute_attachments();
            }
            Ok(false) => self.set_status("Attachment not found"),
            Err(error) => self.set_error(format!("Attachment trash error: {error}")),
        }
    }

    pub(crate) fn handle_delete_file_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                let mut deleted_skill = false;
                if let Some(path) = self.pending_file.take() {
                    let skill = self.document.as_ref().is_some_and(|document| {
                        matches!(&document.kind, DocumentKind::Skill(open) if open == &path)
                    });
                    let archived = self
                        .note_files
                        .iter()
                        .find(|file| file.path == path)
                        .is_some_and(|file| file.archived);
                    let result = if skill {
                        self.storage.delete_skill(&path)
                    } else if archived {
                        self.storage.delete_archived_file(&path)
                    } else {
                        self.storage.delete_file(&path)
                    };
                    match result {
                        Ok(()) => {
                            let kind = if skill {
                                DocumentKind::Skill(path.clone())
                            } else {
                                DocumentKind::File(path.clone())
                            };
                            self.document_render_lru.remove(&kind);
                            self.set_status(format!(
                                "Deleted {}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            ));
                            if skill {
                                self.document = None;
                                deleted_skill = true;
                            } else if self
                                .document
                                .as_ref()
                                .is_some_and(|document| document.kind == DocumentKind::File(path))
                            {
                                self.document = None;
                                self.center_view = CenterView::Daily;
                            }
                            if !skill {
                                self.reload_files();
                            }
                        }
                        Err(error) => self.set_error(format!("Error: {error}")),
                    }
                }
                self.overlay = None;
                self.dialog = None;
                if deleted_skill {
                    self.return_to_skill_browser();
                }
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_file = None;
                self.overlay = None;
                self.dialog = None;
                None
            }
            _ => None,
        }
    }
}
