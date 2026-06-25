//! Markdown → styled ratatui `Line` rendering.
//!
//! Uses `pulldown-cmark` to parse CommonMark (plus tables, strikethrough, and
//! task lists) and maps the event stream to styled `Line`/`Span` values. The
//! parser has no ratatui dependency, so this stays decoupled from the app's
//! ratatui version.
//!
//! Terminals have no font size, so "headings" render as bold + color rather
//! than larger text. Block structure (lists, code blocks, blockquotes) is laid
//! out with prefixes/indentation; long lines are left for the caller's widget
//! to wrap. Tables are collected into a grid and rendered with box borders,
//! measuring columns in *display* width so CJK glyphs (width 2) align.

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Theme constants for rendered elements.
const CODE_INLINE: Style = Style::new().fg(Color::Yellow);
const CODE_BLOCK: Style = Style::new().fg(Color::Cyan);
const QUOTE_BAR: Style = Style::new().fg(Color::Magenta);
const MARKER: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const RULE: Style = Style::new().fg(Color::DarkGray);
const RULE_WIDTH: usize = 40;

/// Render a markdown document into a sequence of styled lines.
pub fn to_lines(source: &str) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(source, opts);

    let mut r = Renderer::default();
    for event in parser {
        r.handle(event);
    }
    r.finish()
}

#[derive(Default)]
struct Renderer {
    /// Completed output lines.
    out: Vec<Line<'static>>,
    /// Spans accumulated for the line currently being built.
    cur: Vec<Span<'static>>,
    /// Folded inline style applied to new text spans.
    style: Style,
    /// Previous styles, one per open inline element (strong/emphasis/…).
    style_stack: Vec<Style>,
    /// One entry per open list, innermost last.
    list_stack: Vec<ListInfo>,
    /// One entry per open list item, innermost last (parallel nesting to
    /// `list_stack` items, not to the lists themselves).
    item_stack: Vec<ItemInfo>,
    quote_depth: usize,
    code_block: bool,
    code_buf: String,
    /// When `Some`, a table is being collected; inline content is routed to the
    /// current cell (`cur_cell`) instead of `cur`.
    table: Option<TableBuild>,
    cur_cell: Vec<Span<'static>>,
    cur_row: Option<TableRow>,
}

struct ListInfo {
    ordered: bool,
    /// Next number to print for an ordered list item.
    counter: u64,
}

struct ItemInfo {
    /// `- [ ]` / `- [x]` task-list marker, if this item had one.
    checkbox: Option<bool>,
    /// `"3. "` for ordered items, `None` for plain bullets.
    ordered_marker: Option<String>,
    /// True until the first content line of the item is flushed.
    needs_marker: bool,
    /// Leading columns occupied by the marker + indent (for continuation lines).
    indent: usize,
}

struct TableBuild {
    /// Per-column alignment from the `|---|` delimiter row.
    alignments: Vec<Alignment>,
    rows: Vec<TableRow>,
}

struct TableRow {
    is_header: bool,
    /// Each cell's styled spans (single-line content).
    cells: Vec<Vec<Span<'static>>>,
}

impl Renderer {
    fn handle(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(s) => {
                if self.code_block {
                    self.code_buf.push_str(&s);
                } else if self.table.is_some() {
                    self.cur_cell.push(Span::styled(s.to_string(), self.style));
                } else {
                    self.cur.push(Span::styled(s.to_string(), self.style));
                }
            }
            Event::Code(s) => {
                let style = CODE_INLINE;
                if self.table.is_some() {
                    self.cur_cell.push(Span::styled(s.to_string(), style));
                } else {
                    self.cur.push(Span::styled(s.to_string(), style));
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if self.table.is_some() {
                    // Cells are single-line; collapse breaks to a space.
                    self.cur_cell.push(Span::raw(" ".to_string()));
                } else {
                    self.flush();
                }
            }
            Event::Rule => {
                self.maybe_blank();
                self.out
                    .push(Line::from(vec![Span::styled("─".repeat(RULE_WIDTH), RULE)]));
            }
            Event::TaskListMarker(checked) => {
                if let Some(ii) = self.item_stack.last_mut() {
                    ii.checkbox = Some(checked);
                }
            }
            Event::Html(s) | Event::InlineHtml(s) => {
                if !self.code_block {
                    if self.table.is_some() {
                        self.cur_cell.push(Span::raw(s.to_string()));
                    } else {
                        self.cur.push(Span::raw(s.to_string()));
                    }
                }
            }
            // Footnotes and anything else are rendered as nothing.
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.flush();
                self.maybe_blank();
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.maybe_blank();
                self.style_stack.push(self.style);
                self.style = heading_style(level);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                if self.list_stack.is_empty() {
                    self.maybe_blank();
                }
                self.quote_depth += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.maybe_blank();
                self.code_block = true;
                self.code_buf.clear();
            }
            Tag::List(start) => {
                self.flush();
                if self.list_stack.is_empty() {
                    self.maybe_blank();
                }
                self.list_stack.push(ListInfo {
                    ordered: start.is_some(),
                    counter: start.unwrap_or(0),
                });
            }
            Tag::Item => {
                let ordered_marker = match self.list_stack.last_mut() {
                    Some(li) if li.ordered => {
                        let m = format!("{}. ", li.counter);
                        li.counter += 1;
                        Some(m)
                    }
                    _ => None,
                };
                self.item_stack.push(ItemInfo {
                    checkbox: None,
                    ordered_marker,
                    needs_marker: true,
                    indent: 0,
                });
            }
            Tag::Emphasis => self.push_modifier(Modifier::ITALIC),
            Tag::Strong => self.push_modifier(Modifier::BOLD),
            Tag::Strikethrough => self.push_modifier(Modifier::CROSSED_OUT),
            Tag::Link { .. } => self.push_modifier(Modifier::UNDERLINED),
            Tag::Image { .. } => self.push_modifier(Modifier::ITALIC),
            Tag::Table(alignments) => {
                self.flush();
                self.maybe_blank();
                self.cur_cell.clear();
                self.cur_row = None;
                self.table = Some(TableBuild {
                    alignments,
                    rows: Vec::new(),
                });
            }
            Tag::TableHead => {
                self.cur_row = Some(TableRow {
                    is_header: true,
                    cells: Vec::new(),
                });
            }
            Tag::TableRow => {
                self.cur_row = Some(TableRow {
                    is_header: false,
                    cells: Vec::new(),
                });
            }
            Tag::TableCell => {
                // A new cell begins; content accumulates in `cur_cell` and is
                // committed on `TagEnd::TableCell`.
            }
            _ => {}
        }
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.pop_modifier();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.emit_code_block();
                self.code_block = false;
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
            }
            TagEnd::Item => {
                self.flush();
                self.item_stack.pop();
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(row) = self.cur_row.take() {
                    if let Some(tbl) = self.table.as_mut() {
                        tbl.rows.push(row);
                    }
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.cur_cell);
                if let Some(row) = self.cur_row.as_mut() {
                    row.cells.push(cell);
                }
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Image => self.pop_modifier(),
            TagEnd::Table => self.emit_table(),
            TagEnd::FootnoteDefinition => {}
            _ => {}
        }
    }

    fn push_modifier(&mut self, m: Modifier) {
        self.style_stack.push(self.style);
        self.style = self.style.add_modifier(m);
    }

    fn pop_modifier(&mut self) {
        if let Some(prev) = self.style_stack.pop() {
            self.style = prev;
        }
    }

    /// Insert a blank separator line between top-level blocks (never inside a
    /// list, and never two in a row).
    fn maybe_blank(&mut self) {
        if self.out.is_empty() || !self.list_stack.is_empty() {
            return;
        }
        let blank = self
            .out
            .last()
            .map(|l| l.spans.is_empty())
            .unwrap_or(true);
        if !blank {
            self.out.push(Line::default());
        }
    }

    fn flush(&mut self) {
        // Nothing pending (e.g. a block whose content was already flushed by an
        // inner block end): emit nothing rather than an empty prefixed line.
        if self.cur.is_empty() {
            return;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();

        for _ in 0..self.quote_depth {
            spans.push(Span::styled("▌ ".to_string(), QUOTE_BAR));
        }

        if let Some(ii) = self.item_stack.last_mut() {
            if ii.needs_marker {
                ii.needs_marker = false;
                let depth_pad = " ".repeat(self.list_stack.len().saturating_sub(1) * 2);
                let marker = marker_for(ii);
                ii.indent = depth_pad.len() + marker.len();
                spans.push(Span::styled(format!("{depth_pad}{marker}"), MARKER));
            } else {
                spans.push(Span::raw(" ".repeat(ii.indent)));
            }
        }

        spans.append(&mut self.cur);
        self.out.push(Line::from(spans));
    }

    /// Quote bars + list-item indent prepended to every line of multi-line
    /// blocks (code blocks, tables).
    fn line_prefix(&self) -> Vec<Span<'static>> {
        let mut v: Vec<Span<'static>> = Vec::new();
        for _ in 0..self.quote_depth {
            v.push(Span::styled("▌ ".to_string(), QUOTE_BAR));
        }
        let indent = self.item_stack.last().map(|ii| ii.indent).unwrap_or(0);
        if indent > 0 {
            v.push(Span::raw(" ".repeat(indent)));
        }
        v
    }

    fn emit_code_block(&mut self) {
        let buf = std::mem::take(&mut self.code_buf);
        for raw in buf.trim_end_matches('\n').split('\n') {
            let mut spans = self.line_prefix();
            spans.push(Span::styled(raw.to_string(), CODE_BLOCK));
            self.out.push(Line::from(spans));
        }
        self.code_buf.clear();
    }

    /// Lay out a collected table as bordered rows, with a separator directly
    /// under the header row. Column widths are measured in *display* columns so
    /// wide (CJK) glyphs align with narrow (ASCII) ones.
    fn emit_table(&mut self) {
        let Some(tbl) = self.table.take() else {
            return;
        };
        let ncols = tbl.rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        if ncols == 0 || tbl.rows.is_empty() {
            return;
        }

        let mut colw = vec![0usize; ncols];
        for row in &tbl.rows {
            for (c, cell) in row.cells.iter().enumerate() {
                let w = cell_width(cell);
                if w > colw[c] {
                    colw[c] = w;
                }
            }
        }

        let prefix = self.line_prefix();
        let aligns = tbl.alignments;

        self.push_table_border(&prefix, &colw, "┌", "┬", "┐");
        for row in &tbl.rows {
            self.push_table_row(&prefix, &colw, &aligns, row);
            if row.is_header {
                self.push_table_border(&prefix, &colw, "├", "┼", "┤");
            }
        }
        self.push_table_border(&prefix, &colw, "└", "┴", "┘");
    }

    fn push_table_border(
        &mut self,
        prefix: &[Span<'static>],
        colw: &[usize],
        left: &str,
        mid: &str,
        right: &str,
    ) {
        let mut spans = prefix.to_vec();
        spans.push(Span::raw(left.to_string()));
        for (i, w) in colw.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(mid.to_string()));
            }
            spans.push(Span::raw("─".repeat(w + 2)));
        }
        spans.push(Span::raw(right.to_string()));
        self.out.push(Line::from(spans));
    }

    fn push_table_row(
        &mut self,
        prefix: &[Span<'static>],
        colw: &[usize],
        aligns: &[Alignment],
        row: &TableRow,
    ) {
        let mut spans = prefix.to_vec();
        spans.push(Span::raw("│".to_string()));
        for (c, &target) in colw.iter().enumerate() {
            let align = aligns.get(c).copied().unwrap_or(Alignment::None);
            let mut cell = row.cells.get(c).cloned().unwrap_or_default();
            if row.is_header {
                for s in &mut cell {
                    s.style = s.style.add_modifier(Modifier::BOLD);
                }
            }
            let width = cell_width(&cell);
            let (lead, trail) = padding(align, target, width);
            spans.push(Span::raw(" ".to_string()));
            spans.push(Span::raw(" ".repeat(lead)));
            spans.append(&mut cell);
            spans.push(Span::raw(" ".repeat(trail)));
            spans.push(Span::raw(" ".to_string()));
            spans.push(Span::raw("│".to_string()));
        }
        self.out.push(Line::from(spans));
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.cur.is_empty() {
            self.flush();
        }
        self.out
    }
}

fn marker_for(ii: &ItemInfo) -> String {
    if let Some(checked) = ii.checkbox {
        if checked {
            "[x] ".into()
        } else {
            "[ ] ".into()
        }
    } else if let Some(m) = &ii.ordered_marker {
        m.clone()
    } else {
        "• ".into()
    }
}

fn heading_style(level: HeadingLevel) -> Style {
    let color = match level {
        HeadingLevel::H1 => Color::Cyan,
        HeadingLevel::H2 => Color::Green,
        HeadingLevel::H3 => Color::Yellow,
        HeadingLevel::H4 => Color::Blue,
        HeadingLevel::H5 => Color::Magenta,
        HeadingLevel::H6 => Color::DarkGray,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// Display width of a cell's spans (CJK glyphs count as 2).
fn cell_width(cell: &[Span]) -> usize {
    cell.iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// Leading/trailing space padding (in display columns) to reach `colw`.
fn padding(align: Alignment, colw: usize, content_w: usize) -> (usize, usize) {
    let pad = colw.saturating_sub(content_w);
    match align {
        Alignment::Right => (pad, 0),
        Alignment::Center => (pad / 2, pad - pad / 2),
        Alignment::None | Alignment::Left => (0, pad),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// Concatenate every span's text across a line.
    fn text_of(line: &Line) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    /// First span (anywhere) whose text contains `needle`.
    fn span_with<'a>(lines: &'a [Line<'a>], needle: &str) -> Option<&'a Span<'a>> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains(needle))
    }

    fn joined(lines: &[Line]) -> String {
        lines.iter().map(|l| text_of(l)).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn heading_is_bold_and_colored() {
        let l = to_lines("# Title");
        let s = span_with(&l, "Title").expect("heading text");
        assert!(s.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(s.style.fg, Some(Color::Cyan));
    }

    #[test]
    fn strong_emphasis_strikethrough() {
        let b = to_lines("**b**");
        let s = span_with(&b, "b").unwrap();
        assert!(s.style.add_modifier.contains(Modifier::BOLD));
        let i = to_lines("*i*");
        let s = span_with(&i, "i").unwrap();
        assert!(s.style.add_modifier.contains(Modifier::ITALIC));
        let st = to_lines("~~s~~");
        let s = span_with(&st, "s").unwrap();
        assert!(s.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn inline_code_is_styled() {
        let l = to_lines("`x`");
        let s = span_with(&l, "x").unwrap();
        assert_eq!(s.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn fenced_code_block_lines_are_cyan() {
        let l = to_lines("```\nlet x = 1;\n```");
        let s = span_with(&l, "let x").unwrap();
        assert_eq!(s.style.fg, Some(Color::Cyan));
    }

    #[test]
    fn unordered_list_uses_bullet() {
        let l = to_lines("- alpha\n- beta");
        assert!(text_of(&l[0]).contains("•"));
        assert!(text_of(&l[0]).contains("alpha"));
        assert!(text_of(&l[1]).contains("beta"));
    }

    #[test]
    fn ordered_list_numbers_items() {
        let l = to_lines("1. one\n2. two");
        assert!(text_of(&l[0]).contains("1. "));
        assert!(text_of(&l[1]).contains("2. "));
    }

    #[test]
    fn task_list_uses_checkbox_marker() {
        let l = to_lines("- [ ] done it");
        assert!(text_of(&l[0]).contains("[ ]"));
        assert!(text_of(&l[0]).contains("done it"));
    }

    #[test]
    fn blockquote_has_bar_prefix() {
        let l = to_lines("> quoted");
        assert!(span_with(&l, "▌").is_some());
        assert!(text_of(&l[0]).contains("quoted"));
    }

    #[test]
    fn plain_text_passes_through() {
        let l = to_lines("just words");
        assert_eq!(l.len(), 1);
        assert_eq!(text_of(&l[0]), "just words");
    }

    #[test]
    fn blocks_are_separated_by_blank_lines() {
        let l = to_lines("first\n\nsecond");
        // paragraph, blank, paragraph.
        assert_eq!(l.len(), 3);
        assert!(l[1].spans.is_empty());
    }

    #[test]
    fn nested_list_indents_child_items() {
        let l = to_lines("- b\n  - nested\n- c");
        // Parent "b", indented child "nested", sibling "c" — each on its own
        // line, child text not merged into the parent's.
        assert!(text_of(&l[0]).contains("• b"));
        assert!(text_of(&l[1]).contains("• nested"));
        assert!(text_of(&l[2]).contains("• c"));
        assert!(!text_of(&l[1]).trim().is_empty());
    }

    #[test]
    fn table_renders_header_separator_and_cells() {
        let l = to_lines("| Name | Age |\n| --- | --- |\n| Bob | 30 |");
        // Header separator border is the line containing ┼.
        assert!(
            l.iter().any(|x| text_of(x).contains('┼')),
            "header separator missing"
        );
        let all = joined(&l);
        assert!(all.contains("Name"));
        assert!(all.contains("Bob"));
        assert!(all.contains("30"));
    }

    #[test]
    fn table_columns_align_with_cjk() {
        let md = "| 名字 | Age |\n| --- | --- |\n| 张三 | 30 |\n| 李 | 300 |";
        let l = to_lines(md);

        // Every table line shares the same display width. A char-count bug
        // would make the CJK header/data rows a different width from the
        // borders, which this asserts against.
        let widths: Vec<usize> = l
            .iter()
            .map(|x| UnicodeWidthStr::width(text_of(x).as_str()))
            .collect();
        let w = widths[0];
        for (i, ww) in widths.iter().enumerate() {
            assert_eq!(*ww, w, "line {i} width {ww} != {w}");
        }

        // CJK content survived into the output.
        let all = joined(&l);
        assert!(all.contains("名字"));
        assert!(all.contains("张三"));
    }

    #[test]
    fn table_renders_multicolumn_and_right_align() {
        // Two columns, first right-aligned: the narrow "1" is padded so column
        // widths (and thus every border/row line) stay uniform.
        let l = to_lines("| a | b |\n| --: | --- |\n| 1 | 22 |");
        let all = joined(&l);
        assert!(all.contains('┼'), "header separator missing");
        assert!(all.contains("22"));
        let widths: Vec<usize> = l
            .iter()
            .map(|x| UnicodeWidthStr::width(text_of(x).as_str()))
            .collect();
        let w = widths[0];
        assert!(widths.iter().all(|ww| *ww == w), "uneven widths: {widths:?}");
    }
}
