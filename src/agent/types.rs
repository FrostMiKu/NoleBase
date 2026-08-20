//! Shared types for the agent runtime: events, approvals, and ask-user state.

use std::path::PathBuf;
use std::time::Duration;

use crate::agent_session::{AgentConversation, TokenUsage};
use crate::provider::{ImageBlock, Message, ToolResult};

/// How the agent runtime decides whether a proposed change needs user approval.
///
/// The mode is shared with the UI through an [`std::sync::Arc`]`<AtomicU8>`
/// using the stable codes from [`PermissionMode::code`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PermissionMode {
    /// Ask the user for every approval request.
    Approve = 0,
    /// Ask only for changes that touch paths outside the NOLE root.
    Auto = 1,
    /// Never ask; approve every request.
    Yolo = 2,
}

impl PermissionMode {
    /// Stable encoding for sharing the mode through an `Arc<AtomicU8>`.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// Stable, non-panicking decode of a stored code. Unknown values fall back
    /// to [`Self::Approve`].
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Approve,
            1 => Self::Auto,
            2 => Self::Yolo,
            _ => Self::Approve,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::Auto => "AUTO",
            Self::Yolo => "YOLO",
        }
    }

    /// Cycle through the modes in Tab order: APPROVE → AUTO → YOLO → APPROVE.
    pub fn cycled(self) -> Self {
        Self::from_code(self.code().wrapping_add(1))
    }
}

/// How an approval request is presented to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalKind {
    /// Show the request as a scrollable unified/side-by-side diff panel.
    Diff,
    /// Show a non-destructive action as a confirmation dialog.
    Confirm,
    /// Show an irreversible or data-removing action as an error-colored confirmation.
    DestructiveConfirm,
    /// Show an Agent-provided purpose and the exact command or PTY input.
    Command(CommandApproval),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandApproval {
    pub purpose: String,
    pub label: String,
    pub code: String,
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub title: String,
    /// Diff text for [`ApprovalKind::Diff`], or the confirmation body for
    /// [`ApprovalKind::Confirm`].
    pub message: String,
    pub kind: ApprovalKind,
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
    /// Streamed reasoning text (thinking blocks), distinct from the final reply.
    ThinkingDelta(String),
    /// The reasoning text finished streaming.
    ThinkingFinished,
    BufferedInputConsumed(usize),
    ToolStarted {
        id: String,
        message: String,
    },
    ToolFinished {
        id: String,
        message: String,
        /// Single-line human-readable result preview; `None` for failed or
        /// structured (JSON) results. Rendered by the wide Agent Chat view.
        preview: Option<String>,
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

/// A tool call outcome: the text result that feeds the conversation plus any
/// native image blocks the tool produced. Failure, denial, deferred, and
/// skipped outcomes always have empty images.
#[derive(Clone)]
pub(crate) struct ToolCallOutput {
    pub(crate) result: ToolResult,
    pub(crate) images: Vec<ImageBlock>,
}

impl ToolCallOutput {
    pub(crate) fn text(result: ToolResult) -> Self {
        Self {
            result,
            images: Vec::new(),
        }
    }

    pub(crate) fn with_images(result: ToolResult, images: Vec<ImageBlock>) -> Self {
        Self { result, images }
    }
}

pub(crate) enum ToolCallExecution {
    Completed(ToolCallOutput),
    Denied(ToolCallOutput),
}

pub(crate) enum ToolBatchExecution {
    Completed {
        messages: Vec<Message>,
        turn_boundary: bool,
        retry_after_error: bool,
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
