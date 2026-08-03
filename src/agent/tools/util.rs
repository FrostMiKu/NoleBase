//! Small helpers shared across several tool implementations.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use similar::TextDiff;

/// Largest rendered diff the model should receive before truncation.
pub(super) const MAX_DIFF_BYTES: usize = 200_000;
/// Largest UTF-8 document accepted by whole-file mutation tools (1 MB).
pub(super) const MAX_EDIT_FILE_BYTES: u64 = 1_000_000;
pub(super) const MAX_SEARCH_RESULTS: usize = 200;
pub(super) const MAX_SEARCH_SNIPPET_CHARS: usize = 500;

pub(super) const DEFAULT_PAGE_SIZE: usize = 50;

/// One-based inclusive selector shared by model-facing list and search tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RangeSelector {
    pub start: usize,
    pub end: usize,
}

impl RangeSelector {
    pub fn from_input(input: &Value, max_span: usize) -> Result<Self> {
        let default = format!("1-{}", DEFAULT_PAGE_SIZE.min(max_span));
        let raw = match input.get("range") {
            Some(value) => value
                .as_str()
                .context("field range must be a string like `1-50`")?,
            None => default.as_str(),
        };
        Self::parse(raw, max_span)
    }

    pub fn parse(raw: &str, max_span: usize) -> Result<Self> {
        let (start, end) = raw
            .split_once('-')
            .context("range must be an inclusive one-based selector like `1-50`")?;
        if start.is_empty() || end.is_empty() || end.contains('-') {
            bail!("range must be an inclusive one-based selector like `1-50`");
        }
        let start = start
            .parse::<usize>()
            .context("range start must be a positive integer")?;
        let end = end
            .parse::<usize>()
            .context("range end must be a positive integer")?;
        if start == 0 || end < start {
            bail!("range must satisfy 1 <= start <= end");
        }
        let span = end
            .checked_sub(start)
            .and_then(|difference| difference.checked_add(1))
            .context("range is too large")?;
        if span > max_span {
            bail!("range may select at most {max_span} items");
        }
        Ok(Self { start, end })
    }

    pub fn as_string(self) -> String {
        format!("{}-{}", self.start, self.end)
    }

    pub fn window(self, total: usize) -> PageWindow {
        let start_index = self.start.saturating_sub(1).min(total);
        let end_index = self.end.min(total);
        PageWindow {
            selector: self,
            start_index,
            end_index: end_index.max(start_index),
            total,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PageWindow {
    pub selector: RangeSelector,
    pub start_index: usize,
    pub end_index: usize,
    pub total: usize,
}

impl PageWindow {
    pub fn returned(self) -> usize {
        self.end_index - self.start_index
    }

    pub fn has_more(self) -> bool {
        self.end_index < self.total
    }

    pub fn next(self) -> Option<String> {
        if !self.has_more() {
            return None;
        }
        let span = self.selector.end - self.selector.start + 1;
        let start = self.end_index.checked_add(1)?;
        let end = start.checked_add(span - 1)?;
        Some(format!("{start}-{end}"))
    }
}

pub(super) fn range_schema(max_span: usize) -> Value {
    let default = format!("1-{}", DEFAULT_PAGE_SIZE.min(max_span));
    json!({
        "type": "string",
        "pattern": "^[1-9][0-9]*-[1-9][0-9]*$",
        "default": default,
        "description": format!(
            "Inclusive one-based result range; may select at most {max_span} items"
        )
    })
}

pub(super) fn display_path(root: &Path, path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    portable_path(canonical.strip_prefix(root).unwrap_or(path))
}

pub(super) fn portable_path(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        text.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        text
    }
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
        let minimum = usize::from(key == "limit");
        bail!("field {key} must be between {minimum} and {maximum}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_selector_is_one_based_inclusive_and_bounded() {
        let selector = RangeSelector::parse("51-100", 50).unwrap();
        assert_eq!(
            selector,
            RangeSelector {
                start: 51,
                end: 100
            }
        );
        assert!(RangeSelector::parse("0-1", 50).is_err());
        assert!(RangeSelector::parse("2-1", 50).is_err());
        assert!(RangeSelector::parse("1", 50).is_err());
        assert!(RangeSelector::parse("1-51", 50).is_err());
    }

    #[test]
    fn range_window_reports_truthful_page_and_continuation() {
        let page = RangeSelector::parse("1-2", 50).unwrap().window(3);
        assert_eq!((page.start_index, page.end_index), (0, 2));
        assert_eq!(page.returned(), 2);
        assert!(page.has_more());
        assert_eq!(page.next().as_deref(), Some("3-4"));

        let last = RangeSelector::parse("3-4", 50).unwrap().window(3);
        assert_eq!(last.returned(), 1);
        assert!(!last.has_more());
        assert_eq!(last.next(), None);
    }
}
