//! Dialog functionality: handlers.

use super::super::*;

use crate::export::ExportDiagnostic;
use crate::storage::{ExportDestinationPolicy, PreparedExport};

impl App {
    pub(crate) fn handle_overlay(&mut self, key: KeyEvent) -> Option<Command> {
        self.handle_dialog_key(key)
    }

    pub(crate) fn handle_dialog_key(&mut self, key: KeyEvent) -> Option<Command> {
        let Some(dialog) = self.dialog.clone() else {
            self.overlay = None;
            return None;
        };
        match dialog.purpose {
            DialogPurpose::DeleteDaily => return self.handle_delete_daily_overlay(key),
            DialogPurpose::DeleteFile => return self.handle_delete_file_overlay(key),
            DialogPurpose::DeleteAttachment => return self.handle_delete_attachment_overlay(key),
            DialogPurpose::Help => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                        self.overlay = None;
                        self.dialog = None;
                    }
                    code if is_down_key(code) => self.adjust_dialog_scroll(1),
                    code if is_up_key(code) => self.adjust_dialog_scroll(-1),
                    KeyCode::PageDown => self.adjust_dialog_scroll(DIALOG_PAGE_STEP),
                    KeyCode::PageUp => self.adjust_dialog_scroll(-DIALOG_PAGE_STEP),
                    _ => {}
                }
                return None;
            }
            DialogPurpose::AgentApproval | DialogPurpose::AgentDestructiveApproval => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        let _ = self.send_approval(ApprovalDecision::Approve);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        let _ = self.send_approval(ApprovalDecision::Deny);
                    }
                    code if is_down_key(code) => self.adjust_dialog_scroll(1),
                    code if is_up_key(code) => self.adjust_dialog_scroll(-1),
                    KeyCode::PageDown => self.adjust_dialog_scroll(DIALOG_PAGE_STEP),
                    KeyCode::PageUp => self.adjust_dialog_scroll(-DIALOG_PAGE_STEP),
                    _ => {}
                }
                return None;
            }
            DialogPurpose::WikiLinkChoice => {
                match key.code {
                    code if is_cancel_key(code) => {
                        self.overlay = None;
                        self.dialog = None;
                        self.wiki_link_target = None;
                        self.wiki_link_candidates.clear();
                        self.wiki_link_index = 0;
                    }
                    code if is_up_key(code) => self.move_dialog_selection(-1),
                    code if is_down_key(code) => self.move_dialog_selection(1),
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
            DialogPurpose::ExportFormat => return self.handle_export_format(key),
            DialogPurpose::AgentPrompt
            | DialogPurpose::NewFile
            | DialogPurpose::RenameFile
            | DialogPurpose::TagRenameTarget
            | DialogPurpose::ExportDestination => return self.handle_text_dialog(key),
            DialogPurpose::ExportOverwrite => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.confirm_export_overwrite();
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.cancel_export_overwrite();
                    }
                    _ => {}
                }
                return None;
            }
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
                code if is_up_key(code) => self.move_dialog_selection(-1),
                code if is_down_key(code) => self.move_dialog_selection(1),
                KeyCode::Enter => {
                    if let Some(option) =
                        self.dialog.as_ref().and_then(DialogState::selected_option)
                    {
                        self.dialog_result = Some(DialogResult::Selected(option.label.clone()));
                    }
                    self.close_dialog();
                }
                code if is_cancel_key(code) => {
                    self.dialog_result = Some(DialogResult::Cancelled);
                    self.close_dialog();
                }
                _ => {}
            },
            DialogMode::MultiSelect => match key.code {
                code if is_up_key(code) => self.move_dialog_selection(-1),
                code if is_down_key(code) => self.move_dialog_selection(1),
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
                code if is_cancel_key(code) => {
                    self.dialog_result = Some(DialogResult::Cancelled);
                    self.close_dialog();
                }
                _ => {}
            },
            DialogMode::SelectOrInput => return self.handle_custom_select_or_input(key),
            DialogMode::SingleLine | DialogMode::FreeText => return self.handle_text_dialog(key),
            DialogMode::CommandPalette => return self.handle_command_palette(key),
            DialogMode::Approval | DialogMode::CommandApproval | DialogMode::Informational => {}
        }
        None
    }

    pub(crate) fn handle_theme_picker(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            code if is_cancel_key(code) => self.close_dialog(),
            code if is_up_key(code) => self.move_dialog_selection(-1),
            code if is_down_key(code) => self.move_dialog_selection(1),
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

    pub(crate) fn handle_export_format(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            code if is_up_key(code) => self.move_dialog_selection(-1),
            code if is_down_key(code) => self.move_dialog_selection(1),
            KeyCode::Enter => {
                let selected = self.dialog_selected();
                if let Some(format) = ExportFormat::ALL.get(selected).copied() {
                    self.pending_export_format = Some(format);
                    self.open_export_destination_dialog();
                }
            }
            code if is_cancel_key(code) => self.close_dialog(),
            _ => {}
        }
        None
    }

    fn submit_export(&mut self) {
        if self.export_in_progress {
            self.set_status("Export is already in progress");
            return;
        }
        let Some(source) = self.pending_export_source.clone() else {
            self.set_error("Export source is no longer available");
            return;
        };
        let Some(format) = self.pending_export_format else {
            self.set_error("Export format is no longer available");
            return;
        };
        let destination = self
            .dialog
            .as_ref()
            .map(|dialog| dialog.input.clone())
            .unwrap_or_default();
        // An existing destination never fails or overwrites silently: when it
        // is a plain file, ask for explicit confirmation first. Directories,
        // symlinks, and special files are not offered and fall through to the
        // normal preparation error.
        if self
            .storage
            .export_destination_is_overwritable(Path::new(&destination))
            .unwrap_or(false)
        {
            self.pending_export_destination = Some(destination);
            self.open_export_overwrite_dialog();
            return;
        }
        let prepared = match self.storage.prepare_export(
            &source,
            Path::new(&destination),
            format,
            ExportDestinationPolicy::CreateNew,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                // Keep the dialog open with the typed destination so the
                // user can fix the input.
                self.set_error(error.to_string());
                return;
            }
        };
        self.start_export_worker(prepared, destination, format);
    }

    /// Launch the background publication worker for a prepared export and
    /// record the pending state so a background failure can restore the
    /// destination input for a direct retry. The prepared export already
    /// carries its destination policy (`CreateNew` from the destination
    /// dialog, `ReplaceExisting` from the overwrite confirmation).
    fn start_export_worker(
        &mut self,
        prepared: PreparedExport,
        destination: String,
        format: ExportFormat,
    ) {
        let resolved = prepared.destination().display().to_string();
        let storage = self.storage.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("nole-export".to_string())
            .spawn(move || {
                let outcome = storage
                    .publish_export(&prepared)
                    .map_err(|error| error.to_string());
                let _ = sender.send(ExportJobResult { format, outcome });
            });
        if let Err(error) = worker {
            self.set_error(format!("starting export worker: {error}"));
            return;
        }
        self.pending_export_destination = Some(destination);
        self.export_job = Some(receiver);
        self.export_job_format = Some(format);
        self.export_in_progress = true;
        // Dismiss the dialog while keeping the pending export state, so a
        // background failure can restore it for a direct retry.
        self.overlay = None;
        self.dialog = None;
        self.set_status(format!("Exporting as {} to {resolved}", format.label()));
    }

    fn confirm_export_overwrite(&mut self) {
        let Some(source) = self.pending_export_source.clone() else {
            self.set_error("Export source is no longer available");
            self.close_dialog();
            return;
        };
        let Some(format) = self.pending_export_format else {
            self.set_error("Export format is no longer available");
            self.close_dialog();
            return;
        };
        let Some(destination) = self.pending_export_destination.clone() else {
            self.set_error("Export destination is no longer available");
            self.close_dialog();
            return;
        };
        match self.storage.prepare_export(
            &source,
            Path::new(&destination),
            format,
            ExportDestinationPolicy::ReplaceExisting,
        ) {
            Ok(prepared) => self.start_export_worker(prepared, destination, format),
            Err(error) => {
                // The destination changed since the confirmation was offered
                // (gone, replaced by a directory or symlink, ...). Return to
                // the destination input so the user can retry without losing
                // their path.
                self.set_error(error.to_string());
                self.open_export_destination_dialog_with_input(Some(&destination));
            }
        }
    }

    fn cancel_export_overwrite(&mut self) {
        let destination = self.pending_export_destination.clone();
        self.open_export_destination_dialog_with_input(destination.as_deref());
    }

    pub fn poll_export(&mut self) {
        let result = match self.export_job.as_ref().map(mpsc::Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(mpsc::TryRecvError::Disconnected)) => Some(ExportJobResult {
                format: self.export_job_format.unwrap_or(ExportFormat::Original),
                outcome: Err("export worker disconnected".to_string()),
            }),
            Some(Err(mpsc::TryRecvError::Empty)) | None => None,
        };
        let Some(result) = result else {
            return;
        };
        self.export_job = None;
        self.export_job_format = None;
        self.export_in_progress = false;
        match result.outcome {
            Ok(outcome) => {
                let message = export_success_message(&outcome, result.format, &outcome.diagnostics);
                self.pending_export_source = None;
                self.pending_export_format = None;
                self.pending_export_destination = None;
                self.notifications.notify(message.clone());
                self.set_status(message);
            }
            Err(error) => {
                self.set_error(error);
                if self.dialog.is_none()
                    && self.pending_export_source.is_some()
                    && self.pending_export_format.is_some()
                {
                    let destination = self.pending_export_destination.clone();
                    self.open_export_destination_dialog_with_input(destination.as_deref());
                }
            }
        }
    }

    pub(crate) fn handle_tag_rename_source(&mut self, key: KeyEvent) -> Option<Command> {
        match key.code {
            code if is_cancel_key(code) => self.close_dialog(),
            code if is_up_key(code) => self.move_dialog_selection(-1),
            code if is_down_key(code) => self.move_dialog_selection(1),
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

    pub(crate) fn submit_tag_rename(&mut self) {
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

    pub(crate) fn handle_custom_select_or_input(&mut self, key: KeyEvent) -> Option<Command> {
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

    pub(crate) fn dialog_selected(&self) -> usize {
        self.dialog.as_ref().map_or(0, |dialog| dialog.selected)
    }

    pub(crate) fn move_dialog_selection(&mut self, delta: i32) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        dialog.selected = move_index(dialog.selected, delta, dialog.options.len());
        if dialog.purpose == DialogPurpose::AskUser {
            self.ask_user_option = dialog.selected;
        } else if dialog.purpose == DialogPurpose::WikiLinkChoice {
            self.wiki_link_index = dialog.selected;
        }
    }

    pub(crate) fn toggle_dialog_option(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        if let Some(checked) = dialog.checked.get_mut(dialog.selected) {
            *checked = !*checked;
        }
    }

    pub(crate) fn adjust_dialog_scroll(&mut self, delta: i32) {
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
            DialogPurpose::AgentApproval | DialogPurpose::AgentDestructiveApproval => {
                self.approval_scroll = dialog.scroll
            }
            _ => {}
        }
    }

    pub(crate) fn handle_select_or_input_dialog(&mut self, key: KeyEvent) -> Option<Command> {
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

    pub(crate) fn handle_text_dialog(&mut self, key: KeyEvent) -> Option<Command> {
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
                    Some(DialogPurpose::ExportDestination) => self.submit_export(),
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
                    Some(DialogPurpose::ExportDestination) => {}
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

    pub(in crate::app) fn edit_dialog_single_line(&mut self, key: KeyEvent) -> TextInputEdit {
        let Some(dialog) = self.dialog.as_mut() else {
            return TextInputEdit::Ignored;
        };
        let edit = edit_single_line(&mut dialog.input, &mut dialog.cursor, key);
        if edit.handled() {
            self.sync_dialog_owner_state();
        }
        edit
    }

    pub(crate) fn insert_dialog_char(&mut self, character: char) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        insert_char(&mut dialog.input, &mut dialog.cursor, character);
        self.sync_dialog_owner_state();
    }

    pub(crate) fn delete_dialog_backward(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        delete_backward(&mut dialog.input, &mut dialog.cursor);
        self.sync_dialog_owner_state();
    }

    pub(crate) fn delete_dialog_forward(&mut self) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        delete_forward(&mut dialog.input, &mut dialog.cursor);
        self.sync_dialog_owner_state();
    }

    pub(in crate::app) fn move_dialog_cursor(&mut self, movement: CursorMove) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        dialog.cursor = move_cursor(&dialog.input, dialog.cursor, movement);
        self.sync_dialog_owner_state();
    }

    pub(crate) fn select_custom_dialog_option(&mut self) {
        let count = self
            .dialog
            .as_ref()
            .map_or(0, |dialog| dialog.options.len());
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.selected = count;
        }
        self.ask_user_option = count;
    }

    pub(crate) fn sync_dialog_owner_state(&mut self) {
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
            DialogPurpose::AgentApproval | DialogPurpose::AgentDestructiveApproval => {
                self.approval_scroll = dialog.scroll
            }
            DialogPurpose::Help => self.help_scroll = dialog.scroll,
            DialogPurpose::WikiLinkChoice => self.wiki_link_index = dialog.selected,
            DialogPurpose::ExportDestination => {}
            _ => {}
        }
    }

    pub(crate) fn close_dialog(&mut self) {
        let purpose = self.dialog.as_ref().map(|dialog| dialog.purpose);
        self.overlay = None;
        self.dialog = None;
        if matches!(
            purpose,
            Some(DialogPurpose::ExportFormat | DialogPurpose::ExportDestination)
        ) {
            self.pending_export_source = None;
            self.pending_export_format = None;
            self.pending_export_destination = None;
            return;
        }
        if !self.export_in_progress
            && self.pending_export_source.is_some()
            && self.pending_export_format.is_some()
        {
            let destination = self.pending_export_destination.clone();
            self.open_export_destination_dialog_with_input(destination.as_deref());
        }
    }
}

/// One-line success notification for a finished export: the resolved target
/// plus a compact summary of any renderer diagnostics.
fn export_success_message(
    outcome: &ExportOutcome,
    format: ExportFormat,
    diagnostics: &[ExportDiagnostic],
) -> String {
    let mut message = format!(
        "Exported {} bytes as {} to {}",
        outcome.bytes,
        format.label(),
        outcome.destination.display()
    );
    if let Some(summary) = diagnostics_summary(diagnostics) {
        message.push(' ');
        message.push_str(&summary);
    }
    message
}

/// Compact summary of renderer diagnostics: `None` when there is nothing to
/// report, otherwise the first item plus the count of remaining items.
fn diagnostics_summary(diagnostics: &[ExportDiagnostic]) -> Option<String> {
    match diagnostics.split_first() {
        None => None,
        Some((first, rest)) => {
            if rest.is_empty() {
                Some(format!("· {first}"))
            } else {
                Some(format!("· {first} (+{} more)", rest.len()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(bytes: u64) -> ExportOutcome {
        ExportOutcome {
            destination: std::path::PathBuf::from("/tmp/out.html"),
            bytes,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn success_message_reports_target_and_summary() {
        let message = export_success_message(&outcome(42), ExportFormat::Html, &[]);
        assert_eq!(message, "Exported 42 bytes as HTML to /tmp/out.html");
    }

    #[test]
    fn diagnostics_summary_shows_first_item_and_remaining_count() {
        assert_eq!(diagnostics_summary(&[]), None);
        assert_eq!(
            diagnostics_summary(&[ExportDiagnostic::warning("image missing.png skipped")]),
            Some("· warning: image missing.png skipped".to_string())
        );
        assert_eq!(
            diagnostics_summary(&[
                ExportDiagnostic::warning("image a.png skipped"),
                ExportDiagnostic::warning("b"),
                ExportDiagnostic::warning("c"),
            ]),
            Some("· warning: image a.png skipped (+2 more)".to_string())
        );
    }
}
