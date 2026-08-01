//! Document browsing and actions: actions.

use super::super::*;

impl App {
    pub(in crate::app) fn act(&mut self, action: Action) -> Option<Command> {
        let date = self.selected_date()?;
        self.dispatch_action(date, action)
    }

    pub(in crate::app) fn dispatch_action(
        &mut self,
        date: NaiveDate,
        action: Action,
    ) -> Option<Command> {
        match action {
            Action::Ai => {
                self.open_agent_prompt(date);
                None
            }
            Action::Move => {
                self.pending_daily_date = Some(date);
                self.file_query.clear();
                self.file_query_cursor = 0;
                self.reload_files();
                self.files_context = FilesContext::MoveTarget;
                self.ensure_visible_file_selection();
                self.focus = Focus::Files;
                None
            }
            Action::Archive => {
                if let Some(note) = self.daily_note_clone(date) {
                    match self.storage.archive_daily(&note.date.to_string()) {
                        Ok(_) => {
                            self.record_undo(UndoOp::Archive(note));
                            self.set_status("Daily note archived");
                            self.reload_workspace();
                        }
                        Err(error) => self.set_error(format!("Error: {error}")),
                    }
                }
                None
            }
            Action::New => {
                self.pending_daily_date = Some(date);
                self.new_file_input.clear();
                self.new_file_cursor = 0;
                self.files_context = FilesContext::NewTarget;
                self.focus = Focus::Files;
                self.open_file_name_dialog(DialogPurpose::NewFile);
                None
            }
            Action::View => {
                self.open_daily_document(date, DocumentReturn::Daily);
                None
            }
            Action::Edit => self.daily_edit_command(date),
            Action::Delete => {
                self.pending_daily_date = Some(date);
                self.set_overlay(Overlay::ConfirmDeleteDaily);
                None
            }
        }
    }
}
