use super::*;

impl App {
    pub(super) fn open_command_palette(&mut self) {
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

    pub(super) fn open_theme_picker(&mut self) {
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

    pub(super) fn open_tag_rename_picker(&mut self) {
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

    pub(super) fn open_skill_browser(&mut self) {
        if self.skill_browser_return.is_none() {
            self.skill_browser_return = Some(SkillBrowserReturn {
                center_view: self.center_view,
                focus: self.focus,
                document: self.document.clone(),
            });
        }
        self.reopen_skill_browser();
    }

    pub(super) fn reopen_skill_browser(&mut self) {
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

    pub(super) fn finish_skill_browser(&mut self) {
        self.close_dialog();
        if let Some(return_to) = self.skill_browser_return.take() {
            self.center_view = return_to.center_view;
            self.focus = return_to.focus;
            self.document = return_to.document;
        }
    }

    pub(super) fn return_to_skill_browser(&mut self) {
        if let Some(return_to) = self.skill_browser_return.as_ref() {
            self.center_view = return_to.center_view;
            self.focus = return_to.focus;
            self.document = return_to.document.clone();
        }
        self.reopen_skill_browser();
    }

    pub(super) fn handle_skill_browser(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.finish_skill_browser(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_dialog_selection(-1);
                self.skill_index = self.dialog_selected();
            }
            KeyCode::Down | KeyCode::Char('j') => {
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
                            skill.id,
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

    pub(super) fn refresh_command_palette(&mut self) {
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

    pub(super) fn execute_selected_palette_command(&mut self) -> Option<Command> {
        let selected = self.dialog_selected();
        let Some(command) = self.command_matches.get(selected).copied() else {
            self.set_status("No matching command");
            return None;
        };
        self.close_dialog();
        self.command_matches.clear();
        self.execute_app_command(command)
    }

    pub(super) fn execute_app_command(&mut self, command: AppCommand) -> Option<Command> {
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
        }
        None
    }

    pub(super) fn command_available(&self, command: AppCommand) -> bool {
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
            AppCommand::ArchiveCurrentNote => self.current_note_archived() == Some(false),
            AppCommand::RestoreCurrentNote => self.current_note_archived() == Some(true),
            AppCommand::EditAiConfig
            | AppCommand::SwitchTheme
            | AppCommand::BrowseTags
            | AppCommand::RenameTag
            | AppCommand::EditAgentInstructions
            | AppCommand::EditAgentMemory
            | AppCommand::BrowseSkills => true,
        }
    }

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

    pub(super) fn open_file_name_dialog(&mut self, purpose: DialogPurpose) {
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

    pub(super) fn dialog_for_overlay(&self, overlay: Overlay) -> DialogState {
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

    pub(super) fn handle_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        self.handle_dialog_key(key)
    }

    pub(super) fn handle_dialog_key(&mut self, key: KeyEvent) -> Option<Command> {
        let Some(dialog) = self.dialog.clone() else {
            self.overlay = None;
            return None;
        };
        match dialog.purpose {
            DialogPurpose::DeleteDaily => {
                return match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.handle_delete_daily_overlay(key)
                    }
                    _ => self.handle_delete_daily_overlay(key),
                };
            }
            DialogPurpose::DeleteFile => return self.handle_delete_file_overlay(key),
            DialogPurpose::Help => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                        self.overlay = None;
                        self.dialog = None;
                    }
                    KeyCode::Down | KeyCode::Char('j') => self.adjust_dialog_scroll(1),
                    KeyCode::Up | KeyCode::Char('k') => self.adjust_dialog_scroll(-1),
                    KeyCode::PageDown => self.adjust_dialog_scroll(8),
                    KeyCode::PageUp => self.adjust_dialog_scroll(-8),
                    _ => {}
                }
                return None;
            }
            DialogPurpose::AgentApproval => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        let _ = self.send_approval(ApprovalDecision::Approve);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        let _ = self.send_approval(ApprovalDecision::Deny);
                    }
                    KeyCode::Down | KeyCode::Char('j') => self.adjust_dialog_scroll(1),
                    KeyCode::Up | KeyCode::Char('k') => self.adjust_dialog_scroll(-1),
                    KeyCode::PageDown => self.adjust_dialog_scroll(8),
                    KeyCode::PageUp => self.adjust_dialog_scroll(-8),
                    _ => {}
                }
                return None;
            }
            DialogPurpose::WikiLinkChoice => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = None;
                        self.dialog = None;
                        self.wiki_link_target = None;
                        self.wiki_link_candidates.clear();
                        self.wiki_link_index = 0;
                    }
                    KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
                    KeyCode::Enter => {
                        if let Some(candidate) = self
                            .wiki_link_candidates
                            .get(self.dialog_selected())
                            .cloned()
                        {
                            self.open_wiki_candidate(&candidate);
                        }
                    }
                    _ => {}
                }
                return None;
            }
            DialogPurpose::SkillBrowser => return self.handle_skill_browser(key),
            DialogPurpose::AskUser => return self.handle_select_or_input_dialog(key),
            DialogPurpose::CommandPalette => return self.handle_command_palette(key),
            DialogPurpose::ThemePicker => return self.handle_theme_picker(key),
            DialogPurpose::TagRenameSource => return self.handle_tag_rename_source(key),
            DialogPurpose::AgentPrompt
            | DialogPurpose::NewFile
            | DialogPurpose::RenameFile
            | DialogPurpose::TagRenameTarget => return self.handle_text_dialog(key),
            DialogPurpose::Custom => {}
        }

        match dialog.mode {
            DialogMode::Confirm => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.dialog_result = Some(DialogResult::Confirm(true));
                    self.close_dialog();
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.dialog_result = Some(DialogResult::Confirm(false));
                    self.close_dialog();
                }
                _ => {}
            },
            DialogMode::SingleSelect => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
                KeyCode::Enter => {
                    if let Some(option) =
                        self.dialog.as_ref().and_then(DialogState::selected_option)
                    {
                        self.dialog_result = Some(DialogResult::Selected(option.label.clone()));
                    }
                    self.close_dialog();
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.dialog_result = Some(DialogResult::Cancelled);
                    self.close_dialog();
                }
                _ => {}
            },
            DialogMode::MultiSelect => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
                KeyCode::Char(' ') => self.toggle_dialog_option(),
                KeyCode::Enter => {
                    let selected = self
                        .dialog
                        .as_ref()
                        .map(DialogState::selected_options)
                        .unwrap_or_default();
                    self.dialog_result = Some(DialogResult::SelectedMany(selected));
                    self.close_dialog();
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.dialog_result = Some(DialogResult::Cancelled);
                    self.close_dialog();
                }
                _ => {}
            },
            DialogMode::SelectOrInput => return self.handle_custom_select_or_input(key),
            DialogMode::SingleLine | DialogMode::FreeText => return self.handle_text_dialog(key),
            DialogMode::CommandPalette => return self.handle_command_palette(key),
            DialogMode::Approval | DialogMode::Informational => {}
        }
        None
    }

    pub(super) fn handle_theme_picker(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_dialog(),
            KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
            KeyCode::Enter => {
                let selection = self
                    .dialog
                    .as_ref()
                    .and_then(DialogState::selected_option)
                    .map(|option| option.label.clone());
                let selection = selection?;
                match self.storage.select_theme(&selection) {
                    Ok(loaded) => {
                        let active = loaded.active.clone();
                        self.apply_loaded_theme(loaded);
                        self.set_status(if selection == active {
                            format!("Theme: {active}")
                        } else {
                            format!("Theme: {active} ({selection})")
                        });
                        self.close_dialog();
                    }
                    Err(error) => self.set_error(format!("Theme switch error: {error}")),
                }
            }
            _ => {}
        }
        None
    }

    pub(super) fn handle_tag_rename_source(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_dialog(),
            KeyCode::Up | KeyCode::Char('k') => self.move_dialog_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_dialog_selection(1),
            KeyCode::Enter => {
                let source = self
                    .dialog
                    .as_ref()
                    .and_then(DialogState::selected_option)
                    .map(|option| option.label.trim_start_matches('#').to_string())?;
                self.pending_tag_rename = Some(source.clone());
                self.open_dialog(DialogState::new(
                    format!("Rename #{source}"),
                    "New tag  #",
                    DialogMode::SingleLine,
                    DialogPurpose::TagRenameTarget,
                    Vec::new(),
                ));
            }
            _ => {}
        }
        None
    }

    pub(super) fn submit_tag_rename(&mut self) {
        let Some(from) = self.pending_tag_rename.clone() else {
            self.set_status("No source tag selected");
            return;
        };
        let to = self
            .dialog
            .as_ref()
            .map(|dialog| dialog.input.clone())
            .unwrap_or_default();
        let Some(paths) = self
            .workspace_index
            .with_index(|index| index.tag_paths(&from))
        else {
            self.set_status("Tag index is still building");
            return;
        };
        let plan = match TagRenamePlan::prepare(&self.storage, paths, &from, &to) {
            Ok(plan) => plan,
            Err(error) => {
                self.set_error(format!("Tag rename error: {error}"));
                return;
            }
        };
        match plan.apply() {
            Ok(outcome) => {
                self.workspace_index
                    .refresh_paths(&self.storage, outcome.paths.clone());
                self.pending_tag_rename = None;
                self.close_dialog();
                self.reload_workspace();
                self.set_status(format!(
                    "Renamed #{} to #{} in {} documents ({} mentions)",
                    outcome.from, outcome.to, outcome.documents, outcome.mentions
                ));
            }
            Err(error) => self.set_error(format!("Tag rename error: {error}")),
        }
    }

    pub(super) fn handle_command_palette(&mut self, key: KeyEvent) -> Option<Command> {
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

    pub(super) fn handle_custom_select_or_input(&mut self, key: KeyEvent) -> Option<Command> {
        let option_count = self
            .dialog
            .as_ref()
            .map_or(0, |dialog| dialog.options.len());
        let custom_selected = self.dialog_selected() >= option_count;
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Esc => {
                self.dialog_result = Some(DialogResult::Cancelled);
                self.close_dialog();
            }
            KeyCode::Up if option_count > 0 => self.move_dialog_selection(-1),
            KeyCode::Down if option_count > 0 => {
                let next = (self.dialog_selected() + 1).min(option_count);
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.selected = next;
                }
            }
            KeyCode::Enter
                if custom_selected
                    && modifiers.intersects(
                        KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
                    ) =>
            {
                self.insert_dialog_char('\n');
            }
            KeyCode::Enter => {
                let result = if custom_selected {
                    DialogResult::Text(
                        self.dialog
                            .as_ref()
                            .map(|dialog| dialog.input.trim().to_string())
                            .unwrap_or_default(),
                    )
                } else {
                    DialogResult::Selected(
                        self.dialog
                            .as_ref()
                            .and_then(DialogState::selected_option)
                            .map(|option| option.label.clone())
                            .unwrap_or_default(),
                    )
                };
                self.dialog_result = Some(result);
                self.close_dialog();
            }
            KeyCode::Backspace => {
                self.select_custom_dialog_option();
                self.delete_dialog_backward();
            }
            KeyCode::Delete => {
                self.select_custom_dialog_option();
                self.delete_dialog_forward();
            }
            KeyCode::Left => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::Left);
            }
            KeyCode::Right => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::Right);
            }
            KeyCode::Home => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::LineStart);
            }
            KeyCode::End => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::LineEnd);
            }
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_dialog_option();
                self.insert_dialog_char('\n');
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_dialog_option();
                self.insert_dialog_char(character);
            }
            _ => {}
        }
        None
    }

    pub(super) fn dialog_selected(&self) -> usize {
        self.dialog.as_ref().map_or(0, |dialog| dialog.selected)
    }

    pub(super) fn move_dialog_selection(&mut self, delta: i32) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        let max = dialog.options.len().saturating_sub(1);
        dialog.selected = if delta < 0 {
            dialog
                .selected
                .saturating_sub(delta.unsigned_abs() as usize)
        } else {
            dialog.selected.saturating_add(delta as usize).min(max)
        };
        if dialog.purpose == DialogPurpose::AskUser {
            self.ask_user_option = dialog.selected;
        } else if dialog.purpose == DialogPurpose::WikiLinkChoice {
            self.wiki_link_index = dialog.selected;
        }
    }

    pub(super) fn toggle_dialog_option(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        if let Some(checked) = dialog.checked.get_mut(dialog.selected) {
            *checked = !*checked;
        }
    }

    pub(super) fn adjust_dialog_scroll(&mut self, delta: i32) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        dialog.scroll = if delta < 0 {
            dialog.scroll.saturating_sub(delta.unsigned_abs() as u16)
        } else {
            dialog.scroll.saturating_add(delta as u16)
        };
        match dialog.purpose {
            DialogPurpose::Help => self.help_scroll = dialog.scroll,
            DialogPurpose::AgentApproval => self.approval_scroll = dialog.scroll,
            _ => {}
        }
    }

    pub(super) fn handle_select_or_input_dialog(&mut self, key: KeyEvent) -> Option<Command> {
        if self
            .ask_user_request
            .as_ref()
            .is_some_and(|request| request.kind == AskUserKind::RoundLimit)
        {
            match key.code {
                KeyCode::Esc => {
                    let _ = self.send_user_response(AskUserResponse::Answer("Stop".to_string()));
                }
                KeyCode::Up => self.move_dialog_selection(-1),
                KeyCode::Down => self.move_dialog_selection(1),
                KeyCode::Enter => {
                    if let Some(answer) = self
                        .dialog
                        .as_ref()
                        .and_then(DialogState::selected_option)
                        .map(|option| option.label.clone())
                    {
                        let _ = self.send_user_response(AskUserResponse::Answer(answer));
                    }
                }
                _ => {}
            }
            return None;
        }
        let option_count = self
            .dialog
            .as_ref()
            .map_or(0, |dialog| dialog.options.len());
        let custom_selected = self.dialog_selected() >= option_count;
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Esc => {
                let _ = self.send_user_response(AskUserResponse::Cancelled);
            }
            KeyCode::Up if option_count > 0 => self.move_dialog_selection(-1),
            KeyCode::Down if option_count > 0 => {
                let next = (self.dialog_selected() + 1).min(option_count);
                if let Some(dialog) = self.dialog.as_mut() {
                    dialog.selected = next;
                }
                self.ask_user_option = next;
            }
            KeyCode::Enter
                if custom_selected
                    && modifiers.intersects(
                        KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
                    ) =>
            {
                self.insert_dialog_char('\n');
            }
            KeyCode::Enter => {
                let answer = if custom_selected {
                    self.dialog
                        .as_ref()
                        .map(|dialog| dialog.input.trim().to_string())
                        .unwrap_or_default()
                } else {
                    self.dialog
                        .as_ref()
                        .and_then(DialogState::selected_option)
                        .map(|option| option.label.clone())
                        .unwrap_or_default()
                };
                if answer.is_empty() {
                    self.set_status("Enter an answer before submitting");
                } else {
                    let _ = self.send_user_response(AskUserResponse::Answer(answer));
                }
            }
            KeyCode::Backspace => {
                self.select_custom_dialog_option();
                self.delete_dialog_backward();
            }
            KeyCode::Delete => {
                self.select_custom_dialog_option();
                self.delete_dialog_forward();
            }
            KeyCode::Left => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::Left);
            }
            KeyCode::Right => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::Right);
            }
            KeyCode::Home => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::LineStart);
            }
            KeyCode::End => {
                self.select_custom_dialog_option();
                self.move_dialog_cursor(CursorMove::LineEnd);
            }
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_dialog_option();
                self.insert_dialog_char('\n');
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.select_custom_dialog_option();
                self.insert_dialog_char(character);
            }
            _ => {}
        }
        None
    }

    pub(super) fn handle_text_dialog(&mut self, key: KeyEvent) -> Option<Command> {
        let modifiers = key.modifiers;
        let single_line = self
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.mode == DialogMode::SingleLine);
        match key.code {
            KeyCode::Enter
                if !single_line
                    && modifiers.intersects(
                        KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
                    ) =>
            {
                self.insert_dialog_char('\n');
            }
            KeyCode::Enter => {
                self.sync_dialog_owner_state();
                match self.dialog.as_ref().map(|dialog| dialog.purpose) {
                    Some(DialogPurpose::AgentPrompt) => self.submit_agent_prompt(),
                    Some(DialogPurpose::NewFile) => {
                        self.handle_new_target(key);
                        if self.files_context != FilesContext::NewTarget {
                            self.close_dialog();
                        }
                    }
                    Some(DialogPurpose::RenameFile) => {
                        self.handle_rename(key);
                        if self.files_context != FilesContext::Rename {
                            self.close_dialog();
                        }
                    }
                    Some(DialogPurpose::TagRenameTarget) => self.submit_tag_rename(),
                    _ => {
                        let text = self
                            .dialog
                            .as_ref()
                            .map(|dialog| dialog.input.clone())
                            .unwrap_or_default();
                        self.dialog_result = Some(DialogResult::Text(text));
                        self.close_dialog();
                    }
                }
            }
            KeyCode::Esc => {
                match self.dialog.as_ref().map(|dialog| dialog.purpose) {
                    Some(DialogPurpose::AgentPrompt) => self.ai_source_date = None,
                    Some(DialogPurpose::NewFile) => {
                        self.pending_daily_date = None;
                        self.files_context = FilesContext::Browse;
                    }
                    Some(DialogPurpose::RenameFile) => {
                        self.pending_file = None;
                        self.files_context = FilesContext::Browse;
                    }
                    Some(DialogPurpose::TagRenameTarget) => {
                        self.pending_tag_rename = None;
                    }
                    _ => self.dialog_result = Some(DialogResult::Cancelled),
                }
                self.close_dialog();
            }
            _ if single_line => {
                self.edit_dialog_single_line(key);
            }
            KeyCode::Backspace => self.delete_dialog_backward(),
            KeyCode::Delete => self.delete_dialog_forward(),
            KeyCode::Left => self.move_dialog_cursor(CursorMove::Left),
            KeyCode::Right => self.move_dialog_cursor(CursorMove::Right),
            KeyCode::Up => self.move_dialog_cursor(CursorMove::Up),
            KeyCode::Down => self.move_dialog_cursor(CursorMove::Down),
            KeyCode::Home => self.move_dialog_cursor(CursorMove::LineStart),
            KeyCode::End => self.move_dialog_cursor(CursorMove::LineEnd),
            KeyCode::Char('j') if !single_line && modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_dialog_char('\n')
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_dialog_char(character)
            }
            _ => {}
        }
        None
    }

    pub(super) fn edit_dialog_single_line(&mut self, key: KeyEvent) -> TextInputEdit {
        let Some(dialog) = self.dialog.as_mut() else {
            return TextInputEdit::Ignored;
        };
        let edit = edit_single_line(&mut dialog.input, &mut dialog.cursor, key);
        if edit.handled() {
            self.sync_dialog_owner_state();
        }
        edit
    }

    pub(super) fn insert_dialog_char(&mut self, character: char) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        insert_char(&mut dialog.input, &mut dialog.cursor, character);
        self.sync_dialog_owner_state();
    }

    pub(super) fn delete_dialog_backward(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        delete_backward(&mut dialog.input, &mut dialog.cursor);
        self.sync_dialog_owner_state();
    }

    pub(super) fn delete_dialog_forward(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        delete_forward(&mut dialog.input, &mut dialog.cursor);
        self.sync_dialog_owner_state();
    }

    pub(super) fn move_dialog_cursor(&mut self, movement: CursorMove) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        dialog.cursor = move_cursor(&dialog.input, dialog.cursor, movement);
        self.sync_dialog_owner_state();
    }

    pub(super) fn select_custom_dialog_option(&mut self) {
        let count = self
            .dialog
            .as_ref()
            .map_or(0, |dialog| dialog.options.len());
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.selected = count;
        }
        self.ask_user_option = count;
    }

    pub(super) fn sync_dialog_owner_state(&mut self) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };
        match dialog.purpose {
            DialogPurpose::AgentPrompt => {
                self.ai_prompt_input = dialog.input.clone();
                self.ai_prompt_cursor = dialog.cursor;
            }
            DialogPurpose::AskUser => {
                self.ask_user_input = dialog.input.clone();
                self.ask_user_cursor = dialog.cursor;
                self.ask_user_option = dialog.selected;
            }
            DialogPurpose::NewFile => {
                self.new_file_input = dialog.input.clone();
                self.new_file_cursor = dialog.cursor;
            }
            DialogPurpose::RenameFile => {
                self.rename_input = dialog.input.clone();
                self.rename_cursor = dialog.cursor;
            }
            DialogPurpose::AgentApproval => self.approval_scroll = dialog.scroll,
            DialogPurpose::Help => self.help_scroll = dialog.scroll,
            DialogPurpose::WikiLinkChoice => self.wiki_link_index = dialog.selected,
            _ => {}
        }
    }

    pub(super) fn close_dialog(&mut self) {
        self.overlay = None;
        self.dialog = None;
    }

    pub(super) fn handle_delete_daily_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(date) = self.pending_daily_date.take() {
                    let note = self.daily_note_clone(date);
                    match self.storage.remove_daily(&date.to_string()) {
                        Ok(true) => {
                            if let Some(note) = note {
                                self.record_undo(UndoOp::Delete(note));
                            }
                            self.set_status("Deleted");
                            self.reload();
                            self.reload_todos();
                        }
                        Ok(false) => self.set_status("Daily note not found"),
                        Err(error) => self.set_error(format!("Error: {error}")),
                    }
                }
                self.overlay = None;
                self.dialog = None;
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_daily_date = None;
                self.overlay = None;
                self.dialog = None;
                None
            }
            _ => None,
        }
    }

    pub(super) fn handle_delete_file_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                let mut deleted_skill = false;
                if let Some(path) = self.pending_file.take() {
                    let skill = self.document.as_ref().is_some_and(|document| {
                        matches!(&document.kind, DocumentKind::Skill(open) if open == &path)
                    });
                    let archived = self
                        .note_files
                        .iter()
                        .find(|file| file.path == path)
                        .is_some_and(|file| file.archived);
                    let result = if skill {
                        self.storage.delete_skill(&path)
                    } else if archived {
                        self.storage.delete_archived_file(&path)
                    } else {
                        self.storage.delete_file(&path)
                    };
                    match result {
                        Ok(()) => {
                            let kind = if skill {
                                DocumentKind::Skill(path.clone())
                            } else {
                                DocumentKind::File(path.clone())
                            };
                            self.document_render_lru.remove(&kind);
                            self.set_status(format!(
                                "Deleted {}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            ));
                            if skill {
                                self.document = None;
                                deleted_skill = true;
                            } else if self
                                .document
                                .as_ref()
                                .is_some_and(|document| document.kind == DocumentKind::File(path))
                            {
                                self.document = None;
                                self.center_view = CenterView::Daily;
                            }
                            if !skill {
                                self.reload_files();
                            }
                        }
                        Err(error) => self.set_error(format!("Error: {error}")),
                    }
                }
                self.overlay = None;
                self.dialog = None;
                if deleted_skill {
                    self.return_to_skill_browser();
                }
                None
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pending_file = None;
                self.overlay = None;
                self.dialog = None;
                None
            }
            _ => None,
        }
    }
}
