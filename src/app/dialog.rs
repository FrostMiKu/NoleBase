//! Modal dialog model: modes, purposes, options, and shared dialog state.

/// The interaction model used by every modal dialog in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    Confirm,
    SingleLine,
    SingleSelect,
    MultiSelect,
    SelectOrInput,
    FreeText,
    Approval,
    Informational,
    CommandPalette,
}

/// Business purpose of a dialog. The mode controls interaction while the
/// purpose controls the result that is sent to the owning subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogPurpose {
    DeleteDaily,
    DeleteFile,
    AgentPrompt,
    AgentApproval,
    AskUser,
    WikiLinkChoice,
    Help,
    NewFile,
    RenameFile,
    CommandPalette,
    ThemePicker,
    TagRenameSource,
    TagRenameTarget,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogOption {
    pub label: String,
    pub hint: Option<String>,
}

impl DialogOption {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: None,
        }
    }

    pub fn with_hint(label: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hint: Some(hint.into()),
        }
    }
}

/// State shared by confirmations, selectors, text prompts, approvals and
/// Agent questions. `options` can be used by both single- and multi-select
/// dialogs; `checked` stores the multi-select state by option index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogState {
    pub title: String,
    pub message: String,
    pub mode: DialogMode,
    pub purpose: DialogPurpose,
    pub options: Vec<DialogOption>,
    pub selected: usize,
    pub checked: Vec<bool>,
    pub input: String,
    pub cursor: usize,
    pub scroll: u16,
}

impl DialogState {
    pub fn new(
        title: impl Into<String>,
        message: impl Into<String>,
        mode: DialogMode,
        purpose: DialogPurpose,
        options: Vec<DialogOption>,
    ) -> Self {
        let checked = vec![false; options.len()];
        Self {
            title: title.into(),
            message: message.into(),
            mode,
            purpose,
            options,
            selected: 0,
            checked,
            input: String::new(),
            cursor: 0,
            scroll: 0,
        }
    }

    pub fn selected_option(&self) -> Option<&DialogOption> {
        self.options.get(self.selected)
    }

    pub fn selected_options(&self) -> Vec<String> {
        self.options
            .iter()
            .enumerate()
            .filter_map(|(index, option)| {
                self.checked
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                    .then_some(option.label.clone())
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogResult {
    Confirm(bool),
    Selected(String),
    SelectedMany(Vec<String>),
    Text(String),
    Cancelled,
}
