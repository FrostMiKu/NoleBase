//! Width-bounded wrapping shared by code blocks and table cells.
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

pub(super) fn line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut current = Line::default();
    let mut used = 0;
    for span in &line.spans {
        let style = line.style.patch(span.style);
        let mut chunk = String::new();
        for grapheme in span.styled_graphemes(line.style) {
            let size = UnicodeWidthStr::width(grapheme.symbol);
            if used + size > width && used > 0 {
                current
                    .spans
                    .push(Span::styled(std::mem::take(&mut chunk), grapheme.style));
                rows.push(std::mem::take(&mut current));
                used = 0;
            }
            if size > width {
                chunk.push('�');
                used += 1;
            } else {
                chunk.push_str(grapheme.symbol);
                used += size;
            }
        }
        if !chunk.is_empty() {
            current.spans.push(Span::styled(chunk, style));
        }
    }
    if !current.spans.is_empty() || rows.is_empty() {
        rows.push(current);
    }
    rows
}
