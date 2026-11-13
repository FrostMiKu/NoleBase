//! Durable collaborator messages and reports, plus command acknowledgements.
use crate::core::provider::Message;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use uuid::Uuid;

pub(crate) const NAME_PATTERN: &str = "^[a-z][a-z0-9]*(-[a-z0-9]+)*$";
pub(crate) const MAX_NAME_LENGTH: usize = 100;

pub(crate) fn validate_name(name: &str) -> anyhow::Result<()> {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let valid = name.len() <= MAX_NAME_LENGTH
        && PATTERN
            .get_or_init(|| regex::Regex::new(NAME_PATTERN).expect("collaborator name pattern"))
            .is_match(name);
    anyhow::ensure!(valid, "invalid input for tool agents (name): use 1–100 lowercase letters, digits and single hyphens, starting with a letter (for example notes-scout)");
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CollaboratorAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CollaboratorStatus {
    Running,
    Stopping,
    Idle,
    Failed,
    Stopped,
    Interrupted,
}
impl CollaboratorStatus {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollaboratorMessage {
    pub sequence: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollaboratorReport {
    pub sequence: u64,
    pub generation: u64,
    pub through_message: u64,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollaboratorNote {
    pub id: Uuid,
    pub name: String,
    pub access: CollaboratorAccess,
    pub instructions: String,
    pub status: CollaboratorStatus,
    pub messages: Vec<Message>,
    pub inbox: VecDeque<CollaboratorMessage>,
    pub received_messages: u64,
    pub consumed_messages: u64,
    pub reports: Vec<CollaboratorReport>,
    pub error: Option<String>,
    pub generation: u64,
}
impl CollaboratorNote {
    pub fn summary(&self) -> CollaboratorSummary {
        CollaboratorSummary {
            id: self.id,
            name: self.name.clone(),
            access: self.access,
            status: self.status.clone(),
            generation: self.generation,
            result: self.reports.last().map(|report| report.text.clone()),
            report_count: self.reports.len(),
            error: self.error.clone(),
            received_messages: self.received_messages,
            consumed_messages: self.consumed_messages,
            pending_messages: self.inbox.len(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct CollaboratorSummary {
    pub id: Uuid,
    pub name: String,
    pub access: CollaboratorAccess,
    pub status: CollaboratorStatus,
    pub generation: u64,
    pub result: Option<String>,
    pub report_count: usize,
    pub error: Option<String>,
    /// Total accepted messages, including the initial assignment.
    pub received_messages: u64,
    /// Messages appended to the conversation at a round boundary.
    pub consumed_messages: u64,
    /// Queue length at the instant this snapshot was taken.
    pub pending_messages: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MessageReceipt {
    pub id: Uuid,
    pub sequence: u64,
    pub generation: u64,
    /// Queue length when the message was durably accepted.
    pub pending_messages_at_acceptance: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StopOutcome {
    Stopping,
    AlreadyStopping,
    AlreadyStopped,
    AlreadyIdle,
}
