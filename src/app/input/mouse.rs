//! Keyboard and mouse input: mouse.

use super::super::*;

impl App {
    pub(in crate::app) fn route_wheel(&mut self, column: u16, row: u16, delta: i32) {
        if self.overlay.is_some() {
            if self.layout.overlay.is_none() || in_area(column, row, self.layout.overlay) {
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.scroll = if delta > 0 {
                        dialog.scroll.saturating_add(delta as u16)
                    } else {
                        dialog.scroll.saturating_sub(delta.unsigned_abs() as u16)
                    };
                    match dialog.purpose {
                        DialogPurpose::Help => self.help_scroll = dialog.scroll,
                        DialogPurpose::AgentApproval | DialogPurpose::AgentDestructiveApproval => {
                            self.approval_scroll = dialog.scroll
                        }
                        _ => {}
                    }
                }
            }
            return;
        }
        if matches!(
            self.files_context,
            FilesContext::NewTarget | FilesContext::Rename
        ) {
            return;
        }

        if in_area(column, row, self.layout.files) {
            self.move_file_selection(delta);
        } else if in_area(column, row, self.layout.views) {
            self.move_workspace_view_selection(delta);
        } else if in_area(column, row, self.layout.agent) {
            self.scroll_agent_by(delta);
        } else if in_area(column, row, self.layout.center) {
            match self.center_view {
                CenterView::Daily => {
                    self.reveal_selected_daily = false;
                    self.scroll = if delta > 0 {
                        self.scroll.saturating_add(delta as u16)
                    } else {
                        self.scroll.saturating_sub(delta.unsigned_abs() as u16)
                    };
                }
                CenterView::Chat => self.scroll_agent_by(delta),
                CenterView::Todo => self.move_todo_selection(delta),
                CenterView::Document => self.scroll_document(delta),
                CenterView::Search | CenterView::DocumentSearch => {
                    self.move_search_selection(delta)
                }
                CenterView::Tags => {
                    if self.active_tag.is_some() {
                        self.scroll_tag_notes(delta);
                    } else {
                        self.move_tag_selection(delta);
                    }
                }
                CenterView::Attachments => self.move_attachment_selection(delta),
            }
        }
    }

    pub(in crate::app) fn handle_left_click(&mut self, column: u16, row: u16) -> Option<Command> {
        if self.overlay.is_some() {
            if let Some(index) = self
                .dialog_hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| hitbox.index)
                .or_else(|| {
                    self.wiki_link_hitboxes
                        .iter()
                        .find(|hitbox| point_in_rect(column, row, hitbox.area))
                        .map(|hitbox| hitbox.index)
                })
            {
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.selected = index;
                }
                self.sync_dialog_owner_state();
                if self
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.purpose == DialogPurpose::CommandPalette)
                {
                    return self.execute_selected_palette_command();
                }
                if self
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.purpose == DialogPurpose::WikiLinkChoice)
                {
                    if let Some(candidate) = self.wiki_link_candidates.get(index).cloned() {
                        self.open_wiki_candidate(&candidate);
                    }
                }
            }
            return None;
        }
        if matches!(
            self.files_context,
            FilesContext::NewTarget | FilesContext::Rename
        ) {
            return None;
        }

        if let Some((target, area)) = self
            .link_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| (hitbox.target.clone(), hitbox.area))
        {
            if matches!(target, LinkTarget::CopyCode(_)) {
                self.begin_code_copy(area);
            }
            return self.activate_link(target);
        }

        if let Some(name) = self
            .tag_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.name.clone())
        {
            self.open_tag_documents(&name);
            return None;
        }

        if let Some(path) = self
            .backlink_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.path.clone())
        {
            // Navigation replaces the open document, so it inherits the
            // current document's return context (Search, Tags, Daily, ...)
            // instead of defaulting to Daily.
            let return_to = self
                .document
                .as_ref()
                .map(|document| document.return_to)
                .unwrap_or(DocumentReturn::Daily);
            self.open_file_document(&path, return_to);
            return None;
        }

        if self.center_view == CenterView::Tags && self.active_tag.is_some() {
            if let Some(index) = self
                .tag_note_hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| hitbox.index)
            {
                self.tag_note_index = index;
                self.open_tag_note_at(index);
                return None;
            }
        }

        if matches!(
            self.center_view,
            CenterView::Search | CenterView::DocumentSearch
        ) {
            if let Some(index) = self
                .search_hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| hitbox.index)
            {
                self.search_index = index;
                self.jump_to_search_result(index);
                return None;
            }

            if let Some(index) = self
                .attachment_hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| hitbox.index)
            {
                self.attachment_index = index;
                return None;
            }
        }

        if let Some(index) = self
            .todo_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.index)
        {
            if self.center_view == CenterView::Todo {
                self.focus = Focus::Center;
            } else {
                self.activate_workspace_view(CenterView::Todo);
            }
            self.todo_index = index;
            self.toggle_todo(index);
            return None;
        }

        if let Some(index) = self
            .workspace_view_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.index)
        {
            if let Some(center_view) = WorkspaceView::ALL.get(index).map(|view| view.center_view) {
                self.activate_workspace_view(center_view);
            }
            return None;
        }

        if let Some(group) = self
            .file_group_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.group)
        {
            self.focus = Focus::Files;
            if let Some(row) = self
                .visible_file_rows()
                .iter()
                .position(|item| *item == FileListRow::Group(group))
            {
                self.select_file_row(row);
                self.toggle_file_group(group);
            }
            return None;
        }

        if let Some(path) = self
            .file_hitboxes
            .iter()
            .find(|hitbox| point_in_rect(column, row, hitbox.area))
            .map(|hitbox| hitbox.path.clone())
        {
            if let Some(index) = self.note_files.iter().position(|file| file.path == path) {
                self.file_index = index;
                self.sync_selected_file();
                self.focus = Focus::Files;
                match self.files_context {
                    FilesContext::Browse | FilesContext::Search => {
                        self.open_selected_file(DocumentReturn::Daily)
                    }
                    FilesContext::MoveTarget => self.perform_move_to(&path),
                    FilesContext::NewTarget | FilesContext::Rename => {}
                }
            }
            return None;
        }

        if self.center_view == CenterView::Daily {
            if let Some((date, action)) = self
                .hitboxes
                .iter()
                .find(|hitbox| point_in_rect(column, row, hitbox.area))
                .map(|hitbox| (hitbox.date, hitbox.action))
            {
                return self.dispatch_action(date, action);
            }
        }

        if in_area(column, row, self.layout.compose)
            && matches!(
                self.center_view,
                CenterView::Daily | CenterView::Chat | CenterView::Document
            )
        {
            self.focus = Focus::Compose;
        } else if in_area(column, row, self.layout.files) {
            self.focus = Focus::Files;
        } else if in_area(column, row, self.layout.views) {
            self.focus = Focus::Views;
        } else if in_area(column, row, self.layout.agent) {
            self.focus = Focus::Agent;
        } else if in_area(column, row, self.layout.center) {
            self.focus = Focus::Center;
        }
        None
    }
}
