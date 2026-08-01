//! Document browsing and actions: todos.

use super::super::*;

impl App {
    /// Original daily-task indices in display order: open tasks first, completed
    /// tasks second, with source order preserved inside each group.
    pub fn visible_todo_indices(&self) -> Vec<usize> {
        self.todo_items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                (!item.checked && fuzzy_match(&item.text, &self.todo_query)).then_some(index)
            })
            .chain(
                self.todo_items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        (item.checked && fuzzy_match(&item.text, &self.todo_query)).then_some(index)
                    }),
            )
            .collect()
    }

    pub fn open_todo(&mut self) {
        self.activate_workspace_view(CenterView::Todo);
    }

    pub(in crate::app) fn move_todo_selection(&mut self, delta: i32) {
        let visible = self.visible_todo_indices();
        if visible.is_empty() {
            self.todo_index = 0;
            return;
        }
        let position = visible
            .iter()
            .position(|index| *index == self.todo_index)
            .unwrap_or(0);
        self.todo_index = visible[move_index(position, delta, visible.len())];
    }

    pub(in crate::app) fn ensure_visible_todo_selection(&mut self) {
        let visible = self.visible_todo_indices();
        if !visible.contains(&self.todo_index) {
            self.todo_index = visible.first().copied().unwrap_or(0);
        }
    }

    pub(in crate::app) fn toggle_todo(&mut self, index: usize) {
        match self.storage.toggle_todo_task(index) {
            Ok(true) => {
                self.reload_todos();
                self.reload_files();
            }
            Ok(false) => self.set_status("No such task"),
            Err(error) => self.set_error(format!("Error: {error}")),
        }
    }
}
