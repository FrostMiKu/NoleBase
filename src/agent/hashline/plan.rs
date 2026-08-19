//! Lowering of parsed hashline sections into streaming line edits.
//!
//! One parsed `[PATH#TAG]` section is lowered against the ORIGINAL file lines:
//! hunk line numbers are 1-based and refer to the file as the model read it;
//! earlier hunks never shift the coordinates of later hunks. The emitted
//! [`LineEdit`]s use 0-based coordinates and are sorted so the streaming
//! applier in `src/agent/tools/file_edit.rs::prepare_edit` can drain them in
//! a single pass: at any line index insertions are drained before the
//! replacement at the same index, and insertions at the exclusive end of a
//! replacement follow it naturally.

use anyhow::{bail, Result};

use super::block::{find_enclosing_block, find_next_block, resolve_block, Syntax};
use super::registers::RegisterBank;
use super::{GapLocator, LineEdit, Op, Payload, PutLocator, Section, SpanLocator};

/// A whole-file operation captured by a section, applied after its line edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FileOp {
    Remove,
    Move { dest: String },
}

/// One section lowered against the original file: the sorted line edits, the
/// original lines they touch (0-based half-open, for the read-coverage gate),
/// an optional whole-file operation, and the diagnostics worth echoing back.
#[derive(Debug, Default)]
pub(crate) struct PlannedFile {
    pub(crate) edits: Vec<LineEdit>,
    pub(crate) touched: Vec<(usize, usize)>,
    pub(crate) file_op: Option<FileOp>,
    pub(crate) warnings: Vec<String>,
    pub(crate) resolutions: Vec<String>,
}

/// An edit plus the patch-text line of the hunk that produced it, so overlap
/// and ordering diagnostics can name the offending hunk.
struct LocatedEdit {
    hunk: usize,
    edit: LineEdit,
}

/// Lower one parsed section into original-coordinate line edits.
///
/// `lines` are the ORIGINAL file lines (1-based indexing for the anchors,
/// 0-based indexing inside the emitted edits). `syntax` selects the lexical
/// family used for `N*` block anchors; `registers` carries session-scoped
/// captures across hunks and `edit` calls.
pub(crate) fn plan_section(
    section: &Section,
    lines: &[String],
    syntax: Syntax,
    registers: &RegisterBank,
) -> Result<PlannedFile> {
    let total = lines.len();
    let mut edits: Vec<LocatedEdit> = Vec::new();
    let mut touched: Vec<(usize, usize)> = Vec::new();
    let mut planned = PlannedFile::default();

    for hunk in &section.hunks {
        let line_num = hunk.line_num;
        match &hunk.op {
            Op::Put { locator, payload } => {
                if matches!(planned.file_op, Some(FileOp::Remove)) {
                    bail!("line {line_num}: REM cannot be combined with line edits");
                }
                let body = match payload {
                    Payload::Body(rows) => rows.clone(),
                    Payload::Register(name) => load_register(registers, line_num, name)?,
                };
                let (start, end, anchor, span, resolution) = match locator {
                    PutLocator::Span(SpanLocator::Range { start, end }) => {
                        check_line(line_num, *start, total)?;
                        check_line(line_num, *end, total)?;
                        if start > end {
                            bail!("line {line_num}: invalid range {start}.={end}");
                        }
                        (*start - 1, *end, *start, true, None)
                    }
                    PutLocator::Span(SpanLocator::Block(n)) => {
                        check_line(line_num, *n, total)?;
                        let (s, e) = resolve_block_strict(line_num, *n, lines, syntax)?;
                        (
                            s - 1,
                            e,
                            *n,
                            true,
                            Some(format!("PUT {n}* resolved to lines {s}-{e}")),
                        )
                    }
                    PutLocator::Gap(GapLocator::Before(n)) => {
                        check_gap_before(line_num, *n, total)?;
                        let start = *n - 1;
                        (start, start, *n, false, None)
                    }
                    PutLocator::Gap(GapLocator::After(n)) => {
                        check_line(line_num, *n, total)?;
                        (*n, *n, *n, false, None)
                    }
                    PutLocator::Gap(GapLocator::AfterBlock(n)) => {
                        check_line(line_num, *n, total)?;
                        match resolve_block(lines, syntax, *n) {
                            Some((s, e)) if s != e => (e, e, *n, false, None),
                            _ => {
                                planned.warnings.push(format!(
                                    "PUT >{n}* could not resolve a block; inserted after line {n} instead"
                                ));
                                (*n, *n, *n, false, None)
                            }
                        }
                    }
                    PutLocator::Gap(GapLocator::Eof) => {
                        // No 1-based anchor line exists past the file end;
                        // `anchor_line == 0` marks an EOF-anchored insertion.
                        (total, total, 0, false, None)
                    }
                };
                if let Some(resolution) = resolution {
                    planned.resolutions.push(resolution);
                }
                edits.push(LocatedEdit {
                    hunk: line_num,
                    edit: LineEdit {
                        start_line: start,
                        end_line_exclusive: end,
                        lines: body,
                        insertion: !span,
                        anchor_line: anchor,
                    },
                });
                push_touched(&mut touched, start, end, span, total);
            }
            Op::Cut { locator, register } => {
                if matches!(planned.file_op, Some(FileOp::Remove)) {
                    bail!("line {line_num}: REM cannot be combined with line edits");
                }
                let (start, end, anchor, resolution) = match locator {
                    SpanLocator::Range { start, end } => {
                        check_line(line_num, *start, total)?;
                        check_line(line_num, *end, total)?;
                        if start > end {
                            bail!("line {line_num}: invalid range {start}.={end}");
                        }
                        (*start - 1, *end, *start, None)
                    }
                    SpanLocator::Block(n) => {
                        check_line(line_num, *n, total)?;
                        let (s, e) = resolve_block_strict(line_num, *n, lines, syntax)?;
                        (
                            s - 1,
                            e,
                            *n,
                            Some(format!("CUT {n}* resolved to lines {s}-{e}")),
                        )
                    }
                };
                if let Some(resolution) = resolution {
                    planned.resolutions.push(resolution);
                }
                // Capture a COPY of the original lines into the register,
                // then emit a deleting edit with an empty body.
                let captured = lines[start..end].to_vec();
                registers.store(register.as_deref(), captured)?;
                edits.push(LocatedEdit {
                    hunk: line_num,
                    edit: LineEdit {
                        start_line: start,
                        end_line_exclusive: end,
                        lines: Vec::new(),
                        insertion: false,
                        anchor_line: anchor,
                    },
                });
                push_touched(&mut touched, start, end, true, total);
            }
            Op::Rem => {
                if !edits.is_empty() {
                    bail!("line {line_num}: REM cannot be combined with line edits");
                }
                if planned.file_op.is_some() {
                    bail!(
                        "line {line_num}: multiple file-level operations are not allowed in one section"
                    );
                }
                planned.file_op = Some(FileOp::Remove);
            }
            Op::Mv { dest } => {
                if planned.file_op.is_some() {
                    bail!(
                        "line {line_num}: multiple file-level operations are not allowed in one section"
                    );
                }
                planned.file_op = Some(FileOp::Move { dest: dest.clone() });
            }
        }
    }

    sort_and_validate(&mut edits)?;

    planned.edits = edits.into_iter().map(|located| located.edit).collect();
    planned.touched = merge_ranges(touched);
    Ok(planned)
}

/// Require a 1-based line to name an existing line of the original file.
fn check_line(line_num: usize, n: usize, total: usize) -> Result<()> {
    if n == 0 || n > total {
        bail!("line {line_num}: line {n} is past the end of the file ({total} lines)");
    }
    Ok(())
}

/// `PUT <N:` inserts before line N; on an empty file only N == 1 is legal
/// (the file head), so the anchor may name the line that would follow the
/// insertion even when the file is empty.
fn check_gap_before(line_num: usize, n: usize, total: usize) -> Result<()> {
    if n == 0 || (n > total && !(total == 0 && n == 1)) {
        bail!("line {line_num}: line {n} is past the end of the file ({total} lines)");
    }
    Ok(())
}

/// Resolve a `N*` block anchor with the strict error texts. A bare single
/// statement and an unresolvable anchor are both hard errors here.
fn resolve_block_strict(
    line_num: usize,
    n: usize,
    lines: &[String],
    syntax: Syntax,
) -> Result<(usize, usize)> {
    match resolve_block(lines, syntax, n) {
        Some((s, e)) if s == e => bail!(
            "line {line_num}: line {n} is a single statement, not a block opener; use PUT {n}.={n}: instead"
        ),
        Some((s, e)) => Ok((s, e)),
        None => {
            let hint = find_next_block(lines, syntax, n)
                .or_else(|| find_enclosing_block(lines, syntax, n))
                .map(|(s, e)| format!(" (nearest block starts at line {s} and ends at line {e})"))
                .unwrap_or_default();
            bail!("line {line_num}: no block begins at line {n}{hint}");
        }
    }
}

/// Load a register capture, rejecting empty or never-stored registers.
fn load_register(
    registers: &RegisterBank,
    line_num: usize,
    name: &Option<String>,
) -> Result<Vec<String>> {
    let loaded = registers.load(name.as_deref())?.unwrap_or_default();
    if loaded.is_empty() {
        match name {
            Some(name) => bail!("line {line_num}: register {name} is empty"),
            None => bail!("line {line_num}: the anonymous register is empty"),
        }
    }
    Ok(loaded)
}

/// Record the original lines a hunk needs the model to have read. Spans
/// require the whole replaced range; insertions require the anchor
/// neighbourhood so drift detection can prove the anchor still exists.
fn push_touched(
    touched: &mut Vec<(usize, usize)>,
    start: usize,
    end: usize,
    span: bool,
    total: usize,
) {
    if span {
        touched.push((start, end));
    } else {
        touched.push((start.saturating_sub(1), (start + 1).min(total)));
    }
}

/// Merge overlapping or adjacent touched ranges into sorted, disjoint spans.
fn merge_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if start >= end {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

/// Sort edits into the order the streaming applier drains them, then reject
/// overlapping spans and insertions that fall inside a replaced span.
///
/// Sorting by `(start_line, end_line_exclusive)` makes an insertion at a line
/// index sort before the replacement at that same index (its exclusive end
/// equals its start), matching `prepare_edit`, which drains insertions at a
/// line index before applying the replacement there. Insertions at the
/// exclusive end of a replacement sort after it, where the applier's
/// loop-top drain picks them up next.
fn sort_and_validate(edits: &mut Vec<LocatedEdit>) -> Result<()> {
    edits.sort_by_key(|located| (located.edit.start_line, located.edit.end_line_exclusive));

    let mut previous_span: Option<&LocatedEdit> = None;
    for located in edits.iter() {
        if located.edit.insertion {
            // An insertion at the boundary of a replaced span is allowed
            // (shared start line, or the span's exclusive end), but one that
            // falls strictly inside would be skipped by the streaming
            // applier, so it is rejected up front.
            if let Some(previous) = previous_span {
                if located.edit.start_line > previous.edit.start_line
                    && located.edit.start_line < previous.edit.end_line_exclusive
                {
                    bail!(
                        "line {}: hunks overlap at line {}",
                        located.hunk,
                        located.edit.anchor_line
                    );
                }
            }
        } else if let Some(previous) = previous_span {
            if located.edit.start_line < previous.edit.end_line_exclusive {
                bail!(
                    "line {}: hunks overlap at line {}",
                    located.hunk,
                    located.edit.start_line + 1
                );
            }
            previous_span = Some(located);
        } else {
            previous_span = Some(located);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::hashline::{Hunk, RegisterBank};

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|row| row.to_string()).collect()
    }

    fn section(hunks: Vec<Hunk>) -> Section {
        Section {
            path: "test.file".to_string(),
            tag: "AAAA".to_string(),
            line_num: 1,
            hunks,
        }
    }

    fn put_range(line_num: usize, start: usize, end: usize, rows: Vec<String>) -> Hunk {
        Hunk {
            line_num,
            op: Op::Put {
                locator: PutLocator::Span(SpanLocator::Range { start, end }),
                payload: Payload::Body(rows),
            },
        }
    }

    fn block_put(line_num: usize, n: usize, rows: Vec<String>) -> Hunk {
        Hunk {
            line_num,
            op: Op::Put {
                locator: PutLocator::Span(SpanLocator::Block(n)),
                payload: Payload::Body(rows),
            },
        }
    }

    fn gap_put(line_num: usize, gap: GapLocator, rows: Vec<String>) -> Hunk {
        Hunk {
            line_num,
            op: Op::Put {
                locator: PutLocator::Gap(gap),
                payload: Payload::Body(rows),
            },
        }
    }

    #[test]
    fn lowers_range_replace() {
        let planned = plan_section(
            &section(vec![put_range(4, 2, 3, vec!["replacement".to_string()])]),
            &lines(&["one", "two", "three", "four"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(
            planned.edits,
            vec![LineEdit {
                start_line: 1,
                end_line_exclusive: 3,
                lines: vec!["replacement".to_string()],
                insertion: false,
                anchor_line: 2,
            }]
        );
        assert_eq!(planned.touched, vec![(1, 3)]);
        assert!(planned.resolutions.is_empty());
        assert!(planned.warnings.is_empty());
        assert!(planned.file_op.is_none());
    }

    #[test]
    fn lowers_range_delete_with_empty_body() {
        let planned = plan_section(
            &section(vec![put_range(5, 2, 3, vec![])]),
            &lines(&["one", "two", "three", "four"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits.len(), 1);
        assert_eq!(planned.edits[0].start_line, 1);
        assert_eq!(planned.edits[0].end_line_exclusive, 3);
        assert!(planned.edits[0].lines.is_empty());
        assert!(!planned.edits[0].insertion);
    }

    #[test]
    fn lowers_insert_before() {
        let planned = plan_section(
            &section(vec![gap_put(
                6,
                GapLocator::Before(2),
                vec!["x".to_string()],
            )]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(
            planned.edits,
            vec![LineEdit {
                start_line: 1,
                end_line_exclusive: 1,
                lines: vec!["x".to_string()],
                insertion: true,
                anchor_line: 2,
            }]
        );
        // Anchor line 2 (0-based 1) plus its neighbours.
        assert_eq!(planned.touched, vec![(0, 2)]);
    }

    #[test]
    fn lowers_insert_after() {
        let planned = plan_section(
            &section(vec![gap_put(
                7,
                GapLocator::After(2),
                vec!["x".to_string()],
            )]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits[0].start_line, 2);
        assert_eq!(planned.edits[0].end_line_exclusive, 2);
        assert!(planned.edits[0].insertion);
        assert_eq!(planned.edits[0].anchor_line, 2);
        assert_eq!(planned.touched, vec![(1, 3)]);
    }

    #[test]
    fn lowers_eof_append() {
        let planned = plan_section(
            &section(vec![gap_put(8, GapLocator::Eof, vec!["tail".to_string()])]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits[0].start_line, 3);
        assert_eq!(planned.edits[0].end_line_exclusive, 3);
        assert!(planned.edits[0].insertion);
        assert_eq!(planned.edits[0].anchor_line, 0);
        // The neighbourhood clamps to the last line.
        assert_eq!(planned.touched, vec![(2, 3)]);
    }

    #[test]
    fn lower_block_replace_resolves_span() {
        let planned = plan_section(
            &section(vec![block_put(9, 1, vec!["new body".to_string()])]),
            &lines(&["fn main() {", "    let x = 1;", "}"]),
            Syntax::Braces,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits.len(), 1);
        assert_eq!(planned.edits[0].start_line, 0);
        assert_eq!(planned.edits[0].end_line_exclusive, 3);
        assert_eq!(planned.edits[0].lines, vec!["new body".to_string()]);
        assert!(!planned.edits[0].insertion);
        assert_eq!(planned.edits[0].anchor_line, 1);
        assert_eq!(planned.resolutions, vec!["PUT 1* resolved to lines 1-3"]);
        assert_eq!(planned.touched, vec![(0, 3)]);
    }

    #[test]
    fn block_anchor_single_statement_is_an_error() {
        let error = plan_section(
            &section(vec![block_put(10, 1, vec!["x".to_string()])]),
            &lines(&["let x = 1;"]),
            Syntax::Braces,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "line 10: line 1 is a single statement, not a block opener; use PUT 1.=1: instead"
        );
    }

    #[test]
    fn block_anchor_unresolvable_reports_nearest_next_block() {
        let error = plan_section(
            &section(vec![block_put(11, 1, vec!["x".to_string()])]),
            &lines(&["", "fn foo() {", "    y();", "}"]),
            Syntax::Braces,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "line 11: no block begins at line 1 (nearest block starts at line 2 and ends at line 4)"
        );
    }

    #[test]
    fn block_anchor_unresolvable_reports_nearest_enclosing_block() {
        let error = plan_section(
            &section(vec![block_put(12, 4, vec!["x".to_string()])]),
            &lines(&["fn foo() {", "    y();", "}", ""]),
            Syntax::Braces,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "line 12: no block begins at line 4 (nearest block starts at line 1 and ends at line 3)"
        );
    }

    #[test]
    fn block_anchor_unresolvable_without_hint() {
        let error = plan_section(
            &section(vec![block_put(13, 1, vec!["x".to_string()])]),
            &lines(&["let x = 1;"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(error, "line 13: no block begins at line 1");
    }

    #[test]
    fn after_block_inserts_after_resolved_span() {
        let planned = plan_section(
            &section(vec![gap_put(
                14,
                GapLocator::AfterBlock(1),
                vec!["after".to_string()],
            )]),
            &lines(&["fn main() {", "    let x = 1;", "}"]),
            Syntax::Braces,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits[0].start_line, 3);
        assert_eq!(planned.edits[0].end_line_exclusive, 3);
        assert!(planned.edits[0].insertion);
        assert_eq!(planned.edits[0].anchor_line, 1);
        assert!(planned.warnings.is_empty());
    }

    #[test]
    fn after_block_degrades_with_warning_on_single_statement() {
        let planned = plan_section(
            &section(vec![gap_put(
                15,
                GapLocator::AfterBlock(1),
                vec!["after".to_string()],
            )]),
            &lines(&["let x = 1;"]),
            Syntax::Braces,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits[0].start_line, 1);
        assert_eq!(planned.edits[0].end_line_exclusive, 1);
        assert!(planned.edits[0].insertion);
        assert_eq!(
            planned.warnings,
            vec!["PUT >1* could not resolve a block; inserted after line 1 instead"]
        );
    }

    #[test]
    fn after_block_degrades_with_warning_on_unresolvable_anchor() {
        let planned = plan_section(
            &section(vec![gap_put(
                16,
                GapLocator::AfterBlock(1),
                vec!["after".to_string()],
            )]),
            &lines(&["", "fn foo() {", "}"]),
            Syntax::Braces,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits[0].start_line, 1);
        assert!(planned.edits[0].insertion);
        assert_eq!(
            planned.warnings,
            vec!["PUT >1* could not resolve a block; inserted after line 1 instead"]
        );
    }

    #[test]
    fn register_paste_round_trips_with_cut_capture() {
        let registers = RegisterBank::default();
        let planned = plan_section(
            &section(vec![
                Hunk {
                    line_num: 20,
                    op: Op::Cut {
                        locator: SpanLocator::Range { start: 1, end: 2 },
                        register: Some("keep".to_string()),
                    },
                },
                Hunk {
                    line_num: 21,
                    op: Op::Put {
                        locator: PutLocator::Gap(GapLocator::Before(3)),
                        payload: Payload::Register(Some("keep".to_string())),
                    },
                },
            ]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &registers,
        )
        .unwrap();
        assert_eq!(planned.edits.len(), 2);
        // Cut deletes lines 1-2 with an empty body...
        assert_eq!(planned.edits[0].start_line, 0);
        assert_eq!(planned.edits[0].end_line_exclusive, 2);
        assert!(planned.edits[0].lines.is_empty());
        assert!(!planned.edits[0].insertion);
        // ...and the paste re-inserts the captured lines before line 3.
        assert_eq!(planned.edits[1].start_line, 2);
        assert_eq!(planned.edits[1].end_line_exclusive, 2);
        assert!(planned.edits[1].insertion);
        assert_eq!(
            planned.edits[1].lines,
            vec!["one".to_string(), "two".to_string()]
        );
        // The captured copy survives outside the section as well.
        assert_eq!(
            registers.load(Some("keep")).unwrap(),
            Some(vec!["one".to_string(), "two".to_string()])
        );
    }

    #[test]
    fn anonymous_register_round_trip() {
        let registers = RegisterBank::default();
        let planned = plan_section(
            &section(vec![
                Hunk {
                    line_num: 22,
                    op: Op::Cut {
                        locator: SpanLocator::Range { start: 1, end: 1 },
                        register: None,
                    },
                },
                Hunk {
                    line_num: 23,
                    op: Op::Put {
                        locator: PutLocator::Gap(GapLocator::Eof),
                        payload: Payload::Register(None),
                    },
                },
            ]),
            &lines(&["one", "two"]),
            Syntax::Unknown,
            &registers,
        )
        .unwrap();
        assert_eq!(planned.edits[1].lines, vec!["one".to_string()]);
        assert!(planned.edits[1].insertion);
    }

    #[test]
    fn empty_named_register_is_an_error() {
        let registers = RegisterBank::default();
        let error = plan_section(
            &section(vec![Hunk {
                line_num: 24,
                op: Op::Put {
                    locator: PutLocator::Gap(GapLocator::Before(1)),
                    payload: Payload::Register(Some("ghost".to_string())),
                },
            }]),
            &lines(&["one", "two"]),
            Syntax::Unknown,
            &registers,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(error, "line 24: register ghost is empty");
    }

    #[test]
    fn empty_anonymous_register_is_an_error() {
        let registers = RegisterBank::default();
        let error = plan_section(
            &section(vec![Hunk {
                line_num: 25,
                op: Op::Put {
                    locator: PutLocator::Gap(GapLocator::Eof),
                    payload: Payload::Register(None),
                },
            }]),
            &lines(&["one", "two"]),
            Syntax::Unknown,
            &registers,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(error, "line 25: the anonymous register is empty");
    }

    #[test]
    fn overlapping_spans_are_rejected() {
        let error = plan_section(
            &section(vec![
                put_range(30, 1, 3, vec!["a".to_string()]),
                put_range(31, 3, 4, vec!["b".to_string()]),
            ]),
            &lines(&["one", "two", "three", "four"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(error, "line 31: hunks overlap at line 3");
    }

    #[test]
    fn adjacent_spans_are_allowed() {
        let planned = plan_section(
            &section(vec![
                put_range(32, 1, 2, vec!["a".to_string()]),
                put_range(33, 3, 4, vec!["b".to_string()]),
            ]),
            &lines(&["one", "two", "three", "four"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits.len(), 2);
        assert_eq!(planned.edits[0].start_line, 0);
        assert_eq!(planned.edits[0].end_line_exclusive, 2);
        assert_eq!(planned.edits[1].start_line, 2);
        assert_eq!(planned.edits[1].end_line_exclusive, 4);
    }

    #[test]
    fn insertion_strictly_inside_span_is_rejected() {
        let error = plan_section(
            &section(vec![
                put_range(34, 1, 3, vec!["a".to_string()]),
                gap_put(35, GapLocator::Before(2), vec!["x".to_string()]),
            ]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(error, "line 35: hunks overlap at line 2");
    }

    #[test]
    fn insertion_at_span_boundaries_is_allowed() {
        // Insert before line 1 (span start) and after line 3 (span end).
        let planned = plan_section(
            &section(vec![
                put_range(36, 1, 3, vec!["a".to_string()]),
                gap_put(37, GapLocator::Before(1), vec!["head".to_string()]),
                gap_put(38, GapLocator::After(3), vec!["tail".to_string()]),
            ]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        // Sorted: head insertion, span, then tail insertion.
        assert_eq!(planned.edits.len(), 3);
        assert!(planned.edits[0].insertion);
        assert_eq!(planned.edits[0].start_line, 0);
        assert!(!planned.edits[1].insertion);
        assert_eq!(planned.edits[1].start_line, 0);
        assert_eq!(planned.edits[1].end_line_exclusive, 3);
        assert!(planned.edits[2].insertion);
        assert_eq!(planned.edits[2].start_line, 3);
    }

    #[test]
    fn insertion_sorts_before_replacement_at_shared_line() {
        // `PUT <2:` (start 1) and `PUT 2.=2:` (start 1) share a start line.
        let planned = plan_section(
            &section(vec![
                put_range(40, 2, 2, vec!["replacement".to_string()]),
                gap_put(41, GapLocator::Before(2), vec!["before".to_string()]),
            ]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits.len(), 2);
        assert!(planned.edits[0].insertion);
        assert_eq!(planned.edits[0].start_line, 1);
        assert_eq!(planned.edits[0].lines, vec!["before".to_string()]);
        assert!(!planned.edits[1].insertion);
        assert_eq!(planned.edits[1].start_line, 1);
        assert_eq!(planned.edits[1].end_line_exclusive, 2);
        // The order matches what prepare_edit drains: insertion first.
        assert_eq!(planned.touched, vec![(0, 2)]);
    }

    #[test]
    fn out_of_bounds_span_is_rejected() {
        let error = plan_section(
            &section(vec![put_range(50, 10, 10, vec!["x".to_string()])]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "line 50: line 10 is past the end of the file (3 lines)"
        );
    }

    #[test]
    fn out_of_bounds_range_end_is_rejected() {
        let error = plan_section(
            &section(vec![put_range(51, 2, 9, vec!["x".to_string()])]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "line 51: line 9 is past the end of the file (3 lines)"
        );
    }

    #[test]
    fn insert_after_last_line_is_valid() {
        let planned = plan_section(
            &section(vec![gap_put(
                52,
                GapLocator::After(3),
                vec!["x".to_string()],
            )]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits[0].start_line, 3);
        assert!(planned.edits[0].insertion);
    }

    #[test]
    fn insert_before_past_end_is_rejected() {
        let error = plan_section(
            &section(vec![gap_put(
                53,
                GapLocator::Before(5),
                vec!["x".to_string()],
            )]),
            &lines(&["one", "two", "three"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "line 53: line 5 is past the end of the file (3 lines)"
        );
    }

    #[test]
    fn empty_file_accepts_only_head_and_eof_inserts() {
        let bank = RegisterBank::default();
        let head = plan_section(
            &section(vec![gap_put(
                54,
                GapLocator::Before(1),
                vec!["x".to_string()],
            )]),
            &[],
            Syntax::Unknown,
            &bank,
        )
        .unwrap();
        assert_eq!(head.edits[0].start_line, 0);
        assert!(head.edits[0].insertion);
        assert!(head.touched.is_empty());

        let eof = plan_section(
            &section(vec![gap_put(55, GapLocator::Eof, vec!["x".to_string()])]),
            &[],
            Syntax::Unknown,
            &bank,
        )
        .unwrap();
        assert_eq!(eof.edits[0].start_line, 0);
        assert!(eof.edits[0].insertion);

        let span = plan_section(
            &section(vec![put_range(56, 1, 1, vec!["x".to_string()])]),
            &[],
            Syntax::Unknown,
            &bank,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            span,
            "line 56: line 1 is past the end of the file (0 lines)"
        );

        let after = plan_section(
            &section(vec![gap_put(
                57,
                GapLocator::After(1),
                vec!["x".to_string()],
            )]),
            &[],
            Syntax::Unknown,
            &bank,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            after,
            "line 57: line 1 is past the end of the file (0 lines)"
        );
    }

    #[test]
    fn edge_boundary_before_is_valid_on_empty_file() {
        let planned = plan_section(
            &section(vec![gap_put(
                58,
                GapLocator::Before(1),
                vec!["x".to_string()],
            )]),
            &[],
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.edits[0].anchor_line, 1);
        assert_eq!(planned.edits[0].start_line, 0);
    }

    #[test]
    fn cut_block_resolves_and_records_resolution() {
        let registers = RegisterBank::default();
        let planned = plan_section(
            &section(vec![Hunk {
                line_num: 60,
                op: Op::Cut {
                    locator: SpanLocator::Block(1),
                    register: Some("block".to_string()),
                },
            }]),
            &lines(&["fn main() {", "    let x = 1;", "}"]),
            Syntax::Braces,
            &registers,
        )
        .unwrap();
        assert_eq!(planned.edits[0].start_line, 0);
        assert_eq!(planned.edits[0].end_line_exclusive, 3);
        assert!(planned.edits[0].lines.is_empty());
        assert_eq!(planned.resolutions, vec!["CUT 1* resolved to lines 1-3"]);
        assert_eq!(
            registers.load(Some("block")).unwrap(),
            Some(lines(&["fn main() {", "    let x = 1;", "}"]))
        );
    }

    #[test]
    fn rem_alone_sets_remove_op() {
        let planned = plan_section(
            &section(vec![Hunk {
                line_num: 70,
                op: Op::Rem,
            }]),
            &lines(&["one", "two"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(planned.file_op, Some(FileOp::Remove));
        assert!(planned.edits.is_empty());
    }

    #[test]
    fn rem_rejects_combined_line_edits() {
        let error = plan_section(
            &section(vec![
                put_range(71, 1, 1, vec!["x".to_string()]),
                Hunk {
                    line_num: 72,
                    op: Op::Rem,
                },
            ]),
            &lines(&["one", "two"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(error, "line 72: REM cannot be combined with line edits");
    }

    #[test]
    fn rem_rejects_edits_after_it() {
        let error = plan_section(
            &section(vec![
                Hunk {
                    line_num: 73,
                    op: Op::Rem,
                },
                put_range(74, 1, 1, vec!["x".to_string()]),
            ]),
            &lines(&["one", "two"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(error, "line 74: REM cannot be combined with line edits");
    }

    #[test]
    fn rem_and_move_are_exclusive() {
        let error = plan_section(
            &section(vec![
                Hunk {
                    line_num: 75,
                    op: Op::Rem,
                },
                Hunk {
                    line_num: 76,
                    op: Op::Mv {
                        dest: "elsewhere.md".to_string(),
                    },
                },
            ]),
            &lines(&["one", "two"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            error,
            "line 76: multiple file-level operations are not allowed in one section"
        );
    }

    #[test]
    fn move_allows_line_edits() {
        let planned = plan_section(
            &section(vec![
                put_range(77, 1, 1, vec!["replacement".to_string()]),
                Hunk {
                    line_num: 78,
                    op: Op::Mv {
                        dest: "elsewhere.md".to_string(),
                    },
                },
            ]),
            &lines(&["one", "two"]),
            Syntax::Unknown,
            &RegisterBank::default(),
        )
        .unwrap();
        assert_eq!(
            planned.file_op,
            Some(FileOp::Move {
                dest: "elsewhere.md".to_string()
            })
        );
        assert_eq!(planned.edits.len(), 1);
    }
}
