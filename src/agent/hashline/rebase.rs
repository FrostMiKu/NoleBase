//! Drift recovery: rebase planned edits from snapshot coordinates onto the
//! live file.
//!
//! A model authors a patch against the file content captured in the snapshot
//! named by the section tag. When the live file has drifted, the planned
//! edits still describe the ORIGINAL (base) coordinates, so they cannot be
//! fed to the streaming applier verbatim. This module maps every original
//! line the plan depends on (touched lines, replaced spans, insertion
//! anchors) through a base-to-live line map derived from `similar`, and
//! refuses to proceed when any of those lines was modified or deleted. When
//! the plan survives, all edit coordinates are rewritten onto the live
//! numbering, which preserves their relative order (the map is monotonic).

use anyhow::{bail, Result};
use similar::{DiffTag, TextDiff};

use super::{LineEdit, PlannedFile};

/// Rebase `planned` edit coordinates from `base` onto `live`.
///
/// Unchanged base lines are mapped through `TextDiff::from_lines(base, live)`
/// to their live positions; lines that were modified or deleted have no
/// mapping and fail validation when a plan depends on them. On success every
/// line coordinate in `planned` (edit spans, touched ranges, and anchors)
/// refers to the live file.
pub(crate) fn rebase_edits(base: &str, live: &str, planned: &mut PlannedFile) -> Result<()> {
    if base == live {
        return Ok(());
    }

    let base_count = line_count(base);
    let diff = TextDiff::from_lines(base, live);

    // line_map[i] = live line index of base token i (0..base_count), set only
    // for tokens that survived unchanged inside an Equal run. Modified and
    // deleted base tokens stay `None`, as does the end-of-file slot: an
    // Equal run's trailing boundary is a position in both files, but the
    // token it precedes was replaced/deleted and is not an unchanged line.
    let mut line_map: Vec<Option<usize>> = vec![None; base_count + 1];
    for op in diff.ops() {
        if op.tag() == DiffTag::Equal {
            let old = op.old_range();
            let new = op.new_range();
            for k in 0..old.len() {
                if old.start + k <= base_count {
                    line_map[old.start + k] = Some(new.start + k);
                }
            }
        }
    }
    // An empty base has a single line index (the head); it maps to the head
    // of whatever the live file has become.
    if base_count == 0 {
        line_map[0] = Some(0);
    }

    // Every line the plan depends on must still exist unchanged in live.
    for &(start, end) in &planned.touched {
        for index in start..end {
            if mapped_line(&line_map, index).is_none() {
                bail!(
                    "file changed since read at line {}; read it again before editing",
                    index + 1
                );
            }
        }
    }
    for edit in &planned.edits {
        if edit.insertion {
            // `anchor_line == 0` marks an EOF-anchored insertion whose anchor
            // lives only in the touched neighbourhood (the last line).
            if edit.anchor_line > 0 {
                let index = edit.anchor_line - 1;
                if mapped_line(&line_map, index).is_none() {
                    bail!(
                        "file changed since read at line {}; read it again before editing",
                        edit.anchor_line
                    );
                }
            }
        } else {
            for index in edit.start_line..edit.end_line_exclusive {
                if mapped_line(&line_map, index).is_none() {
                    bail!(
                        "file changed since read at line {}; read it again before editing",
                        index + 1
                    );
                }
            }
        }
    }

    // Rewrite every coordinate onto the live file. Validation above
    // guarantees every referenced token maps. A span is anchored by its
    // unchanged first token and ends right after its unchanged last token, so
    // lines inserted between base tokens in live are never swallowed.
    for edit in &mut planned.edits {
        if edit.insertion {
            // Insertions name a boundary: content goes before the unchanged
            // token at `start_line`, or after the final unchanged token when
            // the boundary is the end of the file (`start_line == base_count`).
            let live = if edit.start_line == 0 {
                0
            } else if edit.start_line < base_count {
                mapped(&line_map, edit.start_line, edit_anchor_number(edit))?
            } else {
                mapped(&line_map, edit.start_line - 1, edit_anchor_number(edit))? + 1
            };
            edit.start_line = live;
            edit.end_line_exclusive = live;
        } else {
            let start = mapped(&line_map, edit.start_line, edit_anchor_number(edit))?;
            let last = mapped(
                &line_map,
                edit.end_line_exclusive - 1,
                edit.end_line_exclusive,
            )?;
            edit.start_line = start;
            edit.end_line_exclusive = last + 1;
        }
    }
    for range in &mut planned.touched {
        if range.0 == range.1 {
            continue;
        }
        let start = mapped(&line_map, range.0, range.0 + 1)?;
        let last = mapped(&line_map, range.1 - 1, range.1)?;
        *range = (start, last + 1);
    }
    Ok(())
}

/// The live-relative line to name if a rewrite coordinate fails to map.
fn edit_anchor_number(edit: &LineEdit) -> usize {
    if edit.anchor_line > 0 {
        edit.anchor_line
    } else {
        edit.start_line
    }
}

/// The live line a base line mapped to, or `None` when the line changed.
fn mapped_line(map: &[Option<usize>], index: usize) -> Option<usize> {
    map.get(index).copied().flatten()
}

/// Rewrite one base coordinate; the `line` is the 1-based line to report if
/// the coordinate unexpectedly has no mapping.
fn mapped(map: &[Option<usize>], index: usize, line: usize) -> Result<usize> {
    match mapped_line(map, index) {
        Some(live) => Ok(live),
        None => bail!("file changed since read at line {line}; read it again before editing"),
    }
}

/// Number of lines in `text`, matching `similar`'s line tokenizer: a
/// trailing newline does not add a phantom empty line.
fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.split('\n')
        .count()
        .saturating_sub(usize::from(text.ends_with('\n')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(edits: Vec<LineEdit>, touched: Vec<(usize, usize)>) -> PlannedFile {
        PlannedFile {
            edits,
            touched,
            ..Default::default()
        }
    }

    fn replace(start_line: usize, end_line_exclusive: usize) -> LineEdit {
        LineEdit {
            start_line,
            end_line_exclusive,
            lines: vec!["X".to_string()],
            insertion: false,
            anchor_line: start_line + 1,
        }
    }

    fn insert(start_line: usize, anchor_line: usize) -> LineEdit {
        LineEdit {
            start_line,
            end_line_exclusive: start_line,
            lines: vec!["X".to_string()],
            insertion: true,
            anchor_line,
        }
    }

    #[test]
    fn noop_when_base_and_live_match() {
        let base = "a\nb\nc\nd\n";
        let mut planned = plan(vec![replace(1, 3)], vec![(1, 3)]);
        rebase_edits(base, base, &mut planned).unwrap();
        assert_eq!(planned.edits[0].start_line, 1);
        assert_eq!(planned.edits[0].end_line_exclusive, 3);
        assert_eq!(planned.touched, vec![(1, 3)]);
    }

    #[test]
    fn shifts_edits_down_when_lines_are_inserted_above() {
        let base = "a\nb\nc\nd\n";
        let live = "a\nN1\nN2\nb\nc\nd\n";
        let mut planned = plan(vec![replace(1, 3)], vec![(1, 3)]);
        rebase_edits(base, live, &mut planned).unwrap();
        // b and c moved from 0-based 1..3 to 3..5.
        assert_eq!(planned.edits[0].start_line, 3);
        assert_eq!(planned.edits[0].end_line_exclusive, 5);
        assert_eq!(planned.touched, vec![(3, 5)]);
        // The 1-based diagnostic anchor is preserved as authored.
        assert_eq!(planned.edits[0].anchor_line, 2);
    }

    #[test]
    fn shifts_edits_up_when_lines_are_deleted_above() {
        let base = "a\nb\nc\nd\n";
        let live = "b\nc\nd\n";
        let mut planned = plan(vec![replace(1, 3)], vec![(1, 3)]);
        rebase_edits(base, live, &mut planned).unwrap();
        assert_eq!(planned.edits[0].start_line, 0);
        assert_eq!(planned.edits[0].end_line_exclusive, 2);
        assert_eq!(planned.touched, vec![(0, 2)]);
    }

    #[test]
    fn eof_insertion_follows_an_appended_tail() {
        let base = "a\nb\n";
        let live = "a\nb\nX\nY\n";
        let mut planned = plan(vec![insert(2, 0)], vec![(1, 2)]);
        rebase_edits(base, live, &mut planned).unwrap();
        // The EOF gap is still after the unchanged last line.
        assert_eq!(planned.edits[0].start_line, 2);
        assert_eq!(planned.edits[0].end_line_exclusive, 2);
    }

    #[test]
    fn rejects_when_a_touched_line_was_modified() {
        let base = "a\nb\nc\n";
        let live = "a\nMOD\nc\n";
        let mut planned = plan(vec![replace(1, 3)], vec![(1, 3)]);
        let error = rebase_edits(base, live, &mut planned)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "file changed since read at line 2; read it again before editing"
        );
    }

    #[test]
    fn rejects_when_an_insertion_anchor_was_modified() {
        // Two blocks; the anchor of the second block's opener is modified
        // while its end lines survive, so only the anchor check can catch it.
        let base = "fn a() {\n  1\n}\nfn b() {\n  2\n}\n";
        let live = "fn a() {\n  1\n}\nfn B() {\n  2\n}\n";
        // AfterBlock(4) resolves to lines 4-6 in base; the neighbourhood
        // covers only the block end, so the anchor check must reject.
        let mut planned = plan(vec![insert(6, 4)], vec![(5, 6)]);
        let error = rebase_edits(base, live, &mut planned)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "file changed since read at line 4; read it again before editing"
        );
    }

    #[test]
    fn rejects_when_a_span_interior_line_was_modified() {
        let base = "a\nb\nc\nd\n";
        let live = "a\nb\nMOD\nd\n";
        let mut planned = plan(vec![replace(1, 3)], vec![(1, 3)]);
        let error = rebase_edits(base, live, &mut planned)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "file changed since read at line 3; read it again before editing"
        );
    }

    #[test]
    fn empty_base_maps_head_insertion_to_live_head() {
        let base = "";
        let live = "x\ny\n";
        let mut planned = plan(vec![insert(0, 1)], Vec::new());
        rebase_edits(base, live, &mut planned).unwrap();
        assert_eq!(planned.edits[0].start_line, 0);
        assert_eq!(planned.edits[0].end_line_exclusive, 0);
    }

    #[test]
    fn rebase_preserves_insertion_before_replacement_ordering() {
        // Base: insert before line 2 and replace lines 2-3 (same start index
        // 1); plan_section already emitted the insertion first.
        let base = "a\nb\nc\nd\n";
        let live = "a\nN\nb\nc\nd\n";
        let mut planned = plan(vec![insert(1, 2), replace(1, 3)], vec![(0, 3)]);
        rebase_edits(base, live, &mut planned).unwrap();
        assert_eq!(planned.edits.len(), 2);
        assert!(planned.edits[0].insertion);
        assert_eq!(planned.edits[0].start_line, 2);
        assert_eq!(planned.edits[0].anchor_line, 2);
        assert!(!planned.edits[1].insertion);
        assert_eq!(planned.edits[1].start_line, 2);
        assert_eq!(planned.edits[1].end_line_exclusive, 4);
    }
}
