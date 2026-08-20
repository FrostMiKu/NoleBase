//! Formatting helpers for tool activity, diagnostics, and tool results.

use serde_json::{json, Value};

use crate::provider::{MessagePart, StopReason, ToolCall, ToolResult};

pub(crate) fn deferred_tool_result(call: &ToolCall) -> ToolResult {
    ToolResult {
        tool_use_id: call.id.clone(),
        content: "Tool call deferred because new user input arrived before execution.".to_string(),
        is_error: true,
    }
}

pub(crate) fn failed_tool_result(call: &ToolCall, error: &str) -> ToolResult {
    ToolResult {
        tool_use_id: call.id.clone(),
        content: error.to_string(),
        is_error: true,
    }
}

pub(crate) fn skipped_after_denial_tool_result(call: &ToolCall) -> ToolResult {
    ToolResult {
        tool_use_id: call.id.clone(),
        content: "Tool call not executed because the user denied an earlier tool call.".to_string(),
        is_error: true,
    }
}

pub(crate) fn tool_call_value(call: &ToolCall) -> Value {
    json!({"id": call.id, "name": call.name, "input": call.input})
}

pub(crate) fn empty_response_diagnostic(
    stop_reason: &StopReason,
    content: &[MessagePart],
) -> String {
    let mut block_types = content
        .iter()
        .map(|block| match block {
            MessagePart::Text { .. } => "text",
            MessagePart::Thinking { .. } => "thinking",
            MessagePart::RedactedThinking { .. } => "redacted_thinking",
            MessagePart::ToolUse(_) => "tool_use",
            MessagePart::ToolResult(_) => "tool_result",
            MessagePart::Image(_) => "image",
        })
        .collect::<Vec<_>>();
    block_types.sort_unstable();
    block_types.dedup();
    let block_types = if block_types.is_empty() {
        "none".to_string()
    } else {
        block_types.join(", ")
    };
    format!(
        "Provider response did not contain a complete final answer after automatic continuation (stop_reason: {stop_reason:?}, content block types: {block_types})"
    )
}

pub(crate) fn tool_start_activity(call: &Value) -> String {
    let name = call.get("name").and_then(Value::as_str).unwrap_or("");
    let raw_target = tool_activity_target(call);
    let target = raw_target
        .as_deref()
        .map(|target| format!("\n{target}"))
        .unwrap_or_default();
    match name {
        "read" if raw_target.as_deref().is_some_and(is_url) => format!("Fetching Web...{target}"),
        "search_web" => format!("Searching Web...{target}"),
        _ => format!("Calling {}...{target}", tool_display_name(name)),
    }
}

pub(crate) fn tool_finish_activity(call: &Value, error: Option<&str>) -> String {
    let name = tool_display_name(call.get("name").and_then(Value::as_str).unwrap_or(""));
    let target = tool_activity_target(call)
        .map(|target| format!("\n{target}"))
        .unwrap_or_default();
    if let Some(error) = error {
        format!("Failed {name}: {error}{target}")
    } else {
        format!("Completed {name}.{target}")
    }
}

pub(crate) fn tool_activity_target(call: &Value) -> Option<String> {
    let name = call.get("name").and_then(Value::as_str).unwrap_or("");
    let input = call.get("input")?;
    let text = |field: &str| {
        input
            .get(field)
            .and_then(Value::as_str)
            .map(compact_activity_value)
            .filter(|value| !value.is_empty())
    };
    match name {
        "explore" => text("task"),
        "review" => text("task"),
        "calculate" => text("expression"),
        "read" => text("path").map(|path| {
            if is_url(&path) {
                web_base_url(&path)
            } else {
                path
            }
        }),
        "http_request" => text("url").map(|url| {
            let method = input
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_uppercase();
            match input.get("save_to").and_then(Value::as_str) {
                Some(destination) => format!("{method} {url} -> {destination}"),
                None => format!("{method} {url}"),
            }
        }),
        "grep" => text("pattern"),
        "search_web" | "search_files" | "list_tags" => text("query"),
        "search_tag" => text("tag"),
        "resolve_wikilink" | "backlinks" => text("target"),
        "add_daily_entry" => Some(text("date").unwrap_or_else(|| "Today".to_string())),
        "copy" | "move" => Some(format!("{} -> {}", text("source")?, text("destination")?)),
        "import_attachment" => text("source"),
        "attachment_info" | "delete_attachment" => text("uri"),
        "checkout_attachment" => Some(format!("{} -> {}", text("uri")?, text("destination")?)),
        "update_attachment" => Some(format!("{} <- {}", text("uri")?, text("source")?)),
        "move_many" => {
            let count = input.get("sources").and_then(Value::as_array)?.len();
            Some(format!(
                "{count} files -> {}",
                text("destination_directory")?
            ))
        }
        "rename" => Some(format!("{} -> {}", text("path")?, text("new_name")?)),
        "notify" => text("message"),
        "ask" => text("question"),
        _ => text("path"),
    }
}

pub(crate) fn compact_activity_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compact single-line preview of a successful tool result for the Agent Chat
/// activity stream. Structured (JSON) and empty results are withheld: the
/// preview stays human-readable, while tool input/output JSON remains in the
/// conversation history.
pub(crate) fn tool_result_preview(content: &str) -> Option<String> {
    const MAX_PREVIEW_CHARS: usize = 120;
    let content = content.trim();
    if content.is_empty() || content.starts_with('{') || content.starts_with('[') {
        return None;
    }
    let mut preview = compact_activity_value(content);
    if preview.chars().count() > MAX_PREVIEW_CHARS {
        preview = preview
            .chars()
            .take(MAX_PREVIEW_CHARS.saturating_sub(1))
            .collect::<String>()
            .trim_end()
            .to_string()
            + "…";
    }
    Some(preview)
}

pub(crate) fn web_base_url(value: &str) -> String {
    reqwest::Url::parse(value)
        .ok()
        .map(|url| url.origin().ascii_serialization())
        .filter(|origin| origin != "null")
        .unwrap_or_else(|| value.to_string())
}

fn is_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

pub(crate) fn tool_display_name(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}
