use std::sync::OnceLock;

use crate::tui::theme::Theme;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

pub(super) const PADDING_X: usize = 2;

pub(super) fn render(
    source: &str,
    language: &str,
    status: &str,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let padding = PADDING_X.min(width.saturating_sub(1) / 2);
    let available = width - padding * 2;
    let base = Style::default()
        .fg(theme.markdown_code_block_text)
        .bg(theme.markdown_code_block_background);
    let label_style = base.fg(theme.markdown_code_label);
    let blank = || Line::styled(" ".repeat(width), base);
    let label = if status.is_empty() {
        language.to_string()
    } else {
        format!("{language} · {status}")
    };
    let mut header = Line::from(Span::styled(" ".repeat(padding), base));
    let label = super::wrap::line(&Line::styled(label, label_style), available).remove(0);
    header.spans.extend(label.spans);
    header
        .spans
        .push(Span::styled(" ".repeat(width - header.width()), base));
    let mut output = vec![blank(), header, blank()];
    let lines = lines(source, language, base);
    let digits = lines.len().max(1).to_string().len();
    let gutter_width = (digits + 3).min(available.saturating_sub(1));
    let content_width = available - gutter_width;
    for (index, line) in lines.iter().enumerate() {
        for (part, content) in super::wrap::line(line, content_width)
            .into_iter()
            .enumerate()
        {
            let gutter = if part == 0 {
                format!("{:>digits$} │ ", index + 1)
            } else {
                " ".repeat(gutter_width)
            };
            let gutter = super::wrap::line(&Line::styled(gutter, label_style), gutter_width.max(1))
                .remove(0);
            let mut line = Line::from(Span::styled(" ".repeat(padding), base));
            if gutter_width > 0 {
                line.spans.extend(gutter.spans);
            }
            line.spans.extend(content.spans);
            line.spans.push(Span::styled(
                " ".repeat(width.saturating_sub(line.width())),
                base,
            ));
            output.push(line);
        }
    }
    output.push(blank());
    output
}

fn lines(source: &str, language: &str, base: Style) -> Vec<Line<'static>> {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let syntaxes = SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines);
    let themes = THEMES.get_or_init(ThemeSet::load_defaults);
    let Some(syntax) = syntaxes.find_syntax_by_token(language) else {
        return source
            .lines()
            .map(|s| Line::styled(s.to_owned(), base))
            .collect();
    };
    let mut highlighter = HighlightLines::new(syntax, &themes.themes["base16-ocean.dark"]);
    LinesWithEndings::from(source)
        .map(|line| {
            let Ok(ranges) = highlighter.highlight_line(line, syntaxes) else {
                return Line::styled(line.trim_end_matches(['\r', '\n']).to_owned(), base);
            };
            Line::from(
                ranges
                    .into_iter()
                    .map(|(syntax, text)| {
                        let mut style = base.fg(Color::Rgb(
                            syntax.foreground.r,
                            syntax.foreground.g,
                            syntax.foreground.b,
                        ));
                        if syntax.font_style.contains(FontStyle::BOLD) {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if syntax.font_style.contains(FontStyle::ITALIC) {
                            style = style.add_modifier(Modifier::ITALIC);
                        }
                        if syntax.font_style.contains(FontStyle::UNDERLINE) {
                            style = style.add_modifier(Modifier::UNDERLINED);
                        }
                        Span::styled(text.trim_end_matches(['\r', '\n']).to_owned(), style)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}
