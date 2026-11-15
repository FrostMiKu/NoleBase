//! Normalize TeX's paired delimiters for Markdown math renderers.
use super::{parse, Event};
use pulldown_cmark::{Event as Markdown, Tag};
use std::borrow::Cow;

pub(crate) fn normalize_math_delimiters(source: &str) -> Cow<'_, str> {
    if !source.contains("\\(") && !source.contains("\\[") {
        return Cow::Borrowed(source);
    }
    let protected = parse(source)
        .into_iter()
        .filter_map(|item| {
            matches!(
                item.event,
                Event::Markdown(
                    Markdown::Code(_)
                        | Markdown::InlineMath(_)
                        | Markdown::DisplayMath(_)
                        | Markdown::Html(_)
                        | Markdown::InlineHtml(_)
                        | Markdown::Start(Tag::CodeBlock(_) | Tag::Link { .. } | Tag::Image { .. })
                )
            )
            .then_some(item.span)
        })
        .collect::<Vec<_>>();
    let mut output = String::new();
    let mut copied = 0;
    let mut cursor = 0;
    while let Some(relative) = source[cursor..].find('\\') {
        let start = cursor + relative;
        cursor = start + 1;
        let (closing, display) = if source[start..].starts_with("\\(") {
            ("\\)", false)
        } else if source[start..].starts_with("\\[") {
            ("\\]", true)
        } else {
            continue;
        };
        if protected.iter().any(|range| range.contains(&start))
            || source[..start]
                .bytes()
                .rev()
                .take_while(|byte| *byte == b'\\')
                .count()
                % 2
                == 1
        {
            continue;
        }
        let Some(relative_end) = source[start + 2..].find(closing) else {
            continue;
        };
        let end = start + 2 + relative_end;
        if protected
            .iter()
            .any(|range| range.start < end + 2 && range.end > start)
            || source[..end]
                .bytes()
                .rev()
                .take_while(|byte| *byte == b'\\')
                .count()
                % 2
                == 1
        {
            continue;
        }
        let value = source[start + 2..end].trim();
        if value.is_empty() {
            continue;
        }
        output.push_str(&source[copied..start]);
        if display {
            output.push_str("\n\n$$\n");
            output.push_str(value);
            output.push_str("\n$$\n\n");
        } else {
            output.push('$');
            output.push_str(value);
            output.push('$');
        }
        cursor = end + 2;
        copied = cursor;
    }
    if copied == 0 {
        Cow::Borrowed(source)
    } else {
        output.push_str(&source[copied..]);
        Cow::Owned(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paired_tex_delimiters_normalize_in_prose_and_keep_literal_regions() {
        let source = "Inline \\(x^2\\).\n\n\\[\\frac{a}{b}\\]\n\n`\\(code\\)`\n\n```latex\n\\[code\\]\n```\n\n\\\\(escaped\\\\)";
        let normalized = normalize_math_delimiters(source);
        assert!(normalized.contains("Inline $x^2$."));
        assert!(normalized.contains("$$\n\\frac{a}{b}\n$$"));
        assert!(normalized.contains("`\\(code\\)`"));
        assert!(normalized.contains("```latex\n\\[code\\]\n```"));
        assert!(normalized.contains("\\\\(escaped\\\\)"));
        assert_eq!(normalize_math_delimiters("\\(unfinished"), "\\(unfinished");
    }
}
