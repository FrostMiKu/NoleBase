use std::sync::OnceLock;

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

pub(super) fn lines(source: &str, language: &str, base: Style) -> Vec<Line<'static>> {
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
