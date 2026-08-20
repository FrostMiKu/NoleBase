//! Request-token estimation and context compaction boundaries.

use crate::provider::{Message, MessagePart, MessageRole, SystemBlock, ToolSpec};

pub(crate) const CONTEXT_COUNT_THRESHOLD_PERCENT: u64 = 75;
pub(crate) const CONTEXT_COMPACTION_TARGET_PERCENT: u64 = 50;
pub(crate) const CONTEXT_ESTIMATE_OVERHEAD: u64 = 1_024;
pub(crate) const MAX_CONTEXT_COMPACTIONS_PER_ROUND: usize = 3;
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

pub(crate) fn context_compaction_cut(messages: &[Message], target_tokens: u64) -> Option<usize> {
    (1..messages.len()).find(|&cut| {
        is_safe_compaction_boundary(messages, cut)
            && estimate_request_tokens(&[], &messages[cut..], &[]) <= target_tokens
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
}
