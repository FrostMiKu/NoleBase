//! Dialog functionality: skill.

use super::super::*;

impl App {
    pub(crate) fn finish_skill_browser(&mut self) {
        self.close_dialog();
        if let Some(return_to) = self.skill_browser_return.take() {
            self.center_view = return_to.center_view;
            self.focus = return_to.focus;
            self.document = return_to.document;
        }
    }

    pub(crate) fn return_to_skill_browser(&mut self) {
        if let Some(return_to) = self.skill_browser_return.as_ref() {
            self.center_view = return_to.center_view;
            self.focus = return_to.focus;
            self.document = return_to.document.clone();
        }
        self.reopen_skill_browser();
    }

    pub(crate) fn handle_skill_browser(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            code if is_cancel_key(code) => self.finish_skill_browser(),
            code if is_up_key(code) => {
                self.move_dialog_selection(-1);
                self.skill_index = self.dialog_selected();
            }
            code if is_down_key(code) => {
                self.move_dialog_selection(1);
                self.skill_index = self.dialog_selected();
            }
            KeyCode::Enter => {
                self.skill_index = self.dialog_selected();
                let Some(path) = self
                    .skill_entries
                    .get(self.skill_index)
                    .map(|skill| skill.path.clone())
                else {
                    self.set_status("No skills found");
                    return None;
                };
                match self.storage.read_skill(&path) {
                    Ok(skill) => {
                        self.close_dialog();
                        self.show_document(
                            DocumentKind::Skill(skill.path),
                            skill.name,
                            skill.body,
                            DocumentReturn::Skills,
                        );
                        self.center_view = CenterView::Document;
                        self.focus = Focus::Center;
                    }
                    Err(error) => self.set_error(format!("Skill preview error: {error}")),
                }
            }
            _ => {}
        }
        None
    }
}
