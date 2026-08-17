//! Line selectors and bounded text pagination shared by every read parser.
//!
//! Parsers hand a raw `path:start-end` selector or a page window here. This
//! module owns the one-based inclusive selector grammar, the bounded 1 MiB
//! response budget, and the structured `range`/`returned`/`total`/`has_more`/
//! `next`/`items` page shape used by the document, attachment, and web parsers.

use std::fs::File;
use std::io::{BufRead, BufReader, Read as _};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::agent::{SnapshotIdentityHasher, SnapshotTagHasher};

use super::{
    DEFAULT_READ_LINES, MAX_EXTRACTED_TEXT_BYTES, MAX_READ_LINES, MAX_READ_LINE_BYTES,
    MAX_READ_RESPONSE_BYTES, READ_RESPONSE_OVERHEAD,
};

/// An inclusive one-based line window `start..=end` requested via a
/// `path:start-end` selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LineRange {
    offset: usize,
    limit: usize,
}

/// Splits a `path:start-end` suffix off a read target, leaving targets whose
/// suffix is not a valid inclusive line selector untouched.
pub(super) fn split_line_range(requested: &str) -> Result<(&str, Option<LineRange>)> {
    let Some((path, suffix)) = requested.rsplit_once(':') else {
        return Ok((requested, None));
    };
    let Some((start, end)) = suffix.split_once('-') else {
        return Ok((requested, None));
    };
    let (Ok(start), Ok(end)) = (start.parse::<usize>(), end.parse::<usize>()) else {
        return Ok((requested, None));
    };
    if start == 0 || end < start {
        bail!("line selector must be an inclusive range with 1 <= start <= end");
    }
    let limit = end - start + 1;
    if limit > MAX_READ_LINES {
        bail!("line selector may request at most {MAX_READ_LINES} lines");
    }
    Ok((
        path,
        Some(LineRange {
            offset: start - 1,
            limit,
        }),
    ))
}

/// Resolves a parsed line range into an `(offset, limit)` page window, falling
/// back to [`DEFAULT_READ_LINES`] lines when no selector was supplied.
pub(super) fn line_window(range: Option<LineRange>, input: &Value) -> Result<(usize, usize)> {
    if input.get("range").is_some() {
        bail!("file and attachment lines must use a `path:start-end` selector");
    }
    Ok(range
        .map(|range| (range.offset, range.limit))
        .unwrap_or((0, DEFAULT_READ_LINES)))
}

/// The `target:start-end` selector that continues a page after `end` lines.
pub(super) fn continuation_selector(target: &str, end: usize, limit: usize) -> String {
    format!(
        "{target}:{}-{}",
        end.saturating_add(1),
        end.saturating_add(limit)
    )
}

/// One page of selected lines with the pagination bookkeeping needed to render
/// a continuable read response.
#[derive(Debug)]
pub(super) struct TextPage {
    pub(super) lines: Vec<String>,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) total_lines: Option<usize>,
    pub(super) has_more: bool,
    pub(super) tag: Option<String>,
    pub(super) identity: Option<[u8; 32]>,
}

/// Read one UTF-8 line window with bounded response memory while scanning the
/// complete file to retain a strong edit identity and exact line count.
pub(super) fn read_utf8_page(
    path: &Path,
    offset: usize,
    limit: usize,
    encoded_len: fn(&str) -> usize,
) -> Result<Option<TextPage>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut tag_hasher = SnapshotTagHasher::default();
    let mut identity_hasher = SnapshotIdentityHasher::default();
    let mut selected = Vec::with_capacity(limit.min(DEFAULT_READ_LINES));
    let mut line = Vec::new();
    let mut lines_seen = 0usize;
    let mut response_bytes = 0usize;
    let response_budget = MAX_READ_RESPONSE_BYTES.saturating_sub(READ_RESPONSE_OVERHEAD);
    let mut response_full = false;
    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_READ_LINE_BYTES.saturating_add(3) as u64)
            .read_until(b'\n', &mut line)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        let raw = match std::str::from_utf8(&line) {
            Ok(raw) => raw,
            Err(_) => return Ok(None),
        };
        let without_lf = raw.strip_suffix('\n').unwrap_or(raw);
        let text = without_lf.strip_suffix('\r').unwrap_or(without_lf);
        if text.len() > MAX_READ_LINE_BYTES {
            bail!("line {} exceeds the 256 KiB read limit", lines_seen + 1);
        }
        tag_hasher.update(&line);
        identity_hasher.update(&line);
        if lines_seen >= offset && selected.len() < limit && !response_full {
            let cost = encoded_len(text).saturating_add(32);
            if response_bytes.saturating_add(cost) <= response_budget {
                selected.push(text.to_string());
                response_bytes = response_bytes.saturating_add(cost);
            } else if selected.is_empty() {
                bail!(
                    "line {} cannot fit within the 1 MB read response limit",
                    lines_seen + 1
                );
            } else {
                response_full = true;
            }
        }
        lines_seen += 1;
    }
    let start = offset.min(lines_seen);
    let end = start.saturating_add(selected.len());
    Ok(Some(TextPage {
        lines: selected,
        start,
        end,
        total_lines: Some(lines_seen),
        has_more: end < lines_seen,
        tag: Some(tag_hasher.finish()),
        identity: Some(identity_hasher.finish()),
    }))
}

pub(super) fn plain_response_len(text: &str) -> usize {
    text.len()
}

pub(super) fn json_response_len(text: &str) -> usize {
    text.chars()
        .map(|character| match character {
            '"' | '\\' | '\u{0008}' | '\t' | '\n' | '\u{000c}' | '\r' => 2,
            character if character <= '\u{001f}' => 6,
            character => character.len_utf8(),
        })
        .sum()
}

/// Pages already-extracted text (documents, web reader mode) under the same
/// response budget as [`read_utf8_page`], without edit identity hashing.
pub(super) fn page_extracted_text(
    content: &str,
    offset: usize,
    limit: usize,
    encoded_len: fn(&str) -> usize,
) -> Result<TextPage> {
    if content.len() > MAX_EXTRACTED_TEXT_BYTES {
        bail!("extracted text exceeds the {MAX_EXTRACTED_TEXT_BYTES} byte limit");
    }
    let mut selected = Vec::with_capacity(limit.min(DEFAULT_READ_LINES));
    let mut lines_seen = 0usize;
    let mut response_bytes = 0usize;
    let response_budget = MAX_READ_RESPONSE_BYTES.saturating_sub(READ_RESPONSE_OVERHEAD);
    let mut has_more = false;
    for raw in content.split_inclusive('\n') {
        let without_lf = raw.strip_suffix('\n').unwrap_or(raw);
        let text = without_lf.strip_suffix('\r').unwrap_or(without_lf);
        if text.len() > MAX_READ_LINE_BYTES {
            bail!("line {} exceeds the 256 KiB read limit", lines_seen + 1);
        }
        if lines_seen >= offset && selected.len() >= limit {
            has_more = true;
            break;
        }
        if lines_seen >= offset {
            let cost = encoded_len(text).saturating_add(32);
            if response_bytes.saturating_add(cost) <= response_budget {
                selected.push(text.to_string());
                response_bytes = response_bytes.saturating_add(cost);
            } else if selected.is_empty() {
                bail!(
                    "line {} cannot fit within the 1 MB read response limit",
                    lines_seen + 1
                );
            } else {
                has_more = true;
                break;
            }
        }
        lines_seen += 1;
    }
    let start = offset.min(lines_seen);
    let end = start.saturating_add(selected.len());
    Ok(TextPage {
        lines: selected,
        start,
        end,
        total_lines: (!has_more).then_some(lines_seen),
        has_more,
        tag: None,
        identity: None,
    })
}

/// Fills a structured payload with the shared `range`/`returned`/`total`/
/// `has_more`/`next`/`items` page fields.
pub(super) fn add_structured_page(
    payload: &mut Value,
    page: TextPage,
    target: &str,
    offset: usize,
    limit: usize,
) {
    payload["range"] = json!(format!("{}-{}", offset + 1, offset.saturating_add(limit)));
    payload["returned"] = json!(page.end - page.start);
    payload["total"] = json!(page.total_lines);
    payload["has_more"] = json!(page.has_more);
    if page.has_more {
        payload["next"] = json!(continuation_selector(target, page.end, limit));
    }
    payload["items"] = json!(page.lines);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn line_selectors_are_one_based_inclusive_and_bounded() {
        assert_eq!(
            split_line_range("data/note.md:50-200").unwrap(),
            (
                "data/note.md",
                Some(LineRange {
                    offset: 49,
                    limit: 151,
                }),
            )
        );
        assert_eq!(
            split_line_range("data/note.md").unwrap(),
            ("data/note.md", None)
        );
        assert!(split_line_range("data/note.md:0-2").is_err());
        assert!(split_line_range("data/note.md:4-3").is_err());
        assert!(split_line_range("data/note.md:1-2001").is_err());
        assert!(line_window(None, &json!({"range": "1-2"})).is_err());
    }

    #[test]
    fn response_byte_cap_returns_a_continuable_partial_page() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wide.txt");
        let line = "x".repeat(250 * 1024);
        fs::write(&path, vec![line; 5].join("\n")).unwrap();
        let page = read_utf8_page(&path, 0, 5, plain_response_len)
            .unwrap()
            .unwrap();
        assert_eq!(page.total_lines, Some(5));
        assert!(page.end > 0 && page.end < 5);
        assert_eq!(page.start, 0);
        assert_eq!(page.lines.len(), page.end);
    }

    #[test]
    fn oversized_single_line_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("one-line.txt");
        fs::write(&path, vec![b'x'; MAX_READ_LINE_BYTES + 1]).unwrap();
        let error = read_utf8_page(&path, 0, 1, plain_response_len).unwrap_err();
        assert!(error.to_string().contains("line 1"));
        assert!(error.to_string().contains("256 KiB"));
    }
}
