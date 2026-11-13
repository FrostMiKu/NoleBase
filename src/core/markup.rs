//! CommonMark/GFM parsing with Nole's semantic extensions and source spans.
//! All consumers share these events, including indexing, export and the TUI.

use crate::core::{attachment::AttachmentUri, model::LinkTarget};
use pulldown_cmark::{CowStr, Event as MarkdownEvent, Options, Parser, Tag, TagEnd};
use std::{borrow::Cow, ops::Range, path::Path};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug)]
pub enum Event<'a> {
    Markdown(MarkdownEvent<'a>),
    WikiLink(CowStr<'a>),
    Embed(CowStr<'a>),
    Hashtag(CowStr<'a>),
}

#[derive(Clone, Debug)]
pub struct SpannedEvent<'a> {
    pub event: Event<'a>,
    pub span: Range<usize>,
}

/// Parse standard Markdown first. Extensions only inspect literal prose;
/// code, HTML, escaped characters and link destinations stay untouched.
pub fn parse(source: &str) -> Vec<SpannedEvent<'_>> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH;
    let mut parser = Parser::new_ext(source, options)
        .into_offset_iter()
        .peekable();
    let mut output = Vec::new();
    let mut code = false;
    while let Some((event, mut span)) = parser.next() {
        if matches!(event, MarkdownEvent::Start(Tag::CodeBlock(_))) {
            code = true;
        }
        if matches!(event, MarkdownEvent::End(TagEnd::CodeBlock)) {
            code = false;
        }
        if let MarkdownEvent::Text(text) = &event {
            if !code && source.get(span.clone()) == Some(text.as_ref()) {
                // The CommonMark parser may split adjacent bracket text. Merge
                // only byte-identical runs so escapes/entities keep their meaning.
                while let Some((MarkdownEvent::Text(next), range)) = parser.peek() {
                    if range.start != span.end || source.get(range.clone()) != Some(next.as_ref()) {
                        break;
                    }
                    span.end = range.end;
                    parser.next();
                }
                prose(source, span, &mut output);
                continue;
            }
        }
        output.push(SpannedEvent {
            event: Event::Markdown(event),
            span,
        });
    }
    output
}

fn prose<'a>(source: &'a str, span: Range<usize>, output: &mut Vec<SpannedEvent<'a>>) {
    let mut cursor = span.start;
    let mut start = cursor;
    while cursor < span.end {
        let rest = &source[cursor..span.end];
        let escaped = source[..cursor]
            .bytes()
            .rev()
            .take_while(|b| *b == b'\\')
            .count()
            % 2
            == 1;
        let mut token = None;
        if !escaped {
            let prefix = if rest.starts_with("![[") {
                3
            } else if rest.starts_with("[[") {
                2
            } else {
                0
            };
            if prefix > 0 {
                if let Some(close) = rest[prefix..].find("]]") {
                    let target = &rest[prefix..prefix + close];
                    if !target.is_empty()
                        && target.trim() == target
                        && !target.contains(['[', ']', '\n', '\r'])
                    {
                        let event = if prefix == 3 {
                            Event::Embed(target.into())
                        } else {
                            Event::WikiLink(target.into())
                        };
                        token = Some((event, cursor + prefix + close + 2));
                    }
                }
            } else if rest.starts_with('#')
                && (cursor == 0
                    || source[..cursor]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
            {
                let name = &rest[1..];
                if name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    let length = name
                        .char_indices()
                        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
                        .map_or(name.len(), |(i, _)| i);
                    token = Some((Event::Hashtag(name[..length].into()), cursor + 1 + length));
                }
            }
        }
        if let Some((event, end)) = token {
            if start < cursor {
                output.push(SpannedEvent {
                    event: Event::Markdown(MarkdownEvent::Text(source[start..cursor].into())),
                    span: start..cursor,
                });
            }
            output.push(SpannedEvent {
                event,
                span: cursor..end,
            });
            cursor = end;
            start = end;
        } else {
            cursor += rest.chars().next().unwrap().len_utf8();
        }
    }
    if start < span.end {
        output.push(SpannedEvent {
            event: Event::Markdown(MarkdownEvent::Text(source[start..span.end].into())),
            span: start..span.end,
        });
    }
}

pub fn expand_tabs(source: &str) -> Cow<'_, str> {
    if !source.contains('\t') {
        return Cow::Borrowed(source);
    }

    const TAB_STOP: usize = 4;
    let mut expanded = String::with_capacity(source.len());
    let mut column = 0;
    for character in source.chars() {
        match character {
            '\t' => {
                let spaces = TAB_STOP - column % TAB_STOP;
                expanded.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            '\n' | '\r' => {
                expanded.push(character);
                column = 0;
            }
            _ => {
                expanded.push(character);
                column += UnicodeWidthChar::width(character).unwrap_or(0);
            }
        }
    }
    Cow::Owned(expanded)
}

pub struct LinkSpec {
    pub label: String,
    pub target: LinkTarget,
}
#[derive(Clone)]
pub struct CodeBlockSpec {
    pub language: String,
    pub source: String,
}

pub fn code_block_specs(events: &[SpannedEvent<'_>]) -> Vec<CodeBlockSpec> {
    let mut blocks = Vec::new();
    let mut current: Option<CodeBlockSpec> = None;
    for item in events {
        match &item.event {
            Event::Markdown(MarkdownEvent::Start(Tag::CodeBlock(kind))) => {
                let language = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("text")
                    }
                    _ => "text",
                };
                current = Some(CodeBlockSpec {
                    language: language.to_string(),
                    source: String::new(),
                });
            }
            Event::Markdown(MarkdownEvent::Text(text)) => {
                if let Some(block) = &mut current {
                    block.source.push_str(text);
                }
            }
            Event::Markdown(MarkdownEvent::End(TagEnd::CodeBlock)) => {
                if let Some(block) = current.take() {
                    blocks.push(block);
                }
            }
            _ => {}
        }
    }
    blocks
}

pub fn link_target(raw: &str) -> LinkTarget {
    if AttachmentUri::is_attachment_uri(raw) {
        LinkTarget::Attachment(raw.to_string())
    } else if raw.is_empty()
        || raw.starts_with(['#', '?'])
        || raw.starts_with("//")
        || (!Path::new(raw).is_absolute() && reqwest::Url::parse(raw).is_ok())
    {
        LinkTarget::External(raw.to_string())
    } else {
        LinkTarget::LocalFile(raw.into())
    }
}

pub fn embed_link_target(raw: &str) -> LinkTarget {
    if AttachmentUri::is_attachment_uri(raw) {
        LinkTarget::Attachment(raw.to_string())
    } else {
        LinkTarget::LocalFile(raw.into())
    }
}

pub fn link_specs(events: &[SpannedEvent<'_>]) -> Vec<LinkSpec> {
    let mut links = Vec::new();
    for (index, item) in events.iter().enumerate() {
        match &item.event {
            Event::Markdown(MarkdownEvent::Start(Tag::Link { dest_url, .. })) => {
                let end = events[index + 1..]
                    .iter()
                    .position(|e| {
                        matches!(e.event, Event::Markdown(MarkdownEvent::End(TagEnd::Link)))
                    })
                    .map_or(events.len(), |offset| index + 1 + offset);
                links.push(LinkSpec {
                    label: visible_text(&events[index + 1..end]),
                    target: link_target(dest_url),
                });
            }
            Event::WikiLink(target) => links.push(LinkSpec {
                label: format!("[[{target}]]"),
                target: LinkTarget::WikiLink(target.to_string()),
            }),
            Event::Embed(target) => links.push(LinkSpec {
                label: format!("![[{target}]]"),
                target: embed_link_target(target),
            }),
            _ => {}
        }
    }
    links
}

pub fn visible_text(events: &[SpannedEvent<'_>]) -> String {
    let mut text = String::new();
    for item in events {
        match &item.event {
            Event::Markdown(
                MarkdownEvent::Text(value)
                | MarkdownEvent::Code(value)
                | MarkdownEvent::Html(value)
                | MarkdownEvent::InlineHtml(value)
                | MarkdownEvent::InlineMath(value)
                | MarkdownEvent::DisplayMath(value),
            ) => text.push_str(value),
            Event::Hashtag(tag) => text.push_str(&format!("#{tag}")),
            Event::WikiLink(target) => text.push_str(&format!("[[{target}]]")),
            Event::Embed(target) => text.push_str(&format!("![[{target}]]")),
            Event::Markdown(MarkdownEvent::SoftBreak | MarkdownEvent::HardBreak) => text.push(' '),
            _ => {}
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_keep_exact_source_spans_and_skip_non_prose() {
        let source = concat!(
            "See [[计划]] #开发/日志 ![[report.pdf]]\n\n",
            "`[[code]] #code` \\[[escaped]] \\#escaped &#35;entity\n\n",
            "<!-- [[comment]] #comment -->\n\n",
            "[label](https://example.test/#fragment)\n\n",
            "```text\n[[fenced]] #fenced\n```\n\n",
            "    [[indented]] #indented\n\n",
            "$x + #math$\n"
        );
        let events = parse(source);
        let semantic: Vec<_> = events
            .iter()
            .filter(|item| !matches!(item.event, Event::Markdown(_)))
            .map(|item| &source[item.span.clone()])
            .collect();
        assert_eq!(semantic, ["[[计划]]", "#开发/日志", "![[report.pdf]]"]);
    }

    #[test]
    fn standard_markdown_and_literal_bbcode_share_one_parse() {
        let source = "**bold** [b]plain[/b]\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n\n~~gone~~ $x^2$";
        let events = parse(source);
        assert!(events.iter().any(|item| matches!(
            item.event,
            Event::Markdown(MarkdownEvent::Start(Tag::Strong))
        )));
        assert!(events.iter().any(|item| matches!(
            item.event,
            Event::Markdown(MarkdownEvent::Start(Tag::Table(_)))
        )));
        assert!(events.iter().any(|item| matches!(
            item.event,
            Event::Markdown(MarkdownEvent::TaskListMarker(true))
        )));
        assert!(events.iter().any(|item| matches!(
            item.event,
            Event::Markdown(MarkdownEvent::Start(Tag::Strikethrough))
        )));
        assert!(events
            .iter()
            .any(|item| matches!(item.event, Event::Markdown(MarkdownEvent::InlineMath(_)))));
        assert!(visible_text(&events).contains("[b]plain[/b]"));
    }
}
