//! Document browsing and actions: workspace_views.

use super::super::*;

impl App {
    pub fn open_workspace_views(&mut self) {
        if let Some(index) = WorkspaceView::index_of(self.center_view) {
            self.workspace_view_index = index;
        }
        self.focus = Focus::Views;
    }

    pub(in crate::app) fn move_workspace_view_selection(&mut self, delta: i32) {
        self.workspace_view_index =
            move_index(self.workspace_view_index, delta, WorkspaceView::ALL.len());
    }

    pub(in crate::app) fn activate_workspace_view(&mut self, center_view: CenterView) {
        let Some(index) = WorkspaceView::index_of(center_view) else {
            return;
        };
        let previous_view = self.center_view;
        self.workspace_view_index = index;
        self.center_view = center_view;
        self.focus = Focus::Center;
        match center_view {
            CenterView::Chat => self.agent_follow_tail = true,
            CenterView::Todo => {
                self.reload_todos();
                self.todo_query.clear();
                self.todo_cursor = 0;
                self.ensure_visible_todo_selection();
                self.todo_list_start = 0;
            }
            CenterView::Search => {
                self.search_query.clear();
                self.search_cursor = 0;
                self.search_results.clear();
                self.search_index = 0;
                self.search_list_start = 0;
            }
            CenterView::Tags => {
                self.tags_return_view = if previous_view == CenterView::Document {
                    CenterView::Document
                } else {
                    CenterView::Daily
                };
                self.active_tag = None;
                self.tag_notes.clear();
                self.tag_note_index = 0;
                self.tag_note_scroll = 0;
                self.reveal_selected_tag_note = false;
                self.tag_note_vlist = TagNoteVirtualList::default();
                self.tag_query.clear();
                self.tag_cursor = 0;
                self.tag_index = 0;
                self.tag_list_start = 0;
                self.recompute_tags();
            }
            CenterView::Attachments => {
                self.attachment_query.clear();
                self.attachment_cursor = 0;
                self.attachment_index = 0;
                self.attachment_list_start = 0;
                self.recompute_attachments();
            }
            _ => {}
        }
    }
}
