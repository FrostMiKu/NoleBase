use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::{Client, Response};
use serde_json::{json, Map, Value};
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

const MAX_HTTP_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub struct CompletionsProvider {
    api_key: String,
    base_url: String,
    client: Client,
}

impl CompletionsProvider {
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Result<Self> {
        let base_url = normalize_provider_base_url(base_url)?;
        Ok(Self {
            api_key: api_key.into(),
            base_url,
            client: build_agent_http_client().context("building Completions HTTP client")?,
        })
    }

    fn url(&self) -> String {
        format!("{}/v1/chat/completions", self.base_url)
    }

    fn body(request: &ProviderRequest, stream: bool) -> Result<Value> {
        let mut messages = Vec::new();
        if !request.system.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": request.system.iter().map(|block| block.text.as_str()).collect::<Vec<_>>().join("\n\n"),
            }));
        }
        for message in &request.messages {
            messages.extend(message_wire(message)?);
        }
        let mut body = Map::new();
        body.insert("model".to_string(), json!(request.model));
        body.insert("max_tokens".to_string(), json!(request.max_tokens));
        body.insert("messages".to_string(), Value::Array(messages));
        body.insert("stream".to_string(), Value::Bool(stream));
        if stream {
            body.insert("stream_options".to_string(), json!({"include_usage": true}));
        }
        if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(
                    request
                        .tools
                        .iter()
                        .map(|tool| {
                            json!({
                                "type": "function",
                                "function": {
                                    "name": tool.name,
                                    "description": tool.description,
                                    "parameters": tool.input_schema,
                                }
                            })
                        })
                        .collect(),
                ),
            );
        }
        Ok(Value::Object(body))
    }

    async fn send(
        &self,
        body: &Value,
        events: Option<&broadcast::Sender<ProviderEvent>>,
        cancel: &CancellationToken,
    ) -> Result<Response> {
        for attempt in 0..MAX_HTTP_ATTEMPTS {
            let mut request = self.client.post(self.url()).json(body);
            if !self.api_key.trim().is_empty() {
                request = request.bearer_auth(&self.api_key);
            }
            let sent = tokio::select! {
                _ = cancel.cancelled() => bail!("Completions request cancelled"),
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
                    if let Some(events) = events {
                        let _ = events.send(ProviderEvent::Retry);
                    }
                    let delay = retry_delay(attempt, response.headers());
                    tokio::select! {
                        _ = cancel.cancelled() => bail!("Completions request cancelled"),
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                Err(error) if error.is_builder() => {
                    return Err(error).context("calling Completions API");
                }
                Err(error) if attempt + 1 == MAX_HTTP_ATTEMPTS => {
                    return Err(transient_provider_error(format!(
                        "calling Completions API: {error}"
                    )));
                }
                Err(_) => {
                    if let Some(events) = events {
                        let _ = events.send(ProviderEvent::Retry);
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => bail!("Completions request cancelled"),
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
        let response = self.send(&body, events.as_ref(), &cancel).await?;
        let started = Instant::now();
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("unable to read error response: {error}"));
            let error = anyhow::anyhow!(
                "Completions API returned {status}: {}",
                error_message(&body)
            );
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
                    .context("decoding Completions response")?;
                let duration = if stream {
                    Duration::ZERO
                } else {
                    started.elapsed()
                };
                let answer = parse_response(value, duration)?;
                if let Some(events) = events {
                    for part in &answer.message.parts {
                        match part {
                            MessagePart::Thinking { thinking, .. } if !thinking.is_empty() => {
                                let _ = events.send(ProviderEvent::ThinkingDelta(thinking.clone()));
                                let _ = events.send(ProviderEvent::ThinkingFinished);
                            }
                            MessagePart::Text { text } if !text.is_empty() => {
                                let _ = events.send(ProviderEvent::TextDelta(text.clone()));
                            }
                            _ => {}
                        }
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

impl Provider for CompletionsProvider {
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

    fn count_tokens<'a>(&'a self, _request: ProviderRequest) -> BoxFuture<'a, Option<u64>> {
        Box::pin(async { Ok(None) })
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

async fn decode_stream(
    response: Response,
    events: broadcast::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<AssistantMessage> {
    let mut stream = response.bytes_stream().eventsource();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut thinking_open = false;
    let mut tools = BTreeMap::<usize, PartialToolCall>::new();
    let mut usage = TokenUsage::default();
    let mut stop_reason = StopReason::Unknown("unknown".to_string());
    let mut stream_complete = false;
    let mut first_event = None::<Instant>;

    loop {
        let item = tokio::select! {
            _ = cancel.cancelled() => bail!("Completions stream cancelled"),
            item = stream.next() => item,
        };
        let Some(item) = item else { break };
        let event = match item {
            Ok(event) => event,
            Err(error) => {
                emit_usage(&events, usage, first_event);
                return Err(anyhow::anyhow!("reading Completions event stream: {error}"));
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
            .with_context(|| format!("decoding Completions stream event: {}", event.data))?;
        if let Some(error) = value.pointer("/error/message").and_then(Value::as_str) {
            bail!("Completions stream error: {error}");
        }
        if let Some(raw_usage) = value.get("usage") {
            if !raw_usage.is_null() {
                usage = parse_usage(Some(raw_usage));
                emit_usage(&events, usage, first_event);
            }
        }
        let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            continue;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            stop_reason = parse_stop_reason(reason);
        }
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        // DeepSeek-style reasoning streams under `reasoning_content`; keep it
        // separate from the final reply exactly like Anthropic thinking blocks.
        if let Some(part) = delta
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|part| !part.is_empty())
        {
            thinking.push_str(part);
            thinking_open = true;
            let _ = events.send(ProviderEvent::ThinkingDelta(part.to_string()));
        }
        if let Some(part) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|part| !part.is_empty())
        {
            if thinking_open {
                let _ = events.send(ProviderEvent::ThinkingFinished);
                thinking_open = false;
            }
            text.push_str(part);
            let _ = events.send(ProviderEvent::TextDelta(part.to_string()));
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in tool_calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let partial = tools.entry(index).or_default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    partial.id.push_str(id);
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    partial.name.push_str(name);
                }
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                {
                    partial.arguments.push_str(arguments);
                }
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let arguments = call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !name.is_empty() || !arguments.is_empty() {
                    let _ = events.send(ProviderEvent::ToolCallDelta {
                        index,
                        name: name.to_string(),
                        arguments: arguments.to_string(),
                    });
                    // `decode_stream` and its event receiver are driven by the
                    // same outer `select`. A response chunk can contain many
                    // already-buffered SSE events, so yield after each tool
                    // fragment or the receiver may not run until the complete
                    // call has been assembled.
                    tokio::task::yield_now().await;
                }
            }
        }
    }
    if !stream_complete && matches!(stop_reason, StopReason::Unknown(_)) {
        bail!("Completions event stream ended before a finish reason or [DONE]");
    }
    if thinking_open {
        let _ = events.send(ProviderEvent::ThinkingFinished);
    }
    emit_usage(&events, usage, first_event);
    let mut parts = Vec::new();
    if !thinking.is_empty() {
        parts.push(MessagePart::Thinking {
            thinking,
            signature: None,
        });
    }
    if !text.is_empty() {
        parts.push(MessagePart::Text { text });
    }
    let mut errors = HashMap::new();
    for (index, tool) in tools {
        let _ = events.send(ProviderEvent::ToolCallFinished {
            index,
            id: tool.id.clone(),
        });
        let input = parse_tool_input(&tool.id, &tool.arguments, &mut errors);
        parts.push(MessagePart::ToolUse(ToolCall {
            id: tool.id,
            name: tool.name,
            input,
        }));
    }
    Ok(AssistantMessage {
        message: Message::assistant(parts),
        stop_reason,
        token_usage: usage,
        generation_duration: first_event.map_or(Duration::ZERO, |start| start.elapsed()),
        tool_input_errors: errors,
    })
}

fn parse_response(value: Value, duration: Duration) -> Result<AssistantMessage> {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .context("Completions response has no choices")?;
    let message = choice
        .get("message")
        .context("Completions response choice has no message")?;
    let mut parts = Vec::new();
    if let Some(thinking) = message.get("reasoning_content").and_then(Value::as_str) {
        if !thinking.is_empty() {
            parts.push(MessagePart::Thinking {
                thinking: thinking.to_string(),
                signature: None,
            });
        }
    }
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            parts.push(MessagePart::Text {
                text: text.to_string(),
            });
        }
    }
    let mut errors = HashMap::new();
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in tool_calls {
            let id = call.get("id").and_then(Value::as_str).unwrap_or("");
            let arguments = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            parts.push(MessagePart::ToolUse(ToolCall {
                id: id.to_string(),
                name: call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                input: parse_tool_input(id, arguments, &mut errors),
            }));
        }
    }
    Ok(AssistantMessage {
        message: Message::assistant(parts),
        stop_reason: parse_stop_reason(
            choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
        ),
        token_usage: parse_usage(value.get("usage")),
        generation_duration: duration,
        tool_input_errors: errors,
    })
}

/// Encode an image block as a Chat Completions `image_url` content item. The
/// pixels must already be resolved; a missing cache is a local mapping error.
fn image_url_wire(block: &ImageBlock) -> Result<Value> {
    let bytes = image_bytes(block)?;
    Ok(json!({
        "type": "image_url",
        "image_url": {
            "url": format!(
                "data:{};base64,{}",
                block.media_type.mime(),
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ),
            "detail": "auto",
        }
    }))
}

fn message_wire(message: &Message) -> Result<Vec<Value>> {
    match message.role {
        MessageRole::User => {
            let has_image = message
                .parts
                .iter()
                .any(|part| matches!(part, MessagePart::Image(_)));
            if has_image {
                let content = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        MessagePart::Text { text } => {
                            Some(Ok(json!({"type": "text", "text": text})))
                        }
                        MessagePart::Image(block) => Some(image_url_wire(block)),
                        _ => None,
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(vec![json!({"role": "user", "content": content})])
            } else {
                Ok(vec![json!({"role": "user", "content": message.text()})])
            }
        }
        MessageRole::Tool => message
            .parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::ToolResult(result) => Some(Ok(json!({
                    "role": "tool",
                    "tool_call_id": result.tool_use_id,
                    "content": result.content,
                }))),
                // Chat Completions tool messages are text-only; images here are
                // a protocol violation and must surface, not drop.
                MessagePart::Image(block) => Some(Err(anyhow::anyhow!(
                    "tool message for {} contains an image; images must be sent as a user message",
                    block.label
                ))),
                _ => None,
            })
            .collect::<Result<Vec<_>>>(),
        MessageRole::Assistant => {
            let calls = message
                .tool_calls()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": call.input.to_string()},
                    })
                })
                .collect::<Vec<_>>();
            let text = message.text();
            let mut value = json!({
                "role": "assistant",
                "content": if text.is_empty() { Value::Null } else { Value::String(text) },
            });
            if !calls.is_empty() {
                value["tool_calls"] = Value::Array(calls);
            }
            if let Some(MessagePart::Image(block)) = message
                .parts
                .iter()
                .find(|part| matches!(part, MessagePart::Image(_)))
            {
                bail!(
                    "assistant message for {} contains an image; images are only valid in user messages",
                    block.label
                );
            }
            Ok(vec![value])
        }
    }
}

fn parse_usage(value: Option<&Value>) -> TokenUsage {
    let Some(value) = value else {
        return TokenUsage::default();
    };
    let prompt = value
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached = value
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    TokenUsage {
        input_tokens: prompt.saturating_sub(cached),
        output_tokens: value
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: cached,
    }
}

fn parse_stop_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::End,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::Length,
        "content_filter" => StopReason::Refusal,
        other => StopReason::Unknown(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};

    use super::*;
    use crate::provider::{SystemBlock, ToolResult, ToolSpec};

    fn request() -> ProviderRequest {
        ProviderRequest {
            model: "test-model".to_string(),
            max_tokens: 321,
            system: vec![SystemBlock {
                text: "System rules".to_string(),
                cache: true,
            }],
            messages: vec![
                Message::user("Hello"),
                Message::assistant(vec![MessagePart::ToolUse(ToolCall {
                    id: "call-1".to_string(),
                    name: "notify".to_string(),
                    input: json!({"message": "Hi"}),
                })]),
                Message::tool(ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "sent".to_string(),
                    is_error: false,
                }),
            ],
            tools: vec![ToolSpec {
                name: "notify".to_string(),
                description: "Show a message".to_string(),
                input_schema: json!({"type": "object"}),
                cache: true,
            }],
        }
    }

    #[test]
    fn body_uses_chat_completions_wire_format() {
        let body = CompletionsProvider::body(&request(), true).unwrap();
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["max_tokens"], 321);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(body["messages"][3]["tool_call_id"], "call-1");
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn completions_wire_encodes_prompt_and_tool_images() {
        let png: Vec<u8> = {
            let image = image::DynamicImage::new_rgb8(8, 4);
            let mut out = std::io::Cursor::new(Vec::new());
            image.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        let tool = Message::tool(ToolResult {
            tool_use_id: "call-1".to_string(),
            content: "done".to_string(),
            is_error: false,
        });
        let prompt_image = Message::user_parts(vec![
            MessagePart::Text {
                text: "here".to_string(),
            },
            MessagePart::Image(crate::provider::ImageBlock {
                source: crate::provider::ImageSource::Attachment {
                    uri: "nole://attachment/00000000-0000-4000-8000-000000000000".to_string(),
                },
                label: "prompt.png".to_string(),
                media_type: crate::provider::ImageMediaType::Png,
                width: 8,
                height: 4,
                bytes: Some(std::sync::Arc::from(png.clone())),
            }),
        ]);

        let wire = message_wire(&tool).unwrap();
        // Chat Completions tool messages stay text-only.
        assert!(wire
            .iter()
            .all(|message| message["role"] == "tool" && message["content"].is_string()));

        let user = message_wire(&prompt_image).unwrap();
        assert_eq!(user[0]["role"], "user");
        let content = user[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        let url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        let encoded = url.split("base64,").nth(1).unwrap();
        assert_eq!(
            encoded,
            base64::engine::general_purpose::STANDARD.encode(&png)
        );

        // A plain-text user message stays a string, never an array.
        let plain = message_wire(&Message::user("Hello")).unwrap();
        assert!(plain[0]["content"].is_string());

        // Missing bytes is a local mapping error: never a silent drop to text.
        let broken = Message::user_parts(vec![MessagePart::Image(crate::provider::ImageBlock {
            source: crate::provider::ImageSource::Url {
                url: "https://example.com/x.png".to_string(),
            },
            label: "stale.png".to_string(),
            media_type: crate::provider::ImageMediaType::Png,
            width: 8,
            height: 4,
            bytes: None,
        })]);
        assert!(message_wire(&broken).is_err());
    }

    #[test]
    fn response_maps_parallel_tools_invalid_arguments_and_cached_usage() {
        let answer = parse_response(
            json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {
                        "role": "assistant",
                        "content": "Working",
                        "tool_calls": [
                            {"id": "a", "function": {"name": "notify", "arguments": "{\"message\":\"A\"}"}},
                            {"id": "b", "function": {"name": "notify", "arguments": "not-json"}}
                        ]
                    }
                }],
                "usage": {
                    "prompt_tokens": 20,
                    "completion_tokens": 5,
                    "prompt_tokens_details": {"cached_tokens": 8}
                }
            }),
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(answer.stop_reason, StopReason::ToolUse);
        assert_eq!(answer.text(), "Working");
        assert_eq!(answer.message.tool_calls().count(), 2);
        assert!(answer.tool_input_errors.contains_key("b"));
        assert_eq!(answer.token_usage.input_tokens, 12);
        assert_eq!(answer.token_usage.cache_read_input_tokens, 8);
        assert_eq!(answer.token_usage.output_tokens, 5);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streaming_reassembles_parallel_tool_calls_and_uses_expected_endpoint() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
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
                headers.push(line);
            }
            let mut request_body = vec![0; content_length];
            reader.read_exact(&mut request_body).unwrap();
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Working \",\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"notify\",\"arguments\":\"{\\\"message\\\":\"}},{\"index\":1,\"id\":\"call_b\",\"function\":{\"name\":\"notify\",\"arguments\":\"{\\\"message\\\":\"}}]},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"A\\\"}\"}},{\"index\":1,\"function\":{\"arguments\":\"\\\"B\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
                "data: [DONE]\n\n"
            );
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            reader.get_mut().flush().unwrap();
            (
                request_line,
                headers.concat(),
                serde_json::from_slice::<Value>(&request_body).unwrap(),
            )
        });

        let provider = CompletionsProvider::new("secret", format!("http://{address}/")).unwrap();
        let observable = provider.call_streaming(request());
        let mut subscriber = observable.subscribe();
        let mut live_events = observable.events;
        let mut events = live_events.resubscribe();
        let mut output = observable.output;
        let first_tool_delta = loop {
            tokio::select! {
                biased;
                result = &mut output => panic!("provider completed before exposing a live tool delta: {result:?}"),
                event = live_events.recv() => {
                    if let ProviderEvent::ToolCallDelta { index, arguments, .. } = event.unwrap() {
                        break (index, arguments);
                    }
                }
            }
        };
        assert_eq!(first_tool_delta, (0, r#"{"message":"#.to_string()));
        let answer = output.await.unwrap();
        let (request_line, headers, body) = server.join().unwrap();

        assert!(request_line.starts_with("POST /v1/chat/completions "));
        assert!(headers
            .to_ascii_lowercase()
            .contains("authorization: bearer secret"));
        assert_eq!(body["max_tokens"], 321);
        assert_eq!(answer.text(), "Working ");
        let calls = answer.message.tool_calls().collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].input, json!({"message": "A"}));
        assert_eq!(calls[1].input, json!({"message": "B"}));
        assert_eq!(answer.token_usage.input_tokens, 6);
        assert_eq!(answer.token_usage.cache_read_input_tokens, 4);
        assert!(
            matches!(events.try_recv(), Ok(ProviderEvent::TextDelta(text)) if text == "Working ")
        );
        let assert_tool_deltas =
            |receiver: &mut tokio::sync::broadcast::Receiver<ProviderEvent>| {
                let mut arguments = BTreeMap::<usize, String>::new();
                for expected_index in [0, 1, 0, 1] {
                    match receiver.try_recv().unwrap() {
                        ProviderEvent::ToolCallDelta {
                            index,
                            arguments: delta,
                            ..
                        } => {
                            assert_eq!(index, expected_index);
                            arguments.entry(index).or_default().push_str(&delta);
                        }
                        other => panic!("expected streamed tool input, got {other:?}"),
                    }
                }
                assert_eq!(arguments[&0], r#"{"message":"A"}"#);
                assert_eq!(arguments[&1], r#"{"message":"B"}"#);
            };
        assert_tool_deltas(&mut events);
        assert!(matches!(
            events.try_recv(),
            Ok(ProviderEvent::Usage { usage, .. })
                if usage.input_tokens == 6
                    && usage.cache_read_input_tokens == 4
                    && usage.output_tokens == 3
        ));
        assert!(
            matches!(subscriber.try_recv(), Ok(ProviderEvent::TextDelta(text)) if text == "Working ")
        );
        assert_tool_deltas(&mut subscriber);
        assert!(matches!(
            subscriber.try_recv(),
            Ok(ProviderEvent::Usage { usage, .. })
                if usage.input_tokens == 6
                    && usage.cache_read_input_tokens == 4
                    && usage.output_tokens == 3
        ));
        let assert_finished = |receiver: &mut tokio::sync::broadcast::Receiver<ProviderEvent>| {
            let finished = std::iter::from_fn(|| receiver.try_recv().ok())
                .filter_map(|event| match event {
                    ProviderEvent::ToolCallFinished { index, id } => Some((index, id)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                finished,
                vec![(0, "call_a".to_string()), (1, "call_b".to_string())]
            );
        };
        assert_finished(&mut events);
        assert_finished(&mut subscriber);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streaming_call_accepts_json_fallback_and_omits_empty_authorization() {
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
            let response = json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "Fallback response"}
                }],
                "usage": {"prompt_tokens": 4, "completion_tokens": 2}
            });
            let body = serde_json::to_vec(&response).unwrap();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            reader.get_mut().write_all(&body).unwrap();
            reader.get_mut().flush().unwrap();
            headers
        });

        let provider = CompletionsProvider::new("", format!("http://{address}")).unwrap();
        let observable = provider.call_streaming(request());
        let mut events = observable.events;
        let answer = observable.output.await.unwrap();
        let headers = server.join().unwrap();

        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        assert_eq!(answer.text(), "Fallback response");
        assert_eq!(answer.stop_reason, StopReason::End);
        assert_eq!(answer.generation_duration, Duration::ZERO);
        assert!(
            matches!(events.try_recv(), Ok(ProviderEvent::TextDelta(text)) if text == "Fallback response")
        );
        assert!(matches!(
            events.try_recv(),
            Ok(ProviderEvent::Usage {
                usage,
                generation_duration: Duration::ZERO
            }) if usage.input_tokens == 4 && usage.output_tokens == 2
        ));
    }
    #[tokio::test(flavor = "current_thread")]
    async fn streaming_forwards_reasoning_content_as_thinking() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
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
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Let me think\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\" harder.\",\"content\":\"Answer\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            reader.get_mut().flush().unwrap();
            (
                request_line,
                serde_json::from_slice::<Value>(&request_body).unwrap(),
            )
        });

        let provider = CompletionsProvider::new("secret", format!("http://{address}/")).unwrap();
        let observable = provider.call_streaming(request());
        let mut events = observable.events;
        let answer = observable.output.await.unwrap();
        server.join().unwrap();

        assert_eq!(answer.text(), "Answer");
        assert_eq!(answer.message.parts.len(), 2);
        assert!(matches!(
            &answer.message.parts[0],
            MessagePart::Thinking { thinking, .. } if thinking == "Let me think harder."
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(ProviderEvent::ThinkingDelta(text)) if text == "Let me think"
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(ProviderEvent::ThinkingDelta(text)) if text == " harder."
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(ProviderEvent::ThinkingFinished)
        ));
        assert!(matches!(
            events.try_recv(),
            Ok(ProviderEvent::TextDelta(text)) if text == "Answer"
        ));
    }
}
