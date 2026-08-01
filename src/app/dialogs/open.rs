//! Dialog functionality: open.

use super::super::*;

impl App {
    pub(in crate::app) fn open_command_palette(&mut self) {
        let dialog = DialogState::new(
            "Command Palette · Ctrl+P",
            String::new(),
            DialogMode::CommandPalette,
            DialogPurpose::CommandPalette,
            Vec::new(),
        );
        self.open_dialog(dialog);
        self.refresh_command_palette();
    }

    pub(in crate::app) fn open_theme_picker(&mut self) {
        let names = match self.storage.list_theme_names() {
            Ok(names) => names,
            Err(error) => {
                self.set_error(format!("Theme list error: {error}"));
                return;
            }
        };
        let mut options = vec![
            DialogOption::with_hint("default", "themes/default.toml"),
            DialogOption::with_hint("random", "Choose a theme at random"),
        ];
        options.extend(names.into_iter().map(|name| {
            let hint = if name == self.active_theme {
                "Custom theme · active"
            } else {
                "Custom theme"
            };
            DialogOption::with_hint(name, hint)
        }));
        let selected = options
            .iter()
            .position(|option| option.label == self.theme_selection)
            .unwrap_or(0);
        let mut dialog = DialogState::new(
            "Theme · Enter apply",
            format!("Active: {}", self.active_theme),
            DialogMode::SingleSelect,
            DialogPurpose::ThemePicker,
            options,
        );
        dialog.selected = selected;
        self.open_dialog(dialog);
    }

    pub(in crate::app) fn open_tag_rename_picker(&mut self) {
        let Some(tags) = self.workspace_index.with_index(WorkspaceIndex::tags) else {
            self.set_status("Tag index is still building");
            return;
        };
        let options = tags
            .into_iter()
            .map(|tag| {
                DialogOption::with_hint(
                    format!("#{}", tag.name),
                    format!("{} documents · {} mentions", tag.documents, tag.mentions),
                )
            })
            .collect::<Vec<_>>();
        if options.is_empty() {
            self.set_status("No tags found");
            return;
        }
        self.open_dialog(DialogState::new(
            "Rename tag · Select source",
            String::new(),
            DialogMode::SingleSelect,
            DialogPurpose::TagRenameSource,
            options,
        ));
    }

    pub(in crate::app) fn open_skill_browser(&mut self) {
        if self.skill_browser_return.is_none() {
            self.skill_browser_return = Some(SkillBrowserReturn {
                center_view: self.center_view,
                focus: self.focus,
                document: self.document.clone(),
            });
        }
        self.reopen_skill_browser();
    }

    pub(in crate::app) fn reopen_skill_browser(&mut self) {
        let selected_id = self
            .skill_entries
            .get(self.skill_index)
            .map(|skill| skill.id.clone());
        let catalog = match self.storage.load_skills() {
            Ok(catalog) => catalog,
            Err(error) => {
                self.set_error(format!("Skill list error: {error}"));
                return;
            }
        };
        if let Some(warning) = catalog.warnings.first() {
            self.set_status(format!("Skill warning: {warning}"));
        }
        self.skill_entries = catalog.skills;
        self.skill_index = selected_id
            .as_deref()
            .and_then(|id| self.skill_entries.iter().position(|skill| skill.id == id))
            .unwrap_or_else(|| {
                self.skill_index
                    .min(self.skill_entries.len().saturating_sub(1))
            });
        let options = self
            .skill_entries
            .iter()
            .map(|skill| DialogOption::with_hint(&skill.id, &skill.description))
            .collect();
        let mut dialog = DialogState::new(
            "Skills · Enter preview",
            String::new(),
            DialogMode::SingleSelect,
            DialogPurpose::SkillBrowser,
            options,
        );
        dialog.selected = self.skill_index;
        self.open_dialog(dialog);
    }

    pub(in crate::app) fn open_file_name_dialog(&mut self, purpose: DialogPurpose) {
        let (title, input, cursor) = match purpose {
            DialogPurpose::NewFile => (
                "New file · Enter create",
                self.new_file_input.clone(),
                self.new_file_cursor,
            ),
            DialogPurpose::RenameFile => (
                "Rename file · Enter save",
                self.rename_input.clone(),
                self.rename_cursor,
            ),
            _ => return,
        };
        let mut dialog =
            DialogState::new(title, "Name  ", DialogMode::SingleLine, purpose, Vec::new());
        dialog.input = input;
        dialog.cursor = cursor;
        self.open_dialog(dialog);
    }

    pub(crate) fn ensure_file_input_dialog(&mut self) {
        if self.overlay == Some(Overlay::Terminal) {
            return;
        }
        let purpose = match self.files_context {
            FilesContext::NewTarget => Some(DialogPurpose::NewFile),
            FilesContext::Rename => Some(DialogPurpose::RenameFile),
            _ => None,
        };
        match purpose {
            Some(purpose) => {
                let needs_open = self.overlay != Some(Overlay::Dialog)
                    || self
                        .dialog
                        .as_ref()
                        .is_none_or(|dialog| dialog.purpose != purpose);
                if needs_open {
                    self.open_file_name_dialog(purpose);
                } else {
                    let current = self.dialog.as_ref().map(|dialog| dialog.input.as_str());
                    let expected = match purpose {
                        DialogPurpose::NewFile => self.new_file_input.as_str(),
                        DialogPurpose::RenameFile => self.rename_input.as_str(),
                        _ => "",
                    };
                    if current != Some(expected) {
                        self.open_file_name_dialog(purpose);
                    }
                }
            }
            None => {
                if self.dialog.as_ref().is_some_and(|dialog| {
                    matches!(
                        dialog.purpose,
                        DialogPurpose::NewFile | DialogPurpose::RenameFile
                    )
                }) {
                    self.overlay = None;
                    self.dialog = None;
                }
            }
        }
    }

    pub(in crate::app) fn dialog_for_overlay(&self, overlay: Overlay) -> DialogState {
        match overlay {
            Overlay::ConfirmDeleteDaily => DialogState::new(
                "Delete daily note",
                "Delete this daily note?",
                DialogMode::Confirm,
                DialogPurpose::DeleteDaily,
                Vec::new(),
            ),
            Overlay::ConfirmDeleteFile => {
                let name = self
                    .pending_file
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "this file".to_string());
                DialogState::new(
                    "Delete file",
                    format!("Delete {name}?"),
                    DialogMode::Confirm,
                    DialogPurpose::DeleteFile,
                    Vec::new(),
                )
            }
            Overlay::Help => DialogState::new(
                "Help",
                String::new(),
                DialogMode::Informational,
                DialogPurpose::Help,
                Vec::new(),
            ),
            Overlay::AiPrompt => {
                let mut dialog = DialogState::new(
                    "Agent prompt",
                    "",
                    DialogMode::FreeText,
                    DialogPurpose::AgentPrompt,
                    Vec::new(),
                );
                dialog.input = self.ai_prompt_input.clone();
                dialog.cursor = self.ai_prompt_cursor;
                dialog
            }
            Overlay::Approval => {
                let request = self.approval_request.as_ref();
                let mut dialog = DialogState::new(
                    request
                        .map(|request| request.title.clone())
                        .unwrap_or_else(|| "Approve change".to_string()),
                    request
                        .map(|request| request.diff.clone())
                        .unwrap_or_default(),
                    DialogMode::Approval,
                    DialogPurpose::AgentApproval,
                    Vec::new(),
                );
                dialog.scroll = self.approval_scroll;
                dialog
            }
            Overlay::AskUser => {
                let request = self.ask_user_request.as_ref();
                let round_limit =
                    request.is_some_and(|request| request.kind == AskUserKind::RoundLimit);
                let mut dialog = DialogState::new(
                    if round_limit {
                        "Agent round limit"
                    } else {
                        "Agent question"
                    },
                    request
                        .map(|request| request.question.clone())
                        .unwrap_or_default(),
                    if round_limit {
                        DialogMode::SingleSelect
                    } else {
                        DialogMode::SelectOrInput
                    },
                    DialogPurpose::AskUser,
                    request
                        .map(|request| {
                            request
                                .options
                                .iter()
                                .cloned()
                                .map(DialogOption::new)
                                .collect()
                        })
                        .unwrap_or_default(),
                );
                dialog.selected = self.ask_user_option;
                dialog.input = self.ask_user_input.clone();
                dialog.cursor = self.ask_user_cursor;
                dialog
            }
            Overlay::WikiLinkChoice => {
                let target = self.wiki_link_target.as_deref().unwrap_or("wikilink");
                let options = self
                    .wiki_link_candidates
                    .iter()
                    .map(|candidate| {
                        let filename = candidate
                            .path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "(unnamed)".to_string());
                        let extension = candidate
                            .path
                            .extension()
                            .map(|extension| extension.to_string_lossy().to_ascii_uppercase())
                            .unwrap_or_else(|| "?".to_string());
                        let hint = if candidate.archived {
                            format!("Archived · {extension}")
                        } else {
                            extension
                        };
                        DialogOption::with_hint(filename, hint)
                    })
                    .collect();
                let mut dialog = DialogState::new(
                    format!("Choose wikilink · [[{target}]]"),
                    String::new(),
                    DialogMode::SingleSelect,
                    DialogPurpose::WikiLinkChoice,
                    options,
                );
                dialog.selected = self.wiki_link_index;
                dialog
            }
            Overlay::Terminal => unreachable!("terminal overlay does not use a dialog"),
            Overlay::Dialog => match self.dialog.as_ref().map(|dialog| dialog.purpose) {
                Some(DialogPurpose::NewFile) => {
                    let mut dialog = DialogState::new(
                        "New file · Enter create",
                        "Name  ",
                        DialogMode::SingleLine,
                        DialogPurpose::NewFile,
                        Vec::new(),
                    );
                    dialog.input = self.new_file_input.clone();
                    dialog.cursor = self.new_file_cursor;
                    dialog
                }
                Some(DialogPurpose::RenameFile) => {
                    let mut dialog = DialogState::new(
                        "Rename file · Enter save",
                        "Name  ",
                        DialogMode::SingleLine,
                        DialogPurpose::RenameFile,
                        Vec::new(),
                    );
                    dialog.input = self.rename_input.clone();
                    dialog.cursor = self.rename_cursor;
                    dialog
                }
                _ => self.dialog.clone().unwrap_or_else(|| {
                    DialogState::new(
                        "Dialog",
                        String::new(),
                        DialogMode::Informational,
                        DialogPurpose::Custom,
                        Vec::new(),
                    )
                }),
            },
        }
    }

    /// Open a caller-defined command dialog. The caller can inspect the
    /// resulting value with [`App::take_dialog_result`] after the dialog
    /// closes.
    pub fn open_dialog(&mut self, dialog: DialogState) {
        if self.overlay == Some(Overlay::Terminal) {
            self.discard_terminal_return_overlay();
        }
        self.dialog_result = None;
        self.dialog = Some(dialog);
        self.overlay = Some(Overlay::Dialog);
    }

    #[allow(dead_code)]
    pub fn take_dialog_result(&mut self) -> Option<DialogResult> {
        self.dialog_result.take()
    }

    pub(crate) fn set_overlay(&mut self, overlay: Overlay) {
        if self.overlay == Some(Overlay::Terminal) && overlay != Overlay::Terminal {
            self.discard_terminal_return_overlay();
        }
        self.overlay = Some(overlay);
        self.dialog = if overlay == Overlay::Terminal {
            None
        } else {
            Some(self.dialog_for_overlay(overlay))
        };
    }
}
