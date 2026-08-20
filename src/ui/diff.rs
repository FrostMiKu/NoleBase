use super::*;
use similar::{ChangeTag, TextDiff};

// Diff lines are rebuilt while the approval is visible, so keep refinement
// work bounded and fall back to the existing whole-line treatment beyond it.
const MAX_ALIGNMENT_CANDIDATES: usize = 64;
pub(super) const MAX_INTRALINE_BYTES: usize = 512;
const LINE_PAIR_SCORE: f32 = 2.0;

#[derive(Clone, Copy)]
enum AlignmentStep {
    Before,
    After,
    Pair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DiffLineKind {
    Context,
    Deletion,
    Addition,
    Header,
    Hunk,
    Metadata,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SideBySideDiffRow<'a> {
    Full(&'a str, DiffLineKind),
    Columns {
        before: Option<SideBySideDiffCell<'a>>,
        after: Option<SideBySideDiffCell<'a>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SideBySideDiffCell<'a> {
    pub(super) text: &'a str,
    pub(super) kind: DiffLineKind,
    pub(super) line_number: Option<usize>,
}

impl<'a> SideBySideDiffCell<'a> {
    pub(super) fn new(text: &'a str, kind: DiffLineKind, line_number: Option<usize>) -> Self {
        Self {
            text,
            kind,
            line_number,
        }
    }
}

pub(super) fn side_by_side_diff_rows(diff: &str) -> Vec<SideBySideDiffRow<'_>> {
    let lines = diff.lines().collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut index = 0;
    let mut in_hunk = false;
    let mut before_line = None;
    let mut after_line = None;

    while index < lines.len() {
        let line = lines[index];
        if !in_hunk
            && line.starts_with("--- ")
            && lines
                .get(index + 1)
                .is_some_and(|next| next.starts_with("+++ "))
        {
            rows.push(SideBySideDiffRow::Columns {
                before: Some(SideBySideDiffCell::new(line, DiffLineKind::Header, None)),
                after: Some(SideBySideDiffCell::new(
                    lines[index + 1],
                    DiffLineKind::Header,
                    None,
                )),
            });
            index += 2;
            continue;
        }

        if line.starts_with("@@") {
            rows.push(SideBySideDiffRow::Full(line, DiffLineKind::Hunk));
            in_hunk = true;
            (before_line, after_line) = parse_diff_hunk_starts(line)
                .map(|(before, after)| (Some(before), Some(after)))
                .unwrap_or((None, None));
            index += 1;
            continue;
        }

        if is_changed_diff_line(line, in_hunk) {
            let start = index;
            while index < lines.len() && is_changed_diff_line(lines[index], in_hunk) {
                index += 1;
            }
            let block = &lines[start..index];
            let before = block
                .iter()
                .copied()
                .filter(|line| line.starts_with('-'))
                .collect::<Vec<_>>();
            let after = block
                .iter()
                .copied()
                .filter(|line| line.starts_with('+'))
                .collect::<Vec<_>>();
            let before_start = before_line;
            let after_start = after_line;
            for (before_index, after_index) in align_changed_lines(&before, &after) {
                rows.push(SideBySideDiffRow::Columns {
                    before: before_index.map(|line_index| {
                        SideBySideDiffCell::new(
                            before[line_index],
                            DiffLineKind::Deletion,
                            before_start.map(|line| line + line_index),
                        )
                    }),
                    after: after_index.map(|line_index| {
                        SideBySideDiffCell::new(
                            after[line_index],
                            DiffLineKind::Addition,
                            after_start.map(|line| line + line_index),
                        )
                    }),
                });
            }
            before_line = before_line.map(|line| line + before.len());
            after_line = after_line.map(|line| line + after.len());
            continue;
        }

        let kind = if line.starts_with(' ') {
            DiffLineKind::Context
        } else {
            DiffLineKind::Metadata
        };
        if kind == DiffLineKind::Context {
            let before_number = before_line;
            let after_number = after_line;
            rows.push(SideBySideDiffRow::Columns {
                before: Some(SideBySideDiffCell::new(line, kind, before_number)),
                after: Some(SideBySideDiffCell::new(line, kind, after_number)),
            });
            before_line = before_line.map(|line| line + 1);
            after_line = after_line.map(|line| line + 1);
        } else {
            rows.push(SideBySideDiffRow::Full(line, kind));
        }
        if line.is_empty() || line.starts_with("diff ") {
            in_hunk = false;
            before_line = None;
            after_line = None;
        }
        index += 1;
    }

    rows
}

fn align_changed_lines(before: &[&str], after: &[&str]) -> Vec<(Option<usize>, Option<usize>)> {
    if before.is_empty() {
        return (0..after.len()).map(|index| (None, Some(index))).collect();
    }
    if after.is_empty() {
        return (0..before.len()).map(|index| (Some(index), None)).collect();
    }
    if before.len().saturating_mul(after.len()) > MAX_ALIGNMENT_CANDIDATES
        || before
            .iter()
            .chain(after)
            .any(|line| line.len() > MAX_INTRALINE_BYTES)
    {
        return ordinal_alignment(before.len(), after.len());
    }

    let mut scores = vec![vec![0.0f32; after.len() + 1]; before.len() + 1];
    let mut steps = vec![vec![AlignmentStep::Before; after.len() + 1]; before.len() + 1];
    for step in &mut steps[0][1..] {
        *step = AlignmentStep::After;
    }
    for old_index in 1..=before.len() {
        for new_index in 1..=after.len() {
            let mut score = scores[old_index - 1][new_index];
            let mut step = AlignmentStep::Before;
            if scores[old_index][new_index - 1] > score {
                score = scores[old_index][new_index - 1];
                step = AlignmentStep::After;
            }
            let similarity = changed_line_similarity(before[old_index - 1], after[new_index - 1]);
            let paired = scores[old_index - 1][new_index - 1] + LINE_PAIR_SCORE + similarity;
            if paired > score {
                score = paired;
                step = AlignmentStep::Pair;
            }
            scores[old_index][new_index] = score;
            steps[old_index][new_index] = step;
        }
    }

    let mut aligned = Vec::with_capacity(before.len().max(after.len()));
    let mut old_index = before.len();
    let mut new_index = after.len();
    while old_index > 0 || new_index > 0 {
        match steps[old_index][new_index] {
            AlignmentStep::Pair if old_index > 0 && new_index > 0 => {
                old_index -= 1;
                new_index -= 1;
                aligned.push((Some(old_index), Some(new_index)));
            }
            AlignmentStep::Before if old_index > 0 => {
                old_index -= 1;
                aligned.push((Some(old_index), None));
            }
            _ => {
                new_index -= 1;
                aligned.push((None, Some(new_index)));
            }
        }
    }
    aligned.reverse();
    aligned
}

fn ordinal_alignment(before: usize, after: usize) -> Vec<(Option<usize>, Option<usize>)> {
    (0..before.max(after))
        .map(|index| {
            (
                (index < before).then_some(index),
                (index < after).then_some(index),
            )
        })
        .collect()
}

fn changed_line_similarity(before: &str, after: &str) -> f32 {
    let before = before.get(1..).unwrap_or_default();
    let after = after.get(1..).unwrap_or_default();
    TextDiff::from_graphemes(before, after).ratio()
}

pub(super) fn parse_diff_hunk_starts(line: &str) -> Option<(usize, usize)> {
    let ranges = line.strip_prefix("@@ -")?;
    let (before, after_and_context) = ranges.split_once(" +")?;
    let (after, _) = after_and_context.split_once(" @@")?;
    let before = before.split(',').next()?.parse().ok()?;
    let after = after.split(',').next()?.parse().ok()?;
    Some((before, after))
}

pub(super) fn is_changed_diff_line(line: &str, in_hunk: bool) -> bool {
    if in_hunk {
        line.starts_with('-') || line.starts_with('+')
    } else {
        (line.starts_with('-') && !line.starts_with("--- "))
            || (line.starts_with('+') && !line.starts_with("+++ "))
    }
}

pub(super) fn diff_line_style(kind: DiffLineKind, theme: Theme) -> Style {
    let base = Style::default().bg(theme.markdown_code_block_background);
    match kind {
        DiffLineKind::Context => base.fg(theme.markdown_code_block_text),
        DiffLineKind::Deletion => base.fg(theme.ui_error).bg(theme.diff_deletion_background),
        DiffLineKind::Addition => base
            .fg(theme.ui_task_done)
            .bg(theme.diff_addition_background),
        DiffLineKind::Header => base.fg(theme.ui_warning).add_modifier(Modifier::BOLD),
        DiffLineKind::Hunk => base.fg(theme.ui_dialog_choice).add_modifier(Modifier::BOLD),
        DiffLineKind::Metadata => base.fg(theme.text_muted),
    }
}

pub(super) fn unified_diff_lines(diff: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let source = diff.lines().collect::<Vec<_>>();
    let mut lines = Vec::new();
    let mut in_hunk = false;
    let mut index = 0;
    while index < source.len() {
        let text = source[index];
        if is_changed_diff_line(text, in_hunk) {
            let start = index;
            while index < source.len() && is_changed_diff_line(source[index], in_hunk) {
                index += 1;
            }
            lines.extend(unified_change_block_lines(
                &source[start..index],
                width,
                theme,
            ));
            continue;
        }
        let kind = if text.starts_with("@@") {
            in_hunk = true;
            DiffLineKind::Hunk
        } else if text.starts_with("--- ") || text.starts_with("+++ ") {
            DiffLineKind::Header
        } else if text.starts_with(' ') {
            DiffLineKind::Context
        } else {
            DiffLineKind::Metadata
        };
        if text.is_empty() || text.starts_with("diff ") {
            in_hunk = false;
        }
        lines.extend(wrapped_diff_line(
            vec![Span::styled(text.to_string(), diff_line_style(kind, theme))],
            kind,
            width,
            theme,
        ));
        index += 1;
    }
    lines
}

fn unified_change_block_lines(block: &[&str], width: usize, theme: Theme) -> Vec<Line<'static>> {
    let before = block
        .iter()
        .copied()
        .filter(|line| line.starts_with('-'))
        .collect::<Vec<_>>();
    let after = block
        .iter()
        .copied()
        .filter(|line| line.starts_with('+'))
        .collect::<Vec<_>>();
    let mut before_pairs = vec![None; before.len()];
    let mut after_pairs = vec![None; after.len()];
    for (before_index, after_index) in align_changed_lines(&before, &after) {
        if let (Some(before_index), Some(after_index)) = (before_index, after_index) {
            before_pairs[before_index] = Some(after_index);
            after_pairs[after_index] = Some(before_index);
        }
    }

    let mut before_index = 0;
    let mut after_index = 0;
    let mut lines = Vec::new();
    for text in block {
        let (kind, counterpart) = if text.starts_with('-') {
            let counterpart = before_pairs[before_index].map(|index| after[index]);
            before_index += 1;
            (DiffLineKind::Deletion, counterpart)
        } else {
            let counterpart = after_pairs[after_index].map(|index| before[index]);
            after_index += 1;
            (DiffLineKind::Addition, counterpart)
        };
        lines.extend(wrapped_diff_line(
            intraline_diff_spans(text, counterpart, kind, theme),
            kind,
            width,
            theme,
        ));
    }
    lines
}

fn wrapped_diff_line(
    spans: Vec<Span<'static>>,
    kind: DiffLineKind,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let style = diff_line_style(kind, theme);
    wrap_spans_to_width(&spans, width)
        .into_iter()
        .map(move |spans| Line::from(pad_spans(spans, width, style)))
        .collect()
}

fn intraline_diff_spans(
    text: &str,
    counterpart: Option<&str>,
    kind: DiffLineKind,
    theme: Theme,
) -> Vec<Span<'static>> {
    let base = diff_line_style(kind, theme);
    let Some(counterpart) = counterpart.filter(|counterpart| {
        text.len() <= MAX_INTRALINE_BYTES && counterpart.len() <= MAX_INTRALINE_BYTES
    }) else {
        return vec![Span::styled(text.to_string(), base)];
    };
    if text.is_empty() {
        return vec![Span::styled(String::new(), base)];
    }
    let (prefix, content) = text.split_at(1);
    let counterpart = counterpart.get(1..).unwrap_or_default();
    let (old, new, target) = match kind {
        DiffLineKind::Deletion => (content, counterpart, ChangeTag::Delete),
        DiffLineKind::Addition => (counterpart, content, ChangeTag::Insert),
        _ => return vec![Span::styled(text.to_string(), base)],
    };
    let emphasis = match kind {
        DiffLineKind::Deletion => Style::default()
            .fg(theme.text_on_accent)
            .bg(theme.ui_error)
            .add_modifier(Modifier::BOLD),
        DiffLineKind::Addition => Style::default()
            .fg(theme.text_on_accent)
            .bg(theme.ui_task_done)
            .add_modifier(Modifier::BOLD),
        _ => base,
    };
    let diff = TextDiff::from_graphemes(old, new);
    let mut spans = vec![Span::styled(prefix.to_string(), base)];
    for change in diff.iter_all_changes() {
        if change.tag() == ChangeTag::Equal || change.tag() == target {
            push_diff_span(
                &mut spans,
                change.value().to_string(),
                if change.tag() == target {
                    emphasis
                } else {
                    base
                },
            );
        }
    }
    spans
}

fn push_diff_span(spans: &mut Vec<Span<'static>>, text: String, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut().filter(|last| last.style == style) {
        last.content.to_mut().push_str(&text);
    } else {
        spans.push(Span::styled(text, style));
    }
}

pub(super) fn side_by_side_diff_lines(
    diff: &str,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let rows = side_by_side_diff_rows(diff);
    let line_number_width = rows
        .iter()
        .flat_map(|row| match row {
            SideBySideDiffRow::Full(_, _) => [None, None],
            SideBySideDiffRow::Columns { before, after } => [
                before.and_then(|cell| cell.line_number),
                after.and_then(|cell| cell.line_number),
            ],
        })
        .flatten()
        .map(|line| line.to_string().len())
        .max()
        .unwrap_or(1)
        .max(3);
    let before_width = width / 2;
    let after_width = width.saturating_sub(before_width);
    let line_number_gutter_width = line_number_width + 3;
    if before_width <= line_number_gutter_width || after_width <= line_number_gutter_width {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for row in rows {
        let (before, after) = match row {
            SideBySideDiffRow::Full(text, kind) => {
                let style = diff_line_style(kind, theme);
                lines.extend(
                    wrap_spans_to_width(&[Span::styled(text.to_string(), style)], width)
                        .into_iter()
                        .map(|spans| Line::from(pad_spans(spans, width, style))),
                );
                continue;
            }
            SideBySideDiffRow::Columns { before, after } => (before, after),
        };
        let before_lines = diff_cell_lines(
            before,
            after.map(|cell| cell.text),
            before_width,
            line_number_width,
            true,
            theme,
        );
        let after_lines = diff_cell_lines(
            after,
            before.map(|cell| cell.text),
            after_width,
            line_number_width,
            false,
            theme,
        );
        let height = before_lines.len().max(after_lines.len()).max(1);
        for row in 0..height {
            let mut spans = pad_spans(
                before_lines.get(row).cloned().unwrap_or_default(),
                before_width,
                Style::default().bg(theme.markdown_code_block_background),
            );
            spans.extend(pad_spans(
                after_lines.get(row).cloned().unwrap_or_default(),
                after_width,
                Style::default().bg(theme.markdown_code_block_background),
            ));
            lines.push(Line::from(spans));
        }
    }
    lines
}

pub(super) fn diff_cell_lines(
    cell: Option<SideBySideDiffCell<'_>>,
    counterpart: Option<&str>,
    width: usize,
    line_number_width: usize,
    line_number_on_right: bool,
    theme: Theme,
) -> Vec<Vec<Span<'static>>> {
    let Some(cell) = cell else {
        return Vec::new();
    };
    let content_width = width.saturating_sub(line_number_width + 3);
    let content_style = diff_line_style(cell.kind, theme);
    let has_line_number = cell.line_number.is_some();
    wrap_spans_to_width(
        &intraline_diff_spans(cell.text, counterpart, cell.kind, theme),
        content_width,
    )
    .into_iter()
    .enumerate()
    .map(|(row, content)| {
        let number = if row == 0 {
            cell.line_number
                .map(|line| format!("{line:>line_number_width$}"))
                .unwrap_or_else(|| " ".repeat(line_number_width))
        } else {
            " ".repeat(line_number_width)
        };
        let gutter_style = content_style.fg(theme.text_muted);
        let number = Span::styled(number, gutter_style);
        let separator = Span::styled(if has_line_number { " │ " } else { "   " }, gutter_style);
        let spans = if line_number_on_right {
            let mut spans = pad_spans(content, content_width, content_style);
            spans.push(separator);
            spans.push(number);
            spans
        } else {
            let mut spans = vec![number, separator];
            spans.extend(content);
            spans
        };
        pad_spans(spans, width, content_style)
    })
    .collect()
}

pub(super) fn pad_spans(
    mut spans: Vec<Span<'static>>,
    width: usize,
    style: Style,
) -> Vec<Span<'static>> {
    let used = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum::<usize>();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    spans
}
