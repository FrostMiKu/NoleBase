//! Request-token estimation and context compaction boundaries.

use crate::provider::{Message, MessageRole, SystemBlock, ToolSpec};

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
    ascii
        .div_ceil(3)
        .saturating_add(non_ascii)
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
