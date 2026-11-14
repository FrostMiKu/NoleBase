//! Shared note completion matching and viewport behavior.
use crate::core::model::WikiLinkCandidate;

/// An unclosed `[[` marker before the cursor on the same line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WikiSpan {
    /// Character offset of the first `[`.
    pub(crate) start: usize,
}

/// Byte offset of a character index into `input`.
pub(crate) fn byte_offset(input: &str, char_index: usize) -> usize {
    input
        .char_indices()
        .nth(char_index)
        .map_or(input.len(), |(offset, _)| offset)
}

/// Character index of a byte offset into `input`.
fn char_index(input: &str, byte_offset: usize) -> usize {
    input[..byte_offset.min(input.len())].chars().count()
}

/// Find the wiki-link span the cursor is inside: the last `[[` on the
/// current line before the cursor whose query (marker to cursor) contains
/// no brackets, so closed links and nested markers never complete. Offsets
/// are character indexes to match `input_cursor`.
pub(crate) fn open_wiki_span(input: &str, cursor: usize) -> Option<WikiSpan> {
    let cursor_bytes = byte_offset(input, cursor);
    let before = &input[..cursor_bytes];
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let line = &before[line_start..];
    let marker = line.rfind("[[")?;
    let start = line_start + marker;
    let query = &line[marker + 2..];
    if query.contains('[') || query.contains(']') {
        return None;
    }
    // `[[[` is a broken marker, not a fresh one.
    if marker > 0 && line[..marker].ends_with('[') {
        return None;
    }
    Some(WikiSpan {
        start: char_index(input, start),
    })
}

/// Filter and rank candidates for a query: case-insensitive stem matches,
/// prefix hits before substring hits, alphabetical within each tier. The
/// full ranked list is returned; the popup window scrolls through it.
/// Stems containing wiki-link syntax characters are skipped: splicing them
/// into `[[stem]]` would produce a broken link.
pub(crate) fn filter_wiki_completions(
    candidates: &[WikiLinkCandidate],
    query: &str,
) -> Vec<WikiLinkCandidate> {
    let query = query.trim().to_lowercase();
    let mut prefix = Vec::new();
    let mut substring = Vec::new();
    for candidate in candidates {
        let Some(stem) = stem_label(candidate) else {
            continue;
        };
        if stem.contains(['[', ']', '|', '#']) {
            continue;
        }
        let lower = stem.to_lowercase();
        if query.is_empty() || lower.starts_with(&query) {
            prefix.push(candidate.clone());
        } else if lower.contains(&query) {
            substring.push(candidate.clone());
        }
    }
    let by_stem = |a: &WikiLinkCandidate, b: &WikiLinkCandidate| stem_label(a).cmp(&stem_label(b));
    prefix.sort_by(by_stem);
    substring.sort_by(by_stem);
    prefix.extend(substring);
    prefix
}

/// The completion label for a candidate: its file stem.
pub(crate) fn stem_label(candidate: &WikiLinkCandidate) -> Option<String> {
    candidate
        .path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
}

/// Splice an accepted candidate into the input, replacing `span_start..cursor`
/// (character indexes) with `[[stem]]` and returning the new cursor just past
/// the closing bracket, also as a character index.
#[cfg(feature = "tui")]
pub(crate) fn apply_wiki_completion(
    input: &str,
    span_start: usize,
    cursor: usize,
    stem: &str,
) -> (String, usize) {
    let start_bytes = byte_offset(input, span_start);
    let cursor_bytes = byte_offset(input, cursor);
    let mut output = String::with_capacity(input.len() + stem.len() + 4);
    output.push_str(&input[..start_bytes]);
    output.push_str("[[");
    output.push_str(stem);
    output.push_str("]]");
    let new_cursor_bytes = output.len();
    output.push_str(&input[cursor_bytes..]);
    let cursor = char_index(&output, new_cursor_bytes);
    (output, cursor)
}

pub(crate) fn selection_viewport_start(
    start: usize,
    selected: usize,
    visible: usize,
    total: usize,
) -> usize {
    if visible == 0 || total == 0 {
        return 0;
    }
    let selected = selected.min(total - 1);
    let mut start = start.min(total.saturating_sub(visible));
    if selected < start {
        start = selected;
    } else if selected >= start.saturating_add(visible) {
        start = selected.saturating_add(1).saturating_sub(visible);
    }
    start.min(total.saturating_sub(visible))
}
