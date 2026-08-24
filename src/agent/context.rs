//! Request-token estimation and context compaction boundaries.

use crate::provider::{Message, MessagePart, MessageRole, SystemBlock, ToolSpec};

pub(crate) const CONTEXT_COUNT_THRESHOLD_PERCENT: u64 = 75;
pub(crate) const CONTEXT_COMPACTION_TARGET_PERCENT: u64 = 50;
/// Counted input tokens this far below the budget still trigger
/// compaction: token counts can drift below what the real request is
/// charged, so a count just under the budget is not safe to trust.
pub(crate) const CONTEXT_COUNT_SAFETY_MARGIN_PERCENT: u64 = 10;
pub(crate) const CONTEXT_ESTIMATE_OVERHEAD: u64 = 1_024;
pub(crate) const MAX_CONTEXT_COMPACTIONS_PER_ROUND: usize = 3;
/// Share of the per-request input budget a single tool result may occupy.
pub(crate) const TOOL_RESULT_BUDGET_PERCENT: u64 = 25;
/// Emergency cap for stored tool results when no compaction cut exists.
pub(crate) const TOOL_RESULT_EMERGENCY_TOKENS: u64 = 2_048;
pub(crate) const MAX_EMPTY_RESPONSE_RETRIES: usize = 2;
pub(crate) const MAX_TRUNCATION_RETRIES: usize = 3;

pub(crate) fn estimate_request_tokens(
    system: &[SystemBlock],
    messages: &[Message],
    definitions: &[ToolSpec],
) -> u64 {
    let text = format!(
        "{}{}{}",
        serde_json::to_string(system).unwrap_or_default(),
        serde_json::to_string(messages).unwrap_or_default(),
        serde_json::to_string(definitions).unwrap_or_default()
    );
    let mut ascii = 0u64;
    let mut non_ascii = 0u64;
    for character in text.chars() {
        if character.is_ascii() {
            ascii += 1;
        } else {
            non_ascii += 1;
        }
    }
    // Images serialize without their pixel bytes; estimate each at the
    // standard 28px tile count with saturating arithmetic.
    let mut image_tokens = 0u64;
    for message in messages {
        for part in &message.parts {
            if let MessagePart::Image(block) = part {
                let tiles = (u64::from(block.width).div_ceil(28))
                    .saturating_mul(u64::from(block.height).div_ceil(28));
                image_tokens = image_tokens.saturating_add(tiles);
            }
        }
    }
    ascii
        .div_ceil(3)
        .saturating_add(non_ascii)
        .saturating_add(image_tokens)
        .saturating_add(CONTEXT_ESTIMATE_OVERHEAD)
}

/// Estimated tokens for a standalone text, using the same per-character
/// costs as [`estimate_request_tokens`]: three ASCII characters or one
/// non-ASCII character per token.
pub(crate) fn estimate_text_tokens(text: &str) -> u64 {
    text_units(text).div_ceil(3)
}

/// Truncate a tool result so the text plus its truncation marker stays within
/// `max_tokens` estimated tokens. Returns `None` when the text already fits.
pub(crate) fn truncate_tool_result(text: &str, max_tokens: u64) -> Option<String> {
    let budget_units = max_tokens.saturating_mul(3);
    if text_units(text) <= budget_units {
        return None;
    }
    // Reserve room for the widest possible marker up front so the final
    // truncated text always fits the budget.
    let probe = truncation_marker(u64::MAX, u64::MAX);
    let content_units = budget_units.saturating_sub(text_units(&probe));
    let mut kept_units = 0u64;
    let mut end = 0usize;
    for (index, character) in text.char_indices() {
        let units = character_units(character);
        if kept_units.saturating_add(units) > content_units {
            break;
        }
        kept_units += units;
        end = index + character.len_utf8();
    }
    Some(format!(
        "{}{}",
        &text[..end],
        truncation_marker(end as u64, text.len() as u64)
    ))
}

/// Cut every stored tool result above `max_tokens` down to it. Compaction
/// cuts only land on user messages, so an oversized result in the current
/// turn can never be summarized away; shrinking stored results is the only
/// way to keep such a conversation usable. Returns whether anything changed.
pub(crate) fn truncate_stored_tool_results(messages: &mut [Message], max_tokens: u64) -> bool {
    let mut truncated = false;
    for message in messages {
        for part in &mut message.parts {
            if let MessagePart::ToolResult(result) = part {
                if let Some(content) = truncate_tool_result(&result.content, max_tokens) {
                    result.content = content;
                    truncated = true;
                }
            }
        }
    }
    truncated
}

fn truncation_marker(kept: u64, total: u64) -> String {
    format!(
        "\n\n[agent truncated this tool result to fit the context budget: kept {kept} of {total} bytes; narrow the request or fetch the remainder in smaller slices]"
    )
}

/// Token-estimation cost units for `text`: one unit per ASCII character and
/// three per non-ASCII character, i.e. three units per estimated token.
fn text_units(text: &str) -> u64 {
    text.chars()
        .map(character_units)
        .fold(0u64, u64::saturating_add)
}

fn character_units(character: char) -> u64 {
    if character.is_ascii() {
        1
    } else {
        3
    }
}

pub(crate) fn context_compaction_cut_for_request(
    system: &[SystemBlock],
    messages: &[Message],
    definitions: &[ToolSpec],
    target_tokens: u64,
) -> Option<usize> {
    (1..messages.len()).find(|&cut| {
        is_safe_compaction_boundary(messages, cut)
            && estimate_request_tokens(system, &messages[cut..], definitions) <= target_tokens
    })
}

pub(crate) fn is_safe_compaction_boundary(messages: &[Message], cut: usize) -> bool {
    messages
        .get(cut)
        .is_some_and(|message| message.role == MessageRole::User)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::provider::ToolResult;

    use super::*;
    use crate::provider::{ImageBlock, ImageMediaType, ImageSource, MessagePart};

    #[test]
    fn image_token_estimate_ignores_pixel_byte_length() {
        let small = ImageBlock {
            source: ImageSource::LocalFile {
                path: std::path::PathBuf::from("a.png"),
            },
            label: "s".to_string(),
            media_type: ImageMediaType::Png,
            width: 56,
            height: 56,
            bytes: Some(Arc::from(vec![0u8; 1])),
        };
        let large = ImageBlock {
            source: ImageSource::LocalFile {
                path: std::path::PathBuf::from("b.png"),
            },
            label: "l".to_string(),
            media_type: ImageMediaType::Png,
            width: 56,
            height: 56,
            bytes: Some(Arc::from(vec![0u8; 8 * 1024 * 1024])),
        };
        let with_small = estimate_request_tokens(
            &[],
            &[Message::user_parts(vec![MessagePart::Image(small)])],
            &[],
        );
        let with_large = estimate_request_tokens(
            &[],
            &[Message::user_parts(vec![MessagePart::Image(large)])],
            &[],
        );
        // Pixel byte length must not change the estimate: only dimensions do.
        assert_eq!(with_small, with_large);
        let baseline = estimate_request_tokens(&[], &[Message::user("x")], &[]);
        assert!(with_small > baseline);
    }

    #[test]
    fn compaction_boundary_accounts_for_system_and_tool_tokens() {
        let system = vec![SystemBlock {
            text: "system context ".repeat(100),
            cache: false,
        }];
        let definitions = vec![ToolSpec {
            name: "read".to_string(),
            description: "read ".repeat(100),
            input_schema: serde_json::json!({"type": "object"}),
            cache: false,
        }];
        let messages = vec![Message::user("old"), Message::user("latest")];
        let full_tail = estimate_request_tokens(&system, &messages[1..], &definitions);

        assert!(
            context_compaction_cut_for_request(&system, &messages, &definitions, full_tail)
                .is_some()
        );
        assert!(context_compaction_cut_for_request(
            &system,
            &messages,
            &definitions,
            full_tail.saturating_sub(1)
        )
        .is_none());
    }

    #[test]
    fn tool_result_within_budget_is_returned_untouched() {
        assert_eq!(truncate_tool_result("small", 1_000), None);
    }

    #[test]
    fn tool_result_truncation_keeps_estimate_within_budget() {
        let text = "a".repeat(30_000);
        let truncated = truncate_tool_result(&text, 1_000).unwrap();
        assert!(truncated.starts_with(&"a".repeat(2_500)));
        assert!(truncated.len() < text.len());
        assert!(truncated.contains("agent truncated this tool result"));
        assert!(truncated.contains("of 30000 bytes"));
        assert!(estimate_text_tokens(&truncated) <= 1_000);
    }

    #[test]
    fn tool_result_truncation_splits_on_char_boundaries() {
        let text = "界".repeat(9_999);
        let truncated = truncate_tool_result(&text, 100).unwrap();
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
        assert!(estimate_text_tokens(&truncated) <= 100);
    }

    #[test]
    fn stored_tool_results_are_truncated_only_when_oversized() {
        let mut messages = vec![
            Message::user("go"),
            Message::tool(ToolResult {
                tool_use_id: "one".to_string(),
                content: "keep".to_string(),
                is_error: false,
            }),
            Message::tool(ToolResult {
                tool_use_id: "two".to_string(),
                content: "x".repeat(50_000),
                is_error: false,
            }),
        ];
        assert!(truncate_stored_tool_results(&mut messages, 2_048));
        let kept = match &messages[1].parts[0] {
            MessagePart::ToolResult(result) => result.content.as_str(),
            _ => panic!("expected tool result"),
        };
        assert_eq!(kept, "keep");
        let shrunk = match &messages[2].parts[0] {
            MessagePart::ToolResult(result) => result.content.as_str(),
            _ => panic!("expected tool result"),
        };
        assert!(shrunk.contains("agent truncated this tool result"));
        assert!(estimate_text_tokens(shrunk) <= 2_048);
        assert!(!truncate_stored_tool_results(&mut messages, 2_048));
    }
}
