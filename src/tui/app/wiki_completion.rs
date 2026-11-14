//! Inline `[[wiki link]]` completion for the compose input.
//!
//! While the cursor sits after an unclosed `[[` (or `![[`) on its line, the
//! compose input offers matching notes as a compact popup above the input.
//! The whole pipeline is derived from `(input, cursor)` so it cannot desync:
//! helpers here are pure, the App state only carries the filtered candidates
//! and the user's selection.

use crate::core::model::{WikiLinkCandidate, WikiLinkLocation};

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
pub struct WikiCompletionState {
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

pub(crate) use crate::core::completion::stem_label;
#[cfg(test)]
use crate::core::completion::WikiSpan;
use crate::core::completion::{
    apply_wiki_completion, byte_offset, filter_wiki_completions, open_wiki_span,
};

/// Build every completable candidate from the cached daily and note lists.
pub(in crate::tui::app) fn all_wiki_candidates(
    daily_dates: &[chrono::NaiveDate],
    note_files: &[crate::core::model::NoteFile],
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
        path: crate::core::storage::Storage::daily_file_name(*date),
        location: WikiLinkLocation::Daily,
    }));
    candidates
}

impl App {
    /// Recompute the completion popup from the current input. Purely derived:
    /// an unclosed `[[` before the cursor opens the popup, anything else
    /// closes it. An Esc dismissal stays sticky until the span or query
    /// changes, so the popup does not fight the user.
    pub(in crate::tui::app) fn refresh_wiki_completion(&mut self) {
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
    pub(in crate::tui::app) fn accept_wiki_completion(&mut self) {
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
    pub(in crate::tui::app) fn move_wiki_completion(&mut self, delta: i32) {
        if let Some(state) = self.wiki_completion.as_mut() {
            state.index = move_index(state.index, delta, state.candidates.len());
        }
    }

    /// Close the popup for the current span until the query changes again.
    pub(in crate::tui::app) fn dismiss_wiki_completion(&mut self) {
        if let Some(state) = self.wiki_completion.take() {
            self.wiki_completion_dismissed = Some((state.span_start, state.query));
        }
    }

    /// Clear the compose input together with any completion state.
    pub(in crate::tui::app) fn clear_compose_input(&mut self) {
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
    fn stems_that_would_break_wiki_syntax_are_not_offered() {
        let candidates: Vec<WikiLinkCandidate> = ["al|pha", "x]", "[y]", "#h", "ok"]
            .iter()
            .map(|stem| candidate(stem, WikiLinkLocation::Notes))
            .collect();
        let ranked = filter_wiki_completions(&candidates, "");
        assert_eq!(
            ranked.iter().filter_map(stem_label).collect::<Vec<_>>(),
            ["ok"]
        );
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
