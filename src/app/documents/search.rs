//! Document browsing and actions: search.

use super::super::*;

impl App {
    pub fn open_search(&mut self) {
        self.activate_workspace_view(CenterView::Search);
    }

    pub(in crate::app) fn open_document_search(&mut self) {
        self.search_query.clear();
        self.search_cursor = 0;
        self.search_results.clear();
        self.search_index = 0;
        self.search_list_start = 0;
        self.center_view = CenterView::DocumentSearch;
        self.focus = Focus::Center;
    }

    pub(in crate::app) fn recompute_search(&mut self) {
        let query = self.search_query.trim().to_lowercase();
        let mut results = Vec::new();
        if !query.is_empty() && self.center_view == CenterView::DocumentSearch {
            if let Some(document) = &self.document {
                results.extend(
                    document
                        .source
                        .lines()
                        .enumerate()
                        .filter(|(_, line)| line.to_lowercase().contains(&query))
                        .map(|(index, line)| SearchHit::DocumentLine {
                            line_no: index + 1,
                            text: line.trim().to_string(),
                        }),
                );
            }
        } else if !query.is_empty() {
            if let Some(indexed) = self
                .workspace_index
                .with_index(|index| index.search(&query))
            {
                results = indexed;
            } else {
                self.set_status("Workspace index is still building");
            }
        }
        self.search_results = results;
        self.search_index = self
            .search_index
            .min(self.search_results.len().saturating_sub(1));
    }

    pub(in crate::app) fn jump_to_search_result(&mut self, index: usize) {
        let Some(hit) = self.search_results.get(index).cloned() else {
            return;
        };
        if self.center_view == CenterView::DocumentSearch {
            if let SearchHit::DocumentLine { line_no, .. } = hit {
                if let Some(document) = self.document.as_mut() {
                    document.target_line = Some(line_no);
                    self.center_view = CenterView::Document;
                    self.focus = Focus::Center;
                    self.set_status(format!("Found on line {line_no}"));
                }
            }
            return;
        }
        match hit {
            SearchHit::FileLine { path, line_no, .. } => {
                if let Some(date) = self.storage.daily_date_for_path(&path) {
                    self.open_daily_document(date, DocumentReturn::Search);
                } else {
                    self.open_file_document(&path, DocumentReturn::Search);
                }
                if let Some(document) = self.document.as_mut() {
                    document.target_line = Some(line_no);
                }
            }
            SearchHit::DocumentLine { .. } => {}
        }
    }

    pub(in crate::app) fn move_search_selection(&mut self, delta: i32) {
        if !self.search_results.is_empty() {
            self.search_index = move_index(self.search_index, delta, self.search_results.len());
        }
    }

    pub fn selected_date(&self) -> Option<NaiveDate> {
        self.daily_notes.get(self.selected).map(|note| note.date)
    }
}
