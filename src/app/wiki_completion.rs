//! Inline `[[wiki link]]` completion for the compose input.
//!
//! While the cursor sits after an unclosed `[[` (or `![[`) on its line, the
//! compose input offers matching notes as a compact popup above the input.
//! The whole pipeline is derived from `(input, cursor)` so it cannot desync:
//! helpers here are pure, the App state only carries the filtered candidates
//! and the user's selection.

use crate::model::{WikiLinkCandidate, WikiLinkLocation};

use super::{move_index, App};

/// Candidate rows the completion popup window shows at once. The candidate
/// list itself is never capped: the window scrolls as the selection moves.
pub(crate) const WIKI_COMPLETION_WINDOW: usize = 8;

/// The App-side completion state: filtered candidates plus the selected row
/// and the popup's scroll offset. Recomputed from the input after every edit;
/// `None` when no unclosed `[[` precedes the cursor. The selection leads and
/// the list follows only at the edge it reaches, exactly like the command
/// palette: `index` moves on every key press, while `scroll` (the first
/// visible row) is reconciled from `index` at render time and held steady
/// while the selection still fits inside the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WikiCompletionState {
    /// Character offset of the first `[` of the opening marker.
    pub(crate) span_start: usize,
    /// The query typed between `[[` and the cursor.
    pub(crate) query: String,
    pub(crate) candidates: Vec<WikiLinkCandidate>,
    /// The selected candidate row.
    pub(crate) index: usize,
    /// First visible candidate row of the popup window.
    pub(crate) scroll: usize,
}

/// An unclosed `[[` marker before the cursor on the same line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WikiSpan {
    /// Character offset of the first `[`.
    pub(crate) start: usize,
}

/// Byte offset of a character index into `input`.
fn byte_offset(input: &str, char_index: usize) -> usize {
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
pub(crate) fn filter_wiki_completions(
    candidates: &[WikiLinkCandidate],
    query: &str,
) -> Vec<WikiLinkCandidate> {
    let query = query.trim();
    let mut prefix = Vec::new();
    let mut substring = Vec::new();
    for candidate in candidates {
        let Some(stem) = stem_label(candidate) else {
            continue;
        };
        let lower = stem.to_lowercase();
        if query.is_empty() || lower.starts_with(&query.to_lowercase()) {
            prefix.push(candidate.clone());
        } else if lower.contains(&query.to_lowercase()) {
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

/// Build every completable candidate from the cached daily and note lists.
pub(in crate::app) fn all_wiki_candidates(
    daily_dates: &[chrono::NaiveDate],
    note_files: &[crate::model::NoteFile],
) -> Vec<WikiLinkCandidate> {
    let mut candidates: Vec<WikiLinkCandidate> = note_files
        .iter()
        .map(|file| WikiLinkCandidate {
            path: file.path.clone(),
            location: if file.archived {
                WikiLinkLocation::Archives
            } else {
                WikiLinkLocation::Notes
            },
        })
        .collect();
    candidates.extend(daily_dates.iter().map(|date| WikiLinkCandidate {
        path: crate::storage::Storage::daily_file_name(*date),
        location: WikiLinkLocation::Daily,
    }));
    candidates
}

impl App {
    /// Recompute the completion popup from the current input. Purely derived:
    /// an unclosed `[[` before the cursor opens the popup, anything else
    /// closes it. An Esc dismissal stays sticky until the span or query
    /// changes, so the popup does not fight the user.
    pub(in crate::app) fn refresh_wiki_completion(&mut self) {
        let Some(span) = open_wiki_span(&self.input, self.input_cursor) else {
            self.wiki_completion = None;
            self.wiki_completion_dismissed = None;
            return;
        };
        let query = {
            // The two marker chars are ASCII, so two character indexes past
            // the span start are exactly two bytes past its byte offset.
            let start_bytes = byte_offset(&self.input, span.start) + 2;
            let cursor_bytes = byte_offset(&self.input, self.input_cursor);
            self.input[start_bytes..cursor_bytes].to_string()
        };
        if self.wiki_completion_dismissed.as_ref() == Some(&(span.start, query.clone())) {
            return;
        }
        self.wiki_completion_dismissed = None;
        let unchanged = self
            .wiki_completion
            .as_ref()
            .is_some_and(|state| state.span_start == span.start && state.query == query);
        if unchanged {
            return;
        }
        let dates = self
            .daily_notes
            .iter()
            .map(|note| note.date)
            .collect::<Vec<_>>();
        let candidates =
            filter_wiki_completions(&all_wiki_candidates(&dates, &self.note_files), &query);
        if candidates.is_empty() {
            self.wiki_completion = None;
        } else {
            self.wiki_completion = Some(WikiCompletionState {
                span_start: span.start,
                query,
                candidates,
                index: 0,
                scroll: 0,
            });
        }
    }

    /// Accept the selected candidate: splice `[[stem]]` over the open span.
    pub(in crate::app) fn accept_wiki_completion(&mut self) {
        let Some(state) = self.wiki_completion.clone() else {
            return;
        };
        let Some(stem) = state.candidates.get(state.index).and_then(stem_label) else {
            return;
        };
        let (input, cursor) =
            apply_wiki_completion(&self.input, state.span_start, self.input_cursor, &stem);
        self.input = input;
        self.input_cursor = cursor;
        self.wiki_completion = None;
        self.wiki_completion_dismissed = None;
    }

    /// Move the selection by `delta`, clamping at both ends (no wrap). Only
    /// `index` changes here; `scroll` is reconciled from it at render time.
    pub(in crate::app) fn move_wiki_completion(&mut self, delta: i32) {
        if let Some(state) = self.wiki_completion.as_mut() {
            state.index = move_index(state.index, delta, state.candidates.len());
        }
    }

    /// Close the popup for the current span until the query changes again.
    pub(in crate::app) fn dismiss_wiki_completion(&mut self) {
        if let Some(state) = self.wiki_completion.take() {
            self.wiki_completion_dismissed = Some((state.span_start, state.query));
        }
    }

    /// Clear the compose input together with any completion state.
    pub(in crate::app) fn clear_compose_input(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
        self.wiki_completion = None;
        self.wiki_completion_dismissed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(stem: &str, location: WikiLinkLocation) -> WikiLinkCandidate {
        WikiLinkCandidate {
            path: std::path::PathBuf::from(format!("{stem}.md")),
            location,
        }
    }

    #[test]
    fn span_detection_covers_marker_embed_and_closed_links() {
        assert_eq!(open_wiki_span("[[", 2), Some(WikiSpan { start: 0 }));
        assert_eq!(open_wiki_span("![[no", 5), Some(WikiSpan { start: 1 }));
        assert_eq!(
            open_wiki_span("see [[alpha]] and [[be", 20),
            Some(WikiSpan { start: 18 })
        );
        // Closed links and brackets never complete.
        assert_eq!(open_wiki_span("[[done]]", 8), None);
        assert_eq!(open_wiki_span("[[a[b", 5), None);
        // Markers on an earlier line do not complete.
        assert_eq!(open_wiki_span("[[no\ntext", 9), None);
        // `[[[` is malformed, not a fresh marker.
        assert_eq!(open_wiki_span("[[[", 3), None);
    }

    #[test]
    fn filtering_ranks_prefix_before_substring() {
        let candidates: Vec<WikiLinkCandidate> = ["alpha", "alpine", "beta", "salami", "malpa"]
            .iter()
            .map(|stem| candidate(stem, WikiLinkLocation::Notes))
            .collect();
        let ranked = filter_wiki_completions(&candidates, "al");
        let stems: Vec<_> = ranked.iter().filter_map(stem_label).collect();
        assert_eq!(stems, ["alpha", "alpine", "malpa", "salami"]);

        let case = filter_wiki_completions(&candidates, "ALP");
        assert_eq!(
            case.iter().filter_map(stem_label).collect::<Vec<_>>(),
            ["alpha", "alpine", "malpa"]
        );

        // Every match is kept: the popup window scrolls, the list never
        // truncates.
        let many: Vec<WikiLinkCandidate> = (0..20)
            .map(|index| candidate(&format!("n{index:02}"), WikiLinkLocation::Notes))
            .chain((0..5).map(|index| candidate(&format!("nx{index}"), WikiLinkLocation::Notes)))
            .collect();
        assert_eq!(filter_wiki_completions(&many, "n").len(), many.len());
        assert_eq!(filter_wiki_completions(&candidates, "").len(), 5);
        assert!(filter_wiki_completions(&candidates, "zzz").is_empty());
    }

    #[test]
    fn applying_a_completion_splices_the_marker_and_moves_the_cursor() {
        let (input, cursor) = apply_wiki_completion("see ![[al and more", 5, 9, "alpha");
        assert_eq!(input, "see ![[alpha]] and more");
        assert_eq!(cursor, "see ![[alpha]]".len());
    }

    #[test]
    fn span_and_splice_use_character_indexes_with_multibyte_text() {
        let input = "笔记链接[[查";
        let cursor = input.chars().count();
        assert_eq!(open_wiki_span(input, cursor), Some(WikiSpan { start: 4 }));
        let (spliced, cursor) = apply_wiki_completion(input, 4, cursor, "查询");
        assert_eq!(spliced, "笔记链接[[查询]]");
        assert_eq!(cursor, spliced.chars().count());
    }
}
