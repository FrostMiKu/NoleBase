//! Shared types for the agent runtime: events, approvals, and ask-user state.

use std::path::PathBuf;
use std::time::Duration;

use crate::agent_session::{AgentConversation, TokenUsage};
use crate::provider::{Message, ToolResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    Approve,
    Bypass,
}

impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::Bypass => "BYPASS",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Approve => Self::Bypass,
            Self::Bypass => Self::Approve,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub title: String,
    pub diff: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

#[derive(Clone, Debug)]
pub enum AgentEvent {
    AssistantDelta(String),
    AssistantMessageFinished {
        text: String,
        final_output: bool,
    },
    BufferedInputConsumed(usize),
    ToolStarted {
        id: String,
        message: String,
    },
    ToolFinished {
        id: String,
        message: String,
    },
    Usage(TokenUsage),
    ContextWindow {
        tokens: u64,
        capacity: u64,
    },
    ResponseTiming {
        output_tokens: u64,
        elapsed: Duration,
    },
    Retry,
    Round {
        current: u32,
        limit: u32,
    },
    ConversationUpdated(AgentConversation),
    Notification(String),
    FileMoved {
        from: PathBuf,
        to: PathBuf,
    },
    OpenFile(PathBuf),
    Approval(ApprovalRequest),
    AskUser(AskUserRequest),
    Stopped(AgentStopReason),
    Finished(Result<String, String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStopReason {
    RequestRoundLimit,
    ToolApprovalDenied,
}

pub(crate) const AGENT_STREAM_BUFFER: usize = 16_384;
pub(crate) type AgentEventSender = tokio::sync::broadcast::Sender<AgentEvent>;
#[derive(Clone, Debug)]
pub struct AgentRunOutput {
    pub text: String,
    pub conversation: AgentConversation,
}

pub(crate) enum AgentRunCompletion {
    Finished(String),
    Stopped(AgentStopReason),
}

#[derive(Debug)]
pub(crate) struct ApprovalDenied;

impl std::fmt::Display for ApprovalDenied {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("change denied by user")
    }
}

impl std::error::Error for ApprovalDenied {}

pub(crate) enum ToolCallExecution {
    Completed(ToolResult),
    Denied(ToolResult),
}

pub(crate) enum ToolBatchExecution {
    Completed {
        messages: Vec<Message>,
        turn_boundary: bool,
    },
    Denied(Vec<Message>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolExecutionPolicy {
    Exclusive,
    LocalRead,
    Network,
    Subagent,
}

impl ToolExecutionPolicy {
    pub(crate) fn is_concurrent(self) -> bool {
        self != Self::Exclusive
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskUserRequest {
    pub kind: AskUserKind,
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskUserKind {
    Tool,
    RoundLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskUserResponse {
    Answer(String),
    Cancelled,
}
