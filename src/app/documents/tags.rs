//! Document browsing and actions: tags.

use super::super::*;

impl App {
    pub fn open_tags(&mut self) {
        self.close_dialog();
        self.activate_workspace_view(CenterView::Tags);
    }

    pub(in crate::app) fn recompute_tags(&mut self) {
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

    pub(in crate::app) fn open_tag_search(&mut self, name: &str) {
        self.close_dialog();
        self.workspace_view_index = WorkspaceView::index_of(CenterView::Search)
            .expect("Search is registered as a workspace view");
        self.search_query = format!("#{name}");
        self.search_cursor = self.search_query.chars().count();
        self.search_index = 0;
        self.search_list_start = 0;
        self.center_view = CenterView::Search;
        self.focus = Focus::Center;
        self.recompute_search();
    }

    pub(in crate::app) fn move_tag_selection(&mut self, delta: i32) {
        if !self.tag_results.is_empty() {
            self.tag_index = move_index(self.tag_index, delta, self.tag_results.len());
        }
    }
}
