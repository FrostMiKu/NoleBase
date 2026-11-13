use unicode_width::UnicodeWidthStr;

/// TeX rendering stays independent of the Markdown parser. Unsupported or
/// over-wide formulae retain their source in the caller.
pub(super) fn render(source: &str, width: usize) -> Option<String> {
    let source = source
        .replace("\\text{", "\\mathrm{")
        .replace("\\operatorname{", "\\mathrm{");
    let output = strip_ansi_escapes::strip_str(txm::render(&source).ok()?);
    let output = output.replace('\u{200b}', "");
    output
        .lines()
        .all(|line| UnicodeWidthStr::width(line) <= width)
        .then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_and_text_render_without_terminal_controls() {
        let fraction = render("\\frac{1}{2}", 40).expect("fraction");
        assert!(fraction.contains('1') && fraction.contains('2'));
        assert!(!fraction.contains("\\frac"));
        let text = render("\\text{中文}", 40).expect("CJK text");
        assert!(text.contains("中文"), "{text:?}");
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\u{200b}'));
    }
}
