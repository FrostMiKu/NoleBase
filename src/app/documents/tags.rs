//! Document browsing and actions: tags.

use super::super::*;

impl App {
    pub fn open_tags(&mut self) {
        self.close_dialog();
        self.activate_workspace_view(CenterView::Tags);
    }

    /// Recompute the state the Tags view needs right now: the filtered tag
    /// picker while no stream is open, or the active tag's card stream.
    pub(in crate::app) fn recompute_tags(&mut self) {
        if self.active_tag.is_some() {
            self.reload_tag_notes();
            return;
        }
        let query = self.tag_query.trim().trim_start_matches('#').to_lowercase();
        let Some(tags) = self.workspace_index.with_index(WorkspaceIndex::tags) else {
            self.tag_results.clear();
            self.tag_index = 0;
            self.set_status("Workspace index is still building");
            return;
        };
        self.tag_results = tags
            .into_iter()
            .filter(|tag| query.is_empty() || tag.name.to_lowercase().contains(&query))
            .collect();
        self.tag_index = self.tag_index.min(self.tag_results.len().saturating_sub(1));
    }

    /// Replace the Search routing for a selected tag: stay in Tags and open
    /// every distinct managed note containing that exact tag as full-body
    /// cards, oldest-first by modified time.
    pub(in crate::app) fn open_tag_documents(&mut self, name: &str) {
        self.close_dialog();
        if self.center_view == CenterView::Document {
            self.tags_return_view = CenterView::Document;
        } else if self.center_view != CenterView::Tags {
            self.tags_return_view = CenterView::Daily;
        }
        self.active_tag = Some(name.to_string());
        self.tag_note_index = 0;
        self.tag_note_scroll = 0;
        self.reveal_selected_tag_note = true;
        self.workspace_view_index = WorkspaceView::index_of(CenterView::Tags)
            .expect("Tags is registered as a workspace view");
        self.center_view = CenterView::Tags;
        self.focus = Focus::Center;
        self.reload_tag_notes();
    }

    /// Open the full note behind the selected card, returning to the same tag
    /// stream (and selection) on close.
    pub(in crate::app) fn open_tag_note_at(&mut self, index: usize) {
        let Some(note) = self.tag_notes.get(index).cloned() else {
            return;
        };
        if let Some(date) = self.storage.daily_date_for_path(&note.path) {
            self.open_daily_document(date, DocumentReturn::Tags);
        } else {
            self.open_file_document(&note.path, DocumentReturn::Tags);
        }
    }

    pub(in crate::app) fn move_tag_selection(&mut self, delta: i32) {
        if !self.tag_results.is_empty() {
            self.tag_index = move_index(self.tag_index, delta, self.tag_results.len());
        }
    }

    pub(in crate::app) fn move_tag_note_selection(&mut self, delta: i32) {
        if !self.tag_notes.is_empty() {
            self.tag_note_index = move_index(self.tag_note_index, delta, self.tag_notes.len());
            self.reveal_selected_tag_note = true;
        }
    }

    pub(in crate::app) fn scroll_tag_notes(&mut self, delta: i32) {
        self.tag_note_scroll = if delta > 0 {
            self.tag_note_scroll.saturating_add(delta as u16)
        } else {
            self.tag_note_scroll
                .saturating_sub(delta.unsigned_abs() as u16)
        };
        self.reveal_selected_tag_note = false;
    }

    /// Rebuild `tag_notes` for the active tag from the current workspace index,
    /// preserving the selection when the stream survives an index refresh.
    fn reload_tag_notes(&mut self) {
        let Some(tag) = self.active_tag.clone() else {
            return;
        };
        let Some(documents) = self
            .workspace_index
            .with_index(|index| index.tag_documents(&tag))
        else {
            self.tag_notes.clear();
            self.tag_note_index = 0;
            self.set_status("Workspace index is still building");
            return;
        };
        let selected_path = self
            .tag_notes
            .get(self.tag_note_index)
            .map(|note| note.path.clone());
        let notes = documents
            .into_iter()
            .map(|document| self.load_tag_note(document))
            .collect::<anyhow::Result<Vec<_>>>();
        self.tag_notes = match notes {
            Ok(notes) => notes,
            Err(error) => {
                self.tag_note_index = 0;
                self.set_error(format!("Tag note error: {error}"));
                return;
            }
        };
        self.tag_note_index = selected_path
            .and_then(|path| self.tag_notes.iter().position(|note| note.path == path))
            .unwrap_or_else(|| {
                self.tag_note_index
                    .min(self.tag_notes.len().saturating_sub(1))
            });
    }

    fn load_tag_note(&self, document: TagDocument) -> anyhow::Result<TagNote> {
        let body = self.storage.read_document_file(&document.path)?;
        let title = document
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| "Document".to_string());
        Ok(TagNote {
            path: document.path,
            title,
            body,
            modified: document.modified,
        })
    }
}
