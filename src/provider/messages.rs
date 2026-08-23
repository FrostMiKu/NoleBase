use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::{Client, Response};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::agent_session::TokenUsage;
use crate::observable::{BoxFuture, Observable};

use super::{
    build_agent_http_client, emit_usage, error_message, image_bytes, normalize_provider_base_url,
    parse_tool_input, retry_delay, retryable, transient_provider_error, AssistantMessage,
    ImageBlock, Message, MessagePart, MessageRole, Provider, ProviderEvent, ProviderRequest,
    StopReason, ToolCall, DEFAULT_STREAM_BUFFER,
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
        let base_url = normalize_provider_base_url(base_url)?;
        Ok(Self {
            api_key: api_key.into(),
            base_url,
            client: build_agent_http_client().context("building Messages HTTP client")?,
        })
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    fn count_url(&self) -> String {
        format!("{}/v1/messages/count_tokens", self.base_url)
    }

    fn body(request: &ProviderRequest, stream: bool) -> Result<Value> {
        let mut body = json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "system": request.system.iter().map(|block| {
                let mut value = json!({"type": "text", "text": block.text});
                if block.cache {
                    value["cache_control"] = json!({"type": "ephemeral"});
                }
                value
            }).collect::<Vec<_>>(),
            "messages": messages_wire(&request.messages)?,
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
        });
        if let Some(effort) = request.effort {
            body["output_config"] = json!({"effort": effort});
        }
        Ok(body)
    }

    async fn send(
        &self,
        url: &str,
        body: &Value,
        events: Option<&broadcast::Sender<ProviderEvent>>,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        for attempt in 0..MAX_HTTP_ATTEMPTS {
            let mut request = self
                .client
                .post(url)
                .header("anthropic-version", ANTHROPIC_VERSION);
            // Anthropic accepts both credential headers; vLLM-style gateways
            // only authenticate `Authorization: Bearer`, so send both.
            if !self.api_key.trim().is_empty() {
                request = request
                    .header("x-api-key", &self.api_key)
                    .bearer_auth(&self.api_key);
            }
            let request = request.json(body);
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
        let body = Self::body(&request, stream)?;
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
            let body = Self::body(&request, false)?;
            let request_body = json!({
                "model": request.model,
                "system": body["system"],
                "messages": body["messages"],
                "tools": body["tools"],
            });
            let cancel = CancellationToken::new();
            let response = self
                .send(&self.count_url(), &request_body, None, &cancel)
                .await?;
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
                if content[index].get("type").and_then(Value::as_str) == Some("tool_use") {
                    let _ = events.send(ProviderEvent::ToolCallDelta {
                        index,
                        name: content[index]
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        arguments: String::new(),
                    });
                    // Let the Agent consume the preparation event before this
                    // decoder drains more already-buffered SSE events.
                    tokio::task::yield_now().await;
                }
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
                            let _ = events.send(ProviderEvent::ToolCallDelta {
                                index,
                                name: String::new(),
                                arguments: partial.to_string(),
                            });
                            tokio::task::yield_now().await;
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
                if let Some(block) = content
                    .get(index)
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                {
                    let _ = events.send(ProviderEvent::ToolCallFinished {
                        index,
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    });
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

/// Encode a single image block as an Anthropic base64 content block. The
/// pixels must already be resolved; a missing cache is a local mapping error.
fn image_block_wire(block: &ImageBlock) -> Result<Value> {
    let bytes = image_bytes(block)?;
    Ok(json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": block.media_type.mime(),
            "data": base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }))
}

fn messages_wire(messages: &[Message]) -> Result<Vec<Value>> {
    let mut output = Vec::<Value>::new();
    for message in messages {
        let (role, blocks) = match message.role {
            MessageRole::User => (
                "user",
                message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        MessagePart::Text { text } => {
                            Some(Ok(json!({"type": "text", "text": text})))
                        }
                        MessagePart::Image(block) => Some(image_block_wire(block)),
                        _ => None,
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
            MessageRole::Tool => (
                "user",
                message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        MessagePart::ToolResult(result) => Some(Ok(json!({
                            "type": "tool_result",
                            "tool_use_id": result.tool_use_id,
                            "content": result.content,
                            "is_error": result.is_error,
                        }))),
                        // Tool messages never carry pixels (images are moved to
                        // a trailing user message), so an image here is a
                        // protocol violation and must surface, not drop.
                        MessagePart::Image(block) => Some(Err(anyhow::anyhow!(
                            "tool message for {} contains an image; images must be sent as a user message",
                            block.label
                        ))),
                        _ => None,
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
            MessageRole::Assistant => {
                let has_output = message.parts.iter().any(|part| match part {
                    MessagePart::Text { text } => !text.trim().is_empty(),
                    MessagePart::ToolUse(_) => true,
                    _ => false,
                });
                (
                    "assistant",
                    message.parts.iter().filter_map(|part| {
                        if !has_output {
                            return None;
                        }
                        match part {
                            MessagePart::Text { text } if !text.trim().is_empty() => {
                                Some(Ok(json!({"type": "text", "text": text})))
                            }
                            MessagePart::Text { .. } => None,
                            MessagePart::Thinking { thinking, signature } => Some(Ok(json!({
                                "type": "thinking", "thinking": thinking, "signature": signature
                            }))),
                            MessagePart::RedactedThinking { data } => Some(Ok(json!({
                                "type": "redacted_thinking", "data": data
                            }))),
                            MessagePart::ToolUse(call) => Some(Ok(json!({
                                "type": "tool_use", "id": call.id, "name": call.name, "input": call.input
                            }))),
                            MessagePart::ToolResult(_) => None,
                            MessagePart::Image(block) => Some(Err(anyhow::anyhow!(
                                "assistant message for {} contains an image; images are only valid in user messages",
                                block.label
                            ))),
                        }
                    }).collect::<Result<Vec<_>>>()?,
                )
            }
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
    Ok(output)
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

fn parse_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" => StopReason::End,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::Length,
        "refusal" => StopReason::Refusal,
        other => StopReason::Unknown(other.to_string()),
    }
}

#[cfg(test)]
mod streaming_tests {
    use std::io::{BufRead, BufReader, Read, Write};

    use super::*;
    use crate::provider::{ProviderRequest, SystemBlock};

    #[tokio::test(flavor = "current_thread")]
    async fn streaming_forwards_tool_name_and_input_json_deltas() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap();
                }
            }
            let mut request_body = vec![0; content_length];
            reader.read_exact(&mut request_body).unwrap();
            let body = concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_write\",\"name\":\"write\",\"input\":{}}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"notes/design.md\\\",\\\"content\\\":\\\"hello\\\"}\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            );
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            reader.get_mut().flush().unwrap();
        });

        let provider = MessagesProvider::new("secret", format!("http://{address}")).unwrap();
        let observable = provider.call_streaming(ProviderRequest {
            model: "test-model".to_string(),
            max_tokens: 128,
            effort: None,
            system: vec![SystemBlock {
                text: "test".to_string(),
                cache: false,
            }],
            messages: vec![Message::user("write a note")],
            tools: Vec::new(),
        });
        let mut live_events = observable.events;
        let mut events = live_events.resubscribe();
        let mut output = observable.output;
        let live_arguments = loop {
            tokio::select! {
                biased;
                result = &mut output => panic!("provider completed before exposing live tool arguments: {result:?}"),
                event = live_events.recv() => {
                    if let ProviderEvent::ToolCallDelta { arguments, .. } = event.unwrap() {
                        if !arguments.is_empty() {
                            break arguments;
                        }
                    }
                }
            }
        };
        assert_eq!(
            live_arguments,
            r#"{"path":"notes/design.md","content":"hello"}"#
        );
        let answer = output.await.unwrap();
        server.join().unwrap();

        let call = answer.message.tool_calls().next().unwrap();
        assert_eq!(call.id, "call_write");
        assert_eq!(
            call.input,
            json!({"path": "notes/design.md", "content": "hello"})
        );
        assert!(matches!(
            events.try_recv().unwrap(),
            ProviderEvent::ToolCallDelta { index: 0, name, arguments }
                if name == "write" && arguments.is_empty()
        ));
        assert!(matches!(
            events.try_recv().unwrap(),
            ProviderEvent::ToolCallDelta { index: 0, name, arguments }
                if name.is_empty()
                    && arguments == r#"{"path":"notes/design.md","content":"hello"}"#
        ));
        assert!(matches!(
            events.try_recv().unwrap(),
            ProviderEvent::ToolCallFinished { index: 0, id } if id == "call_write"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn requests_carry_both_credential_headers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut headers = String::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap();
                }
                headers.push_str(&line);
            }
            let mut request_body = vec![0; content_length];
            reader.read_exact(&mut request_body).unwrap();
            let body = r#"{"content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            reader.get_mut().flush().unwrap();
            (
                headers,
                serde_json::from_slice::<Value>(&request_body).unwrap(),
            )
        });

        let provider = MessagesProvider::new("secret", format!("http://{address}")).unwrap();
        let observable = provider.call_streaming(ProviderRequest {
            model: "test-model".to_string(),
            max_tokens: 128,
            effort: None,
            system: vec![SystemBlock {
                text: "test".to_string(),
                cache: false,
            }],
            messages: vec![Message::user("write a note")],
            tools: Vec::new(),
        });
        let answer = observable.output.await.unwrap();
        let (headers, _body) = server.join().unwrap();

        assert_eq!(answer.text(), "hello");
        let headers = headers.to_ascii_lowercase();
        assert!(headers.contains("x-api-key: secret"));
        // vLLM-style gateways authenticate only via the Bearer header.
        assert!(headers.contains("authorization: bearer secret"));
        assert!(headers.contains("anthropic-version: 2023-06-01"));
    }
}

#[cfg(test)]
mod cache_tests {
    use std::sync::Arc;

    use super::*;
    use crate::provider::{
        ImageBlock, ImageMediaType, ImageSource, Message, MessagePart, ProviderRequest,
        SystemBlock, ToolResult, ToolSpec,
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
            effort: None,
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

        let body = MessagesProvider::body(&request, true).unwrap();
        assert_eq!(cache_control_count(&body), 4);
        let messages = body["messages"].as_array().unwrap();
        let content = messages.last().unwrap()["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert!(content[0].get("cache_control").is_none());
        assert_eq!(content[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn body_encodes_effort_as_output_config_and_omits_it_when_unset() {
        let mut request = ProviderRequest {
            model: "test-model".to_string(),
            max_tokens: 512,
            effort: Some(crate::provider::ReasoningEffort::Xhigh),
            system: Vec::new(),
            messages: vec![Message::user("hello")],
            tools: Vec::new(),
        };
        let body = MessagesProvider::body(&request, false).unwrap();
        assert_eq!(body["output_config"]["effort"], "xhigh");

        request.effort = None;
        let body = MessagesProvider::body(&request, false).unwrap();
        assert!(body.get("output_config").is_none());
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
        ])
        .unwrap();

        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert!(content[0].get("cache_control").is_none());
        assert_eq!(content[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn thinking_only_assistant_is_not_replayed_as_an_invalid_message() {
        let messages = messages_wire(&[
            Message::user("original request"),
            Message::assistant(vec![MessagePart::Thinking {
                thinking: "unfinished private reasoning".to_string(),
                signature: None,
            }]),
            Message::user("Provide a non-empty final answer"),
            Message::user("continue"),
        ])
        .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["text"], "original request");
        assert_eq!(content[1]["text"], "Provide a non-empty final answer");
        assert_eq!(content[2]["text"], "continue");
    }

    #[test]
    fn thinking_is_preserved_when_assistant_has_a_tool_call() {
        let messages = messages_wire(&[
            Message::user("research this"),
            Message::assistant(vec![
                MessagePart::Thinking {
                    thinking: "research plan".to_string(),
                    signature: Some("signature".to_string()),
                },
                MessagePart::ToolUse(ToolCall {
                    id: "search-1".to_string(),
                    name: "search_web".to_string(),
                    input: json!({"query": "example"}),
                }),
            ]),
        ])
        .unwrap();

        assert_eq!(messages.len(), 2);
        let assistant = messages[1]["content"].as_array().unwrap();
        assert_eq!(assistant[0]["type"], "thinking");
        assert_eq!(assistant[1]["type"], "tool_use");
    }

    #[test]
    fn messages_wire_encodes_prompt_and_tool_images() {
        let png: Vec<u8> = {
            let image = image::DynamicImage::new_rgb8(8, 4);
            let mut out = std::io::Cursor::new(Vec::new());
            image.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let prompt_image = Message::user_parts(vec![
            MessagePart::Text {
                text: "here".to_string(),
            },
            MessagePart::Image(ImageBlock {
                source: ImageSource::Attachment {
                    uri: "nole://attachment/00000000-0000-4000-8000-000000000000".to_string(),
                },
                label: "prompt.png".to_string(),
                media_type: ImageMediaType::Png,
                width: 8,
                height: 4,
                bytes: Some(Arc::from(png.clone())),
            }),
        ]);
        let tool = Message::tool(ToolResult {
            tool_use_id: "call-1".to_string(),
            content: "done".to_string(),
            is_error: false,
        });

        let wire = messages_wire(&[tool, prompt_image]).unwrap();
        let user = wire
            .iter()
            .find(|message| {
                message["content"]
                    .as_array()
                    .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "image"))
            })
            .expect("user image message present");
        let blocks = user["content"].as_array().unwrap();
        // tool_result blocks come before the image in the merged user message.
        let types = blocks
            .iter()
            .map(|b| b["type"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(types, ["tool_result", "text", "image"]);
        let image_block = blocks.iter().find(|b| b["type"] == "image").unwrap();
        assert_eq!(image_block["source"]["type"], "base64");
        assert_eq!(image_block["source"]["media_type"], "image/png");
        assert_eq!(
            image_block["source"]["data"],
            base64::engine::general_purpose::STANDARD.encode(&png)
        );

        // Missing bytes is a local mapping error, never a silent drop.
        let broken = Message::user_parts(vec![MessagePart::Image(ImageBlock {
            source: ImageSource::Url {
                url: "https://example.com/x.png".to_string(),
            },
            label: "stale.png".to_string(),
            media_type: ImageMediaType::Png,
            width: 8,
            height: 4,
            bytes: None,
        })]);
        assert!(messages_wire(&[broken]).is_err());
    }
}
