//! Dialog functionality: palette.

use super::super::*;

impl App {
    pub(crate) fn refresh_command_palette(&mut self) {
        let query = self
            .dialog
            .as_ref()
            .map(|dialog| dialog.input.trim().to_string())
            .unwrap_or_default();
        let mut matches = APP_COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, command)| self.command_available(command.id))
            .filter_map(|(index, command)| {
                command_match_score(command, &query).map(|score| (score, index, command.id))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(score, index, _)| (*score, *index));
        self.command_matches = matches.into_iter().map(|(_, _, id)| id).collect();
        let options = self
            .command_matches
            .iter()
            .filter_map(|id| {
                let command = command_definition(*id)?;
                let (label, description) = if *id == AppCommand::ToggleMouseSupport {
                    if self.mouse_captured {
                        (
                            "Interface: Disable mouse support",
                            "Disable mouse support to select and copy text with the terminal",
                        )
                    } else {
                        (
                            "Interface: Enable mouse support",
                            "Restore mouse clicking and scrolling",
                        )
                    }
                } else {
                    (command.label, command.description)
                };
                Some(DialogOption::with_hint(label, description))
            })
            .collect::<Vec<_>>();
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.options = options;
            dialog.checked = vec![false; dialog.options.len()];
            dialog.selected = dialog.selected.min(dialog.options.len().saturating_sub(1));
        }
    }

    pub(crate) fn execute_selected_palette_command(&mut self) -> Option<Command> {
        let selected = self.dialog_selected();
        let Some(command) = self.command_matches.get(selected).copied() else {
            self.set_status("No matching command");
            return None;
        };
        self.close_dialog();
        self.command_matches.clear();
        self.execute_app_command(command)
    }

    pub(in crate::app) fn execute_app_command(&mut self, command: AppCommand) -> Option<Command> {
        match command {
            AppCommand::InterruptAgent => self.cancel_agent(),
            AppCommand::ClearAgentSession => self.clear_agent_session(),
            AppCommand::OpenTerminal => self.toggle_terminal(),
            AppCommand::ToggleMouseSupport => {
                self.mouse_captured = !self.mouse_captured;
                self.set_status(if self.mouse_captured {
                    "Mouse support enabled"
                } else {
                    "Mouse support disabled; terminal text selection available"
                });
                return Some(Command::SetMouseCapture(self.mouse_captured));
            }
            AppCommand::NewNote => self.begin_new_note(),
            AppCommand::NewNoteFromTemplate => self.begin_new_note_from_template(),
            AppCommand::EditTemplate => {
                return Some(Command::Edit(self.storage.template_path.clone()));
            }
            AppCommand::EditCurrentNote => {
                return self.current_note_path().map(Command::Edit);
            }
            AppCommand::ExportCurrentFile => self.open_export_dialog(),
            AppCommand::RenameCurrentNote => self.rename_current_note(),
            AppCommand::DeleteCurrentNote => self.delete_current_note(),
            AppCommand::ArchiveCurrentNote => self.manage_current_note(false),
            AppCommand::RestoreCurrentNote => self.manage_current_note(true),
            AppCommand::EditAiConfig => {
                return Some(Command::Edit(self.storage.ai_config_path.clone()));
            }
            AppCommand::SwitchTheme => self.open_theme_picker(),
            AppCommand::BrowseTags => self.open_tags(),
            AppCommand::RenameTag => self.open_tag_rename_picker(),
            AppCommand::EditAgentInstructions => {
                return Some(Command::Edit(self.storage.agents_path.clone()));
            }
            AppCommand::EditAgentMemory => {
                return Some(Command::Edit(self.storage.memory_path.clone()));
            }
            AppCommand::BrowseSkills => self.open_skill_browser(),
            AppCommand::BrowseAttachments => self.open_attachments(),
            AppCommand::PasteClipboardAsAttachment => self.paste_clipboard_as_attachment(),
        }
        None
    }

    pub(in crate::app) fn command_available(&self, command: AppCommand) -> bool {
        match command {
            AppCommand::InterruptAgent
            | AppCommand::ClearAgentSession
            | AppCommand::OpenTerminal
            | AppCommand::ToggleMouseSupport
            | AppCommand::NewNote
            | AppCommand::NewNoteFromTemplate
            | AppCommand::EditTemplate => true,
            AppCommand::EditCurrentNote
            | AppCommand::RenameCurrentNote
            | AppCommand::DeleteCurrentNote => self.current_note_path().is_some(),
            AppCommand::ExportCurrentFile => self.current_export_path().is_some(),
            AppCommand::ArchiveCurrentNote => self.current_note_archived() == Some(false),
            AppCommand::RestoreCurrentNote => self.current_note_archived() == Some(true),
            AppCommand::EditAiConfig
            | AppCommand::SwitchTheme
            | AppCommand::BrowseTags
            | AppCommand::RenameTag
            | AppCommand::EditAgentInstructions
            | AppCommand::EditAgentMemory
            | AppCommand::BrowseSkills
            | AppCommand::BrowseAttachments => true,
            AppCommand::PasteClipboardAsAttachment => self.can_paste_clipboard_as_attachment(),
        }
    }

    pub(crate) fn handle_command_palette(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc => {
                self.close_dialog();
                self.command_matches.clear();
            }
            KeyCode::Up => self.move_dialog_selection(-1),
            KeyCode::Down => self.move_dialog_selection(1),
            KeyCode::Enter => return self.execute_selected_palette_command(),
            _ => {
                if self.edit_dialog_single_line(key).changed() {
                    self.refresh_command_palette();
                }
            }
        }
        None
    }
}
