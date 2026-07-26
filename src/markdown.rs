//! MBDown markup -> styled ratatui lines.
//!
//! MBDown owns the language and syntax tree. MBTUI owns Nole's Ratatui layout.

use std::sync::OnceLock;

use mbtui::{Renderer, Theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};

/// Render markup for an exact terminal display width.
pub fn to_lines_at_width(source: &str, width: usize) -> Vec<Line<'static>> {
    match mbdown::parse(source) {
        Ok(document) => {
            Renderer::with_theme(width.max(1), note_theme().clone())
                .render(&document)
                .lines
        }
        Err(error) => Text::raw(format!("MBDown parse error: {error}")).lines,
    }
}

fn note_theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let mut theme = Theme::default();
        theme.quote = Style::default().fg(Color::Magenta);
        theme.list = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        theme.rule = Style::default().fg(Color::DarkGray);
        theme.code = Style::default().fg(Color::Cyan);
        theme.insert("markdown-box", Style::default().fg(Color::DarkGray));
        theme
    })
}

/// Map a one-based source line to its first terminal row after MBTUI layout.
pub fn rendered_row_for_source_line(source: &str, line_no: usize, width: usize) -> usize {
    let rendered = to_lines_at_width(source, width);
    if rendered.is_empty() {
        return 0;
    }

    let source_lines: Vec<&str> = source.lines().collect();
    let source_index = line_no
        .saturating_sub(1)
        .min(source_lines.len().saturating_sub(1));
    let target_key = normalized_rendered_text(&to_lines_at_width(
        source_lines.get(source_index).copied().unwrap_or(""),
        width,
    ));
    let expected = source_index
        .saturating_mul(rendered.len())
        .checked_div(source_lines.len().max(1))
        .unwrap_or(0)
        .min(rendered.len().saturating_sub(1));

    if target_key.is_empty() {
        return expected;
    }
    rendered
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let key = normalized_line_text(line);
            (!key.is_empty() && (key.contains(&target_key) || target_key.contains(&key)))
                .then_some(index)
        })
        .min_by_key(|index| index.abs_diff(expected))
        .unwrap_or(expected)
}

fn normalized_rendered_text(lines: &[Line<'_>]) -> String {
    let text = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    normalize_search_text(&text)
}

fn normalized_line_text(line: &Line<'_>) -> String {
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    normalize_search_text(&text)
}

fn normalize_search_text(text: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.extend(character.to_lowercase());
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;

    const WIDTH: usize = 120;

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn span_with<'a>(lines: &'a [Line<'_>], needle: &str) -> &'a ratatui::text::Span<'a> {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains(needle))
            .expect("styled span")
    }

    #[test]
    fn renders_markdown_styles_and_structure() {
        let lines = to_lines_at_width("# Heading\n\n- **bold** and `code`\n\n> quote", WIDTH);
        assert!(span_with(&lines, "Heading")
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(span_with(&lines, "bold")
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        let output = text(&lines);
        assert!(output.contains("• bold and code"));
        assert!(output.contains("  ▌ quote"));
    }

    #[test]
    fn renders_bbcode_foreground_and_background_colors() {
        let lines = to_lines_at_width(
            "[color=#12abef]foreground[/color] [bg=196]background[/bg]",
            WIDTH,
        );
        assert_eq!(
            span_with(&lines, "foreground").style.fg,
            Some(Color::Rgb(0x12, 0xab, 0xef))
        );
        assert_eq!(
            span_with(&lines, "background").style.bg,
            Some(Color::Indexed(196))
        );
    }

    #[test]
    fn renders_boxes_and_responsive_columns() {
        let box_lines = to_lines_at_width(
            "[box title=Info width=24 border=single bg=17]\nHello\n[/box]",
            40,
        );
        let boxed = text(&box_lines);
        assert!(boxed.contains("┌─ Info"));
        assert!(boxed.contains("Hello"));
        assert!(boxed.lines().any(|line| line.starts_with('│')));
        assert!(box_lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.bg == Some(Color::Indexed(17))));

        let source = concat!(
            "[columns gap=2]\n",
            "[column]Left[/column]\n",
            "[column]Right[/column]\n",
            "[/columns]"
        );
        let wide = text(&to_lines_at_width(source, 30));
        assert!(wide
            .lines()
            .any(|line| line.contains("Left") && line.contains("Right")));
        let narrow = text(&to_lines_at_width(source, 15));
        assert!(!narrow
            .lines()
            .any(|line| line.contains("Left") && line.contains("Right")));
    }

    #[test]
    fn output_contains_no_terminal_escape_sequences() {
        let output = text(&to_lines_at_width("[red][b]safe[/b][/red]", WIDTH));
        assert_eq!(output, "safe");
        assert!(!output.contains('\x1b'));

        let linked = to_lines_at_width("**bold** [label](https://example.test)", WIDTH);
        assert_eq!(text(&linked), "bold label");
        assert!(span_with(&linked, "bold")
            .style
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn source_lines_map_to_mbterm_rows() {
        assert_eq!(
            rendered_row_for_source_line("# Heading\n\nintro\n\nneedle", 5, 80),
            4
        );
        assert_eq!(rendered_row_for_source_line("abcdefghij\nneedle", 2, 5), 2);
        assert_eq!(
            rendered_row_for_source_line("| Name |\n| --- |\n| needle |", 3, 80),
            3
        );
    }
}
