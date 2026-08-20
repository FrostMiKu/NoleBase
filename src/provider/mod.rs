use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_session::TokenUsage;
use crate::observable::{BoxFuture, Observable};

pub mod completions;
pub mod messages;

pub const DEFAULT_STREAM_BUFFER: usize = 1_024;
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

pub(crate) fn build_agent_http_client() -> reqwest::Result<Client> {
    build_agent_http_client_with_timeouts(HTTP_CONNECT_TIMEOUT, HTTP_READ_IDLE_TIMEOUT)
}

fn build_agent_http_client_with_timeouts(
    connect_timeout: Duration,
    read_timeout: Duration,
) -> reqwest::Result<Client> {
    Client::builder()
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
        .user_agent(concat!("nole/", env!("CARGO_PKG_VERSION")))
        .build()
}

#[derive(Debug)]
struct TransientProviderError(String);

impl std::fmt::Display for TransientProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TransientProviderError {}

pub(crate) fn transient_provider_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(TransientProviderError(error.to_string()))
}

pub(crate) fn is_transient_provider_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TransientProviderError>().is_some()
}

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

    /// A user message from an explicit part list, preserving exact ordering
    /// (for example interleaved text and embedded image parts).
    pub fn user_parts(parts: Vec<MessagePart>) -> Self {
        Self {
            role: MessageRole::User,
            parts,
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
    Image(ImageBlock),
}

/// Media types that the agent may send to a model as native image input.
/// GIF is normalized to PNG during validation, so the wire types never carry
/// GIF; this is the single MIME source for both providers.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageMediaType {
    Jpeg,
    Png,
    Webp,
}

impl ImageMediaType {
    pub fn mime(self) -> &'static str {
        match self {
            ImageMediaType::Jpeg => "image/jpeg",
            ImageMediaType::Png => "image/png",
            ImageMediaType::Webp => "image/webp",
        }
    }
}

/// A persistent weak reference to image content. This source metadata is stored
/// on disk so pixels can be re-read after session restore; raw bytes remain an
/// in-process cache on [`ImageBlock`] and are never serialized.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImageSource {
    Attachment { uri: String },
    LocalFile { path: PathBuf },
    Url { url: String },
}

/// A pixel-carrying image content block for user messages and tool output.
/// `bytes` is skipped by serde so sessions never persist base64 or pixels, and
/// clones share the underlying pixels through `Arc` without copying.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageBlock {
    pub source: ImageSource,
    pub label: String,
    pub media_type: ImageMediaType,
    pub width: u32,
    pub height: u32,
    #[serde(skip, default)]
    pub bytes: Option<Arc<[u8]>>,
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
    use std::io::{BufRead, BufReader, Write};

    use futures_util::StreamExt;

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

    #[tokio::test(flavor = "current_thread")]
    async fn read_timeout_resets_while_response_body_keeps_arriving() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut stream = BufReader::new(stream);
            loop {
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            write!(
                stream.get_mut(),
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.get_mut().flush().unwrap();
            for chunk in [b"one".as_slice(), b"two", b"three"] {
                std::thread::sleep(Duration::from_millis(450));
                write!(stream.get_mut(), "{:X}\r\n", chunk.len()).unwrap();
                stream.get_mut().write_all(chunk).unwrap();
                stream.get_mut().write_all(b"\r\n").unwrap();
                stream.get_mut().flush().unwrap();
            }
            stream.get_mut().write_all(b"0\r\n\r\n").unwrap();
            stream.get_mut().flush().unwrap();
        });

        let client =
            build_agent_http_client_with_timeouts(Duration::from_secs(1), Duration::from_secs(1))
                .unwrap();
        let response = client
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap();
        let mut body = response.bytes_stream();
        let mut received = Vec::new();
        while let Some(chunk) = body.next().await {
            received.extend_from_slice(&chunk.unwrap());
        }
        server.join().unwrap();

        assert_eq!(received, b"onetwothree");
    }
}
