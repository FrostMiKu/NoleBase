//! Small helpers shared across several tool implementations.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use similar::TextDiff;

/// Largest rendered diff the model should receive before truncation.
pub(super) const MAX_DIFF_BYTES: usize = 200_000;
/// Largest UTF-8 document accepted by whole-file mutation tools (1 MB).
pub(super) const MAX_EDIT_FILE_BYTES: u64 = 1_000_000;
pub(super) const DEFAULT_SEARCH_RESULTS: usize = 50;
pub(super) const MAX_SEARCH_RESULTS: usize = 200;
pub(super) const MAX_SEARCH_OFFSET: usize = 10_000;
pub(super) const MAX_SEARCH_SNIPPET_CHARS: usize = 500;

pub(super) fn display_path(root: &Path, path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub(super) fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let mut offset = 0;
    for wanted in needle.to_lowercase().chars() {
        let Some(found) = hay[offset..]
            .iter()
            .position(|candidate| *candidate == wanted)
        else {
            return false;
        };
        offset += found + 1;
    }
    true
}

pub(super) fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

pub(super) fn optional_usize(
    input: &Value,
    key: &str,
    default: usize,
    maximum: usize,
) -> Result<usize> {
    let Some(value) = input.get(key) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .with_context(|| format!("field {key} must be a non-negative integer"))?;
    let value = usize::try_from(value).with_context(|| format!("field {key} is too large"))?;
    if value > maximum || (key == "limit" && value == 0) {
        bail!(
            "field {key} must be between {} and {maximum}",
            usize::from(key == "limit")
        );
    }
    Ok(value)
}

pub(super) fn required_string<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string field {key}"))
}

pub(super) fn limited_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let diff = TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(old_label, new_label)
        .to_string();
    if diff.len() <= MAX_DIFF_BYTES {
        return diff;
    }
    let mut end = MAX_DIFF_BYTES;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... diff truncated ...\n", &diff[..end])
}
