//! Content-sized table columns within a terminal-cell budget.
use pulldown_cmark::Alignment;
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub(super) struct Table {
    pub alignments: Vec<Alignment>,
    pub rows: Vec<Vec<Line<'static>>>,
    pub cells: Vec<Line<'static>>,
}

pub(super) fn render(table: Table, width: usize) -> Vec<Line<'static>> {
    let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return Vec::new();
    }
    let preferred: Vec<_> = (0..columns)
        .map(|i| {
            table
                .rows
                .iter()
                .filter_map(|row| row.get(i))
                .map(Line::width)
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect();
    let minimum: Vec<_> = (0..columns)
        .map(|i| {
            table
                .rows
                .iter()
                .filter_map(|row| row.get(i))
                .flat_map(|line| &line.spans)
                .flat_map(|span| span.styled_graphemes(Style::default()))
                .map(|g| UnicodeWidthStr::width(g.symbol))
                .max()
                .unwrap_or(1)
                .max(1)
        })
        .collect();
    let budget = width.saturating_sub(columns * 3 + 1);
    if budget < minimum.iter().sum::<usize>() {
        return table
            .rows
            .into_iter()
            .flatten()
            .flat_map(|line| super::wrap::line(&line, width))
            .collect();
    }
    let widths = column_widths(&preferred, &minimum, budget);
    let border = |left: char, middle: char, right: char| {
        let segments = widths
            .iter()
            .map(|width| "─".repeat(width + 2))
            .collect::<Vec<_>>();
        Line::raw(format!(
            "{left}{}{right}",
            segments.join(&middle.to_string())
        ))
    };
    let mut output = vec![border('╭', '┬', '╮')];
    for (index, row) in table.rows.into_iter().enumerate() {
        let cells: Vec<_> = widths
            .iter()
            .enumerate()
            .map(|(column, width)| {
                super::wrap::line(row.get(column).unwrap_or(&Line::default()), *width)
            })
            .collect();
        let height = cells.iter().map(Vec::len).max().unwrap_or(1);
        for y in 0..height {
            let mut line = Line::raw("│");
            for (column, rows) in cells.iter().enumerate() {
                let cell = rows.get(y).cloned().unwrap_or_default();
                let padding = widths[column].saturating_sub(cell.width());
                let left = match table.alignments.get(column) {
                    Some(Alignment::Right) => padding,
                    Some(Alignment::Center) => padding / 2,
                    _ => 0,
                };
                line.spans.push(Span::raw(" ".repeat(left + 1)));
                line.spans.extend(cell.spans);
                line.spans
                    .push(Span::raw(format!("{} │", " ".repeat(padding - left))));
            }
            output.push(line);
        }
        if index == 0 {
            output.push(border('├', '┼', '┤'));
        }
    }
    output.push(border('╰', '┴', '╯'));
    output
}

/// Keep short columns at their natural width; cap larger columns evenly when
/// content exceeds the budget. Spare space belongs to the longest column.
fn column_widths(preferred: &[usize], minimum: &[usize], budget: usize) -> Vec<usize> {
    let total: usize = preferred.iter().sum();
    if total <= budget {
        let mut widths = preferred.to_vec();
        let longest = preferred
            .iter()
            .enumerate()
            .max_by_key(|(_, width)| **width)
            .unwrap()
            .0;
        widths[longest] += budget - total;
        return widths;
    }
    let mut low = 0;
    let mut high = budget;
    while low < high {
        let cap = low + (high - low).div_ceil(2);
        let used: usize = preferred
            .iter()
            .zip(minimum)
            .map(|(ideal, min)| (*ideal).min(cap).max(*min))
            .sum();
        if used <= budget {
            low = cap;
        } else {
            high = cap - 1;
        }
    }
    let mut widths: Vec<_> = preferred
        .iter()
        .zip(minimum)
        .map(|(ideal, min)| (*ideal).min(low).max(*min))
        .collect();
    let mut remaining = budget - widths.iter().sum::<usize>();
    for (width, ideal) in widths.iter_mut().zip(preferred) {
        if remaining > 0 && *width < *ideal {
            *width += 1;
            remaining -= 1;
        }
    }
    widths
}
