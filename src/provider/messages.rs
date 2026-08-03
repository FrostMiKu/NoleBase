use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::agent_session::TokenUsage;
use crate::observable::{BoxFuture, Observable};

use super::{
    parse_tool_input, transient_provider_error, AssistantMessage, Message, MessagePart,
    MessageRole, Provider, ProviderEvent, ProviderRequest, StopReason, ToolCall,
    DEFAULT_STREAM_BUFFER,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_HTTP_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub struct MessagesProvider {
    api_key: String,
    base_url: String,
    client: Client,
}

impl MessagesProvider {
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if base_url.ends_with("/v1") {
            bail!("base_url must not include /v1");
        }
        Ok(Self {
            api_key: api_key.into(),
            base_url,
            client: Client::builder()
                .timeout(Duration::from_secs(90))
                .user_agent(concat!("nole/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("building Messages HTTP client")?,
        })
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    fn count_url(&self) -> String {
        format!("{}/v1/messages/count_tokens", self.base_url)
    }

    fn body(request: &ProviderRequest, stream: bool) -> Value {
        json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "system": request.system.iter().map(|block| {
                let mut value = json!({"type": "text", "text": block.text});
                if block.cache {
                    value["cache_control"] = json!({"type": "ephemeral"});
                }
                value
            }).collect::<Vec<_>>(),
            "messages": messages_wire(&request.messages),
            "tools": request.tools.iter().map(|tool| {
                let mut value = json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                });
                if tool.cache {
                    value["cache_control"] = json!({"type": "ephemeral"});
                }
                value
            }).collect::<Vec<_>>(),
            "stream": stream,
        })
    }

    async fn send(
        &self,
        url: &str,
        body: &Value,
        events: Option<&broadcast::Sender<ProviderEvent>>,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        for attempt in 0..MAX_HTTP_ATTEMPTS {
            let request = self
                .client
                .post(url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(body);
            let sent = tokio::select! {
                _ = cancel.cancelled() => bail!("Messages request cancelled"),
                result = request.send() => result,
            };
            match sent {
                Ok(response)
                    if response.status().is_success()
                        || !retryable(response.status())
                        || attempt + 1 == MAX_HTTP_ATTEMPTS =>
                {
                    return Ok(response);
                }
                Ok(response) => {
                    let delay = retry_delay(attempt, response.headers());
                    if let Some(events) = events {
                        let _ = events.send(ProviderEvent::Retry);
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => bail!("Messages request cancelled"),
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                Err(error) if error.is_builder() => {
                    return Err(error).context("calling Messages API");
                }
                Err(error) if attempt + 1 == MAX_HTTP_ATTEMPTS => {
                    return Err(transient_provider_error(format!(
                        "calling Messages API: {error}"
                    )));
                }
                Err(_) => {
                    if let Some(events) = events {
                        let _ = events.send(ProviderEvent::Retry);
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => bail!("Messages request cancelled"),
                        _ = tokio::time::sleep(retry_delay(attempt, &reqwest::header::HeaderMap::new())) => {}
                    }
                }
            }
        }
        unreachable!()
    }

    async fn call_inner(
        &self,
        request: ProviderRequest,
        stream: bool,
        events: Option<broadcast::Sender<ProviderEvent>>,
        cancel: CancellationToken,
    ) -> Result<AssistantMessage> {
        let body = Self::body(&request, stream);
        let response = self
            .send(&self.messages_url(), &body, events.as_ref(), &cancel)
            .await?;
        let started = Instant::now();
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("unable to read error response: {error}"));
            let error = anyhow::anyhow!("Messages API returned {status}: {}", error_message(&body));
            return if retryable(status) {
                Err(transient_provider_error(error))
            } else {
                Err(error)
            };
        }
        let is_stream = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let decoded = if stream && is_stream {
            decode_stream(
                response,
                events.clone().expect("stream events"),
                cancel.clone(),
            )
            .await
        } else {
            let result = async {
                let value: Value = response
                    .json()
                    .await
                    .context("decoding Messages response")?;
                let duration = if stream {
                    Duration::ZERO
                } else {
                    started.elapsed()
                };
                let answer = parse_response(value, duration)?;
                if let Some(events) = events {
                    let text = answer.text();
                    if !text.is_empty() {
                        let _ = events.send(ProviderEvent::TextDelta(text));
                    }
                    if !answer.token_usage.is_empty() {
                        let _ = events.send(ProviderEvent::Usage {
                            usage: answer.token_usage,
                            generation_duration: answer.generation_duration,
                        });
                    }
                }
                Ok(answer)
            };
            result.await
        };
        match decoded {
            Err(error) if cancel.is_cancelled() => Err(error),
            Err(error) => Err(transient_provider_error(error)),
            result => result,
        }
    }
}

impl Provider for MessagesProvider {
    fn call<'a>(&'a self, request: ProviderRequest) -> BoxFuture<'a, AssistantMessage> {
        Box::pin(self.call_inner(request, false, None, CancellationToken::new()))
    }

    fn call_streaming(
        &self,
        request: ProviderRequest,
    ) -> Observable<AssistantMessage, ProviderEvent> {
        let provider = self.clone();
        let (tx, events) = broadcast::channel(DEFAULT_STREAM_BUFFER);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        Observable {
            output: Box::pin(async move {
                provider
                    .call_inner(request, true, Some(tx), task_cancel)
                    .await
            }),
            events,
            cancel,
        }
    }

    fn count_tokens<'a>(&'a self, request: ProviderRequest) -> BoxFuture<'a, Option<u64>> {
        Box::pin(async move {
            let body = json!({
                "model": request.model,
                "system": Self::body(&request, false)["system"],
                "messages": messages_wire(&request.messages),
                "tools": Self::body(&request, false)["tools"],
            });
            let cancel = CancellationToken::new();
            let response = self.send(&self.count_url(), &body, None, &cancel).await?;
            let status = response.status();
            let text = response
                .text()
                .await
                .context("reading Messages token count response")?;
            if matches!(status.as_u16(), 404 | 405 | 501) {
                return Ok(None);
            }
            if !status.is_success() {
                bail!(
                    "Messages token counting returned {status}: {}",
                    error_message(&text)
                );
            }
            Ok(serde_json::from_str::<Value>(&text)
                .context("decoding Messages token count response")?
                .get("input_tokens")
                .and_then(Value::as_u64))
        })
    }
}

async fn decode_stream(
    response: Response,
    events: broadcast::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<AssistantMessage> {
    let mut stream = response.bytes_stream().eventsource();
    let mut content = Vec::<Value>::new();
    let mut partial_inputs = HashMap::<usize, String>::new();
    let mut tool_input_errors = HashMap::new();
    let mut stop_reason = StopReason::Unknown("unknown".to_string());
    let mut usage = TokenUsage::default();
    let mut first_event = None::<Instant>;
    let mut stream_complete = false;
    let mut saw_text = false;

    loop {
        let item = tokio::select! {
            _ = cancel.cancelled() => bail!("Messages stream cancelled"),
            item = stream.next() => item,
        };
        let Some(item) = item else { break };
        let event = match item {
            Ok(event) => event,
            Err(error) => {
                if !usage.is_empty() {
                    emit_usage(&events, usage, first_event);
                }
                return Err(anyhow::anyhow!("reading Messages event stream: {error}"));
            }
        };
        if event.data.is_empty() {
            continue;
        }
        if event.data == "[DONE]" {
            stream_complete = true;
            break;
        }
        first_event.get_or_insert_with(Instant::now);
        let value: Value = serde_json::from_str(&event.data)
            .with_context(|| format!("decoding Messages stream event: {}", event.data))?;
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                update_usage_snapshot(&mut usage, value.pointer("/message/usage"));
                emit_usage(&events, usage, first_event);
            }
            Some("content_block_start") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if content.len() <= index {
                    content.resize(index + 1, Value::Null);
                }
                content[index] = value.get("content_block").cloned().unwrap_or(Value::Null);
                if content[index].get("type").and_then(Value::as_str) == Some("text") {
                    if saw_text {
                        let _ = events.send(ProviderEvent::TextDelta("\n".to_string()));
                    }
                    saw_text = true;
                    if let Some(text) = content[index].get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            let _ = events.send(ProviderEvent::TextDelta(text.to_string()));
                        }
                    }
                }
            }
            Some("content_block_delta") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if content.len() <= index {
                    content.resize(index + 1, Value::Null);
                }
                let delta = value.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            append_string(&mut content[index], "text", text);
                            let _ = events.send(ProviderEvent::TextDelta(text.to_string()));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            append_string(&mut content[index], "thinking", text);
                            let _ = events.send(ProviderEvent::ThinkingDelta(text.to_string()));
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(text) = delta.get("signature").and_then(Value::as_str) {
                            append_string(&mut content[index], "signature", text);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            partial_inputs.entry(index).or_default().push_str(partial);
                        }
                    }
                    _ => {}
                }
            }
            Some("content_block_stop") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(partial) = partial_inputs.remove(&index) {
                    if let Some(block) = content.get_mut(index) {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        block["input"] = parse_tool_input(&id, &partial, &mut tool_input_errors);
                    }
                }
                if content
                    .get(index)
                    .and_then(|block| block.get("type"))
                    .and_then(Value::as_str)
                    == Some("thinking")
                {
                    let _ = events.send(ProviderEvent::ThinkingFinished);
                }
            }
            Some("message_delta") => {
                stop_reason = parse_stop_reason(
                    value
                        .pointer("/delta/stop_reason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                );
                update_usage_snapshot(&mut usage, value.get("usage"));
                emit_usage(&events, usage, first_event);
            }
            Some("message_stop") => stream_complete = true,
            Some("error") => bail!("Messages stream error: {}", error_message(&event.data)),
            _ => {}
        }
    }
    if !stream_complete {
        bail!("Messages event stream ended before message_stop");
    }
    for (index, partial) in partial_inputs {
        if let Some(block) = content.get_mut(index) {
            let id = block
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            block["input"] = parse_tool_input(&id, &partial, &mut tool_input_errors);
        }
    }
    if !usage.is_empty() {
        emit_usage(&events, usage, first_event);
    }
    Ok(AssistantMessage {
        message: Message::assistant(content_to_parts(&content)?),
        stop_reason,
        token_usage: usage,
        generation_duration: first_event.map_or(Duration::ZERO, |start| start.elapsed()),
        tool_input_errors,
    })
}

fn parse_response(value: Value, duration: Duration) -> Result<AssistantMessage> {
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .context("Messages response has no content array")?;
    let usage = usage_value(value.get("usage"));
    Ok(AssistantMessage {
        message: Message::assistant(content_to_parts(content)?),
        stop_reason: parse_stop_reason(
            value
                .get("stop_reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
        ),
        token_usage: usage,
        generation_duration: duration,
        tool_input_errors: HashMap::new(),
    })
}

fn content_to_parts(content: &[Value]) -> Result<Vec<MessagePart>> {
    let mut parts = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(MessagePart::Text {
                text: block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            }),
            Some("thinking") => parts.push(MessagePart::Thinking {
                thinking: block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                signature: block
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
            Some("redacted_thinking") => parts.push(MessagePart::RedactedThinking {
                data: block
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            }),
            Some("tool_use") => parts.push(MessagePart::ToolUse(ToolCall {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                input: block.get("input").cloned().unwrap_or_else(|| json!({})),
            })),
            _ => {}
        }
    }
    Ok(parts)
}

fn messages_wire(messages: &[Message]) -> Vec<Value> {
    let mut output = Vec::<Value>::new();
    for message in messages {
        let (role, blocks) = match message.role {
            MessageRole::User => (
                "user",
                message.parts.iter().filter_map(|part| match part {
                    MessagePart::Text { text } => Some(json!({"type": "text", "text": text})),
                    _ => None,
                }).collect::<Vec<_>>(),
            ),
            MessageRole::Tool => (
                "user",
                message.parts.iter().filter_map(|part| match part {
                    MessagePart::ToolResult(result) => Some(json!({
                        "type": "tool_result",
                        "tool_use_id": result.tool_use_id,
                        "content": result.content,
                        "is_error": result.is_error,
                    })),
                    _ => None,
                }).collect::<Vec<_>>(),
            ),
            MessageRole::Assistant => (
                "assistant",
                message.parts.iter().map(|part| match part {
                    MessagePart::Text { text } => json!({"type": "text", "text": text}),
                    MessagePart::Thinking { thinking, signature } => json!({
                        "type": "thinking", "thinking": thinking, "signature": signature
                    }),
                    MessagePart::RedactedThinking { data } => {
                        json!({"type": "redacted_thinking", "data": data})
                    }
                    MessagePart::ToolUse(call) => json!({
                        "type": "tool_use", "id": call.id, "name": call.name, "input": call.input
                    }),
                    MessagePart::ToolResult(_) => Value::Null,
                }).filter(|value| !value.is_null()).collect::<Vec<_>>(),
            ),
        };
        if blocks.is_empty() {
            continue;
        }
        let merge = output
            .last()
            .and_then(|value| value.get("role"))
            .and_then(Value::as_str)
            == Some(role);
        if merge {
            if let Some(existing) = output
                .last_mut()
                .and_then(|value| value.get_mut("content"))
                .and_then(Value::as_array_mut)
            {
                existing.extend(blocks);
            }
        } else {
            output.push(json!({"role": role, "content": blocks}));
        }
    }
    if let Some(block) = output
        .iter_mut()
        .rev()
        .filter_map(|message| message.get_mut("content")?.as_array_mut())
        .find_map(|content| {
            content.iter_mut().rev().find(|block| {
                !matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("thinking" | "redacted_thinking")
                )
            })
        })
    {
        block["cache_control"] = json!({"type": "ephemeral"});
    }
    output
}

fn append_string(block: &mut Value, field: &str, delta: &str) {
    if let Some(object) = block.as_object_mut() {
        let current = object.get(field).and_then(Value::as_str).unwrap_or("");
        object.insert(
            field.to_string(),
            Value::String(format!("{current}{delta}")),
        );
    }
}

fn usage_value(value: Option<&Value>) -> TokenUsage {
    let mut usage = TokenUsage::default();
    update_usage_snapshot(&mut usage, value);
    usage
}

fn update_usage_snapshot(usage: &mut TokenUsage, value: Option<&Value>) {
    let Some(value) = value else { return };
    if let Some(tokens) = value.get("input_tokens").and_then(Value::as_u64) {
        usage.input_tokens = tokens;
    }
    if let Some(tokens) = value.get("output_tokens").and_then(Value::as_u64) {
        usage.output_tokens = tokens;
    }
    if let Some(tokens) = value
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
    {
        usage.cache_creation_input_tokens = tokens;
    }
    if let Some(tokens) = value.get("cache_read_input_tokens").and_then(Value::as_u64) {
        usage.cache_read_input_tokens = tokens;
    }
}

fn emit_usage(
    events: &broadcast::Sender<ProviderEvent>,
    usage: TokenUsage,
    first_event: Option<Instant>,
) {
    if !usage.is_empty() {
        let _ = events.send(ProviderEvent::Usage {
            usage,
            generation_duration: first_event.map_or(Duration::ZERO, |start| start.elapsed()),
        });
    }
}

fn parse_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::End,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::Length,
        "refusal" => StopReason::Refusal,
        other => StopReason::Unknown(other.to_string()),
    }
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")?
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.to_string())
}

fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429) || status.is_server_error()
}

fn retry_delay(attempt: usize, headers: &reqwest::header::HeaderMap) -> Duration {
    if let Some(seconds) = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        return Duration::from_secs(seconds.min(5));
    }
    let base = 500u64.saturating_mul(1u64 << attempt.min(3));
    Duration::from_millis(base.min(5_000))
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::provider::{
        Message, MessagePart, ProviderRequest, SystemBlock, ToolResult, ToolSpec,
    };

    fn cache_control_count(value: &Value) -> usize {
        match value {
            Value::Array(values) => values.iter().map(cache_control_count).sum(),
            Value::Object(values) => {
                usize::from(values.contains_key("cache_control"))
                    + values.values().map(cache_control_count).sum::<usize>()
            }
            _ => 0,
        }
    }

    #[test]
    fn request_uses_four_cache_breakpoints_and_caches_the_conversation_tail() {
        let request = ProviderRequest {
            model: "test-model".to_string(),
            max_tokens: 512,
            system: vec![
                SystemBlock {
                    text: "base".to_string(),
                    cache: true,
                },
                SystemBlock {
                    text: "project".to_string(),
                    cache: false,
                },
                SystemBlock {
                    text: "skills".to_string(),
                    cache: true,
                },
                SystemBlock {
                    text: "memory".to_string(),
                    cache: false,
                },
            ],
            messages: vec![Message::user("first"), Message::user("latest")],
            tools: vec![ToolSpec {
                name: "read".to_string(),
                description: "Read".to_string(),
                input_schema: json!({"type": "object"}),
                cache: true,
            }],
        };

        let body = MessagesProvider::body(&request, true);
        assert_eq!(cache_control_count(&body), 4);
        let messages = body["messages"].as_array().unwrap();
        let content = messages.last().unwrap()["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert!(content[0].get("cache_control").is_none());
        assert_eq!(content[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn merged_tool_results_cache_only_the_last_content_block() {
        let messages = messages_wire(&[
            Message::tool(ToolResult {
                tool_use_id: "one".to_string(),
                content: "first".to_string(),
                is_error: false,
            }),
            Message::tool(ToolResult {
                tool_use_id: "two".to_string(),
                content: "second".to_string(),
                is_error: false,
            }),
        ]);

        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert!(content[0].get("cache_control").is_none());
        assert_eq!(content[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn conversation_cache_skips_an_uncacheable_thinking_tail() {
        let messages = messages_wire(&[
            Message::user("stable request"),
            Message::assistant(vec![MessagePart::Thinking {
                thinking: "private reasoning".to_string(),
                signature: Some("signature".to_string()),
            }]),
        ]);

        let user = messages[0]["content"].as_array().unwrap();
        let assistant = messages[1]["content"].as_array().unwrap();
        assert_eq!(user[0]["cache_control"]["type"], "ephemeral");
        assert!(assistant[0].get("cache_control").is_none());
    }
}
