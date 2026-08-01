//! Document browsing and actions: document_view.

use super::super::*;

impl App {
    pub fn open_help(&mut self) {
        self.help_scroll = 0;
        self.set_overlay(Overlay::Help);
    }

    pub(in crate::app) fn scroll_document(&mut self, delta: i32) {
        if let Some(document) = self.document.as_mut() {
            document.scroll = if delta > 0 {
                document.scroll.saturating_add(delta as u16)
            } else {
                document.scroll.saturating_sub(delta.unsigned_abs() as u16)
            };
        }
    }

    pub(in crate::app) fn open_file_document(&mut self, path: &Path, return_to: DocumentReturn) {
        match self.storage.read_document_file(path) {
            Ok(source) => {
                self.sync_file_tree_to_note(path);
                let title = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Document".to_string());
                self.show_document(
                    DocumentKind::File(path.to_path_buf()),
                    title,
                    source,
                    return_to,
                );
                self.center_view = CenterView::Document;
                self.focus = Focus::Center;
            }
            Err(error) => self.set_error(format!("Error: {error}")),
        }
    }

    pub(in crate::app) fn sync_file_tree_to_note(&mut self, path: &Path) {
        let Some(index) = self.note_files.iter().position(|file| file.path == path) else {
            return;
        };
        if self.note_files[index].archived {
            self.archives_expanded = true;
        } else {
            self.notes_expanded = true;
        }
        self.file_index = index;
        self.selected_file = Some(path.to_path_buf());
        self.ensure_visible_file_selection();
    }

    pub(in crate::app) fn open_daily_document(
        &mut self,
        date: NaiveDate,
        return_to: DocumentReturn,
    ) {
        let Some(note) = self.daily_note_clone(date) else {
            return;
        };
        self.show_document(
            DocumentKind::Daily(note.date),
            format!("Daily {}", note.date),
            note.body,
            return_to,
        );
        self.center_view = CenterView::Document;
        self.focus = Focus::Center;
    }

    pub(in crate::app) fn show_document(
        &mut self,
        kind: DocumentKind,
        title: String,
        source: String,
        return_to: DocumentReturn,
    ) {
        self.stash_current_document();
        let render_cache = self.document_render_lru.take(&kind, &source);
        self.document = Some(Document {
            kind,
            title,
            source,
            scroll: 0,
            target_line: None,
            return_to,
            render_cache,
        });
    }

    pub(in crate::app) fn stash_current_document(&mut self) {
        let Some(mut document) = self.document.take() else {
            return;
        };
        if let Some(render) = document.render_cache.take() {
            self.document_render_lru
                .insert(document.kind, document.source, render);
        }
    }

    pub(in crate::app) fn close_document(&mut self) {
        let Some(document) = self.document.as_ref() else {
            self.center_view = CenterView::Daily;
            self.focus = Focus::Center;
            return;
        };
        let return_to = document.return_to;
        self.stash_current_document();
        match return_to {
            DocumentReturn::Search => {
                self.center_view = CenterView::Search;
                self.focus = Focus::Center;
            }
            DocumentReturn::Daily => {
                self.center_view = CenterView::Daily;
                self.focus = Focus::Center;
            }
            DocumentReturn::Skills => self.return_to_skill_browser(),
        }
    }

    pub(in crate::app) fn daily_edit_command(&self, date: NaiveDate) -> Option<Command> {
        self.storage
            .daily_file_path(&date.to_string())
            .ok()
            .filter(|path| path.is_file())
            .map(Command::Edit)
    }
}
