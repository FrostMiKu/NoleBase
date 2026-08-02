use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_session::TokenUsage;
use crate::observable::{BoxFuture, Observable};

pub mod completions;
pub mod messages;

pub const DEFAULT_STREAM_BUFFER: usize = 1_024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    Messages,
    Completions,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            parts: vec![MessagePart::Text { text: text.into() }],
        }
    }

    pub fn assistant(parts: Vec<MessagePart>) -> Self {
        Self {
            role: MessageRole::Assistant,
            parts,
        }
    }

    pub fn tool(result: ToolResult) -> Self {
        Self {
            role: MessageRole::Tool,
            parts: vec![MessagePart::ToolResult(result)],
        }
    }

    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.parts.iter().filter_map(|part| match part {
            MessagePart::ToolUse(call) => Some(call),
            _ => None,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePart {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse(ToolCall),
    ToolResult(ToolResult),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SystemBlock {
    pub text: String,
    pub cache: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub cache: bool,
}

#[derive(Clone, Debug)]
pub struct ProviderRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    End,
    ToolUse,
    Length,
    Refusal,
    Unknown(String),
}

#[derive(Clone, Debug)]
pub struct AssistantMessage {
    pub message: Message,
    pub stop_reason: StopReason,
    pub token_usage: TokenUsage,
    pub generation_duration: std::time::Duration,
    pub tool_input_errors: HashMap<String, String>,
}

impl AssistantMessage {
    pub fn text(&self) -> String {
        self.message.text()
    }
}

#[derive(Clone, Debug)]
pub enum ProviderEvent {
    TextDelta(String),
    /// Streamed reasoning block text (Anthropic `thinking` blocks), kept
    /// separate from the final reply so the UI can render it as process.
    ThinkingDelta(String),
    /// The reasoning block finished streaming.
    ThinkingFinished,
    Usage {
        usage: TokenUsage,
        generation_duration: std::time::Duration,
    },
    Retry,
}

pub trait Provider: Send + Sync {
    fn call<'a>(&'a self, request: ProviderRequest) -> BoxFuture<'a, AssistantMessage>;
    fn call_streaming(
        &self,
        request: ProviderRequest,
    ) -> Observable<AssistantMessage, ProviderEvent>;
    fn count_tokens<'a>(&'a self, request: ProviderRequest) -> BoxFuture<'a, Option<u64>>;
}

pub(crate) fn tool_input_error(reason: impl std::fmt::Display) -> String {
    format!(
        "Tool input was not executed because the streamed JSON was invalid: {reason}. Retry the tool with one complete JSON object."
    )
}

pub(crate) fn parse_tool_input(
    id: &str,
    input: &str,
    errors: &mut HashMap<String, String>,
) -> Value {
    match serde_json::from_str::<Value>(input) {
        Ok(value) if value.is_object() => value,
        Ok(_) => {
            errors.insert(
                id.to_string(),
                tool_input_error("tool input must be a JSON object"),
            );
            serde_json::json!({})
        }
        Err(error) => {
            errors.insert(
                id.to_string(),
                tool_input_error(format!("invalid JSON ({error})")),
            );
            serde_json::json!({})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Provider>() {}

    #[test]
    fn provider_implementations_are_send_sync_and_object_safe() {
        assert_send_sync::<messages::MessagesProvider>();
        assert_send_sync::<completions::CompletionsProvider>();

        let messages = messages::MessagesProvider::new("key", "https://example.com").unwrap();
        let completions =
            completions::CompletionsProvider::new("key", "https://example.com").unwrap();
        let providers: [&dyn Provider; 2] = [&messages, &completions];
        assert_eq!(providers.len(), 2);
    }
}
