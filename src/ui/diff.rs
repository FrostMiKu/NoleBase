use super::*;

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
            for row in 0..before.len().max(after.len()) {
                rows.push(SideBySideDiffRow::Columns {
                    before: before.get(row).copied().map(|line| {
                        let cell =
                            SideBySideDiffCell::new(line, DiffLineKind::Deletion, before_line);
                        before_line = before_line.map(|line| line + 1);
                        cell
                    }),
                    after: after.get(row).copied().map(|line| {
                        let cell =
                            SideBySideDiffCell::new(line, DiffLineKind::Addition, after_line);
                        after_line = after_line.map(|line| line + 1);
                        cell
                    }),
                });
            }
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
        DiffLineKind::Deletion => base.fg(theme.ui_error),
        DiffLineKind::Addition => base.fg(theme.ui_task_done),
        DiffLineKind::Header => base.fg(theme.ui_warning).add_modifier(Modifier::BOLD),
        DiffLineKind::Hunk => base.fg(theme.ui_dialog_choice).add_modifier(Modifier::BOLD),
        DiffLineKind::Metadata => base.fg(theme.text_muted),
    }
}

pub(super) fn side_by_side_diff_lines(diff: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
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
    let divider_width = 3;
    let columns_width = width.saturating_sub(divider_width);
    let before_width = columns_width / 2;
    let after_width = columns_width.saturating_sub(before_width);
    let line_number_gutter_width = line_number_width + 3;
    if before_width <= line_number_gutter_width || after_width <= line_number_gutter_width {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for row in rows {
        let (before, after) = match row {
            SideBySideDiffRow::Full(text, kind) => {
                (Some(SideBySideDiffCell::new(text, kind, None)), None)
            }
            SideBySideDiffRow::Columns { before, after } => (before, after),
        };
        let before_lines = diff_cell_lines(before, before_width, line_number_width, theme);
        let after_lines = diff_cell_lines(after, after_width, line_number_width, theme);
        let height = before_lines.len().max(after_lines.len()).max(1);
        for row in 0..height {
            let mut spans = pad_spans(
                before_lines.get(row).cloned().unwrap_or_default(),
                before_width,
                Style::default().bg(theme.markdown_code_block_background),
            );
            spans.push(Span::styled(
                " ┃ ",
                Style::default()
                    .fg(theme.ui_dialog_choice)
                    .bg(theme.surface_overlay)
                    .add_modifier(Modifier::BOLD),
            ));
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
    width: usize,
    line_number_width: usize,
    theme: Theme,
) -> Vec<Vec<Span<'static>>> {
    let Some(cell) = cell else {
        return Vec::new();
    };
    let content_width = width.saturating_sub(line_number_width + 3);
    let content_style = diff_line_style(cell.kind, theme);
    wrap_spans_to_width(
        &[Span::styled(cell.text.to_string(), content_style)],
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
        let gutter_style = Style::default()
            .fg(theme.text_muted)
            .bg(theme.markdown_code_block_background);
        let mut spans = vec![
            Span::styled(number, gutter_style),
            Span::styled(" │ ", gutter_style),
        ];
        spans.extend(content);
        pad_spans(spans, width, content_style)
    })
    .collect()
}

pub(super) fn pad_spans(mut spans: Vec<Span<'static>>, width: usize, style: Style) -> Vec<Span<'static>> {
    let used = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum::<usize>();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    spans
}
