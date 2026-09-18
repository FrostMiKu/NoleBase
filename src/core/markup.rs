//! Pure MBDown AST traversal and extraction shared by indexing, the agent,
//! and the TUI renderer. Depends on mbdown and core types only; rendering
//! lives in the TUI markdown renderer module.

use std::borrow::Cow;
use std::path::Path;

use mbdown::{Container, ContainerEnd, Event, InlineTag, Node};
use unicode_width::UnicodeWidthChar;

use crate::core::attachment::AttachmentUri;
use crate::core::model::LinkTarget;

/// Visit every Markdown leaf in document order, hiding MBDown's structural
/// container recursion from consumers that only inspect parsed events.
pub(crate) fn visit_markdown<'a>(
    nodes: &[Node<'a>],
    visitor: &mut impl FnMut(&mbdown::Markdown<'a>),
) {
    for node in nodes {
        match node {
            Node::Markdown(markdown) => visitor(markdown),
            Node::Box { children, .. }
            | Node::Center { children }
            | Node::Right { children }
            | Node::Indent { children, .. }
            | Node::Columns { children, .. }
            | Node::Column { children, .. } => visit_markdown(children, visitor),
        }
    }
}

pub(crate) fn expand_tabs(source: &str) -> Cow<'_, str> {
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

pub(crate) struct LinkSpec {
    pub label: String,
    pub target: LinkTarget,
}

#[derive(Clone)]
pub(crate) struct CodeBlockSpec {
    pub language: String,
    pub source: String,
}

pub(crate) fn code_block_specs(nodes: &[Node<'_>]) -> Vec<CodeBlockSpec> {
    let mut blocks = Vec::new();
    collect_code_block_specs(nodes, &mut blocks);
    blocks
}

fn collect_code_block_specs(nodes: &[Node<'_>], blocks: &mut Vec<CodeBlockSpec>) {
    visit_markdown(nodes, &mut |markdown| {
        collect_event_code_blocks(markdown.events(), blocks)
    });
}

fn collect_event_code_blocks(events: &[mbdown::SpannedEvent<'_>], blocks: &mut Vec<CodeBlockSpec>) {
    let mut index = 0;
    while index < events.len() {
        let Event::Start(Container::CodeBlock(info)) = &events[index].event else {
            index += 1;
            continue;
        };
        let end = events[index + 1..]
            .iter()
            .position(|item| matches!(item.event, Event::End(ContainerEnd::CodeBlock)))
            .map_or(events.len(), |offset| index + 1 + offset);
        let source = events[index + 1..end]
            .iter()
            .filter_map(|item| match &item.event {
                Event::Text(text)
                | Event::Code(text)
                | Event::Html(text)
                | Event::InlineHtml(text) => Some(text.as_ref()),
                _ => None,
            })
            .collect();
        blocks.push(CodeBlockSpec {
            language: info
                .as_deref()
                .and_then(code_language)
                .unwrap_or("text")
                .to_string(),
            source,
        });
        index = end.saturating_add(1);
    }
}

fn code_language(info: &str) -> Option<&str> {
    let language = info
        .split_whitespace()
        .next()?
        .trim_matches(|character| matches!(character, '{' | '}' | '.'));
    (!language.is_empty()).then_some(language)
}

/// Classify a raw Markdown or BBCode link target. Managed attachment URIs stay
/// attachment targets even when malformed, absolute and relative file paths
/// become local files, and URI-like targets remain external. Relative local
/// files are rebased against the containing document by the UI.
fn link_target(raw: &str) -> LinkTarget {
    if AttachmentUri::is_attachment_uri(raw) {
        LinkTarget::Attachment(raw.to_string())
    } else if raw.is_empty()
        || raw.starts_with('#')
        || raw.starts_with('?')
        || raw.starts_with("//")
        || (!Path::new(raw).is_absolute() && reqwest::Url::parse(raw).is_ok())
    {
        LinkTarget::External(raw.to_string())
    } else {
        LinkTarget::LocalFile(raw.into())
    }
}

/// Classify an embed (`![[...]]`) target: attachment URIs become attachment
/// links, everything else a local file.
pub(crate) fn embed_link_target(raw: &str) -> LinkTarget {
    if AttachmentUri::is_attachment_uri(raw) {
        LinkTarget::Attachment(raw.to_string())
    } else {
        LinkTarget::LocalFile(raw.into())
    }
}

pub(crate) fn link_specs(nodes: &[Node<'_>]) -> Vec<LinkSpec> {
    let mut links = Vec::new();
    collect_link_specs(nodes, &mut links);
    links
}

fn collect_link_specs(nodes: &[Node<'_>], links: &mut Vec<LinkSpec>) {
    visit_markdown(nodes, &mut |markdown| {
        collect_event_links(markdown.events(), links)
    });
}

fn collect_event_links(events: &[mbdown::SpannedEvent<'_>], links: &mut Vec<LinkSpec>) {
    let mut index = 0;
    while index < events.len() {
        match &events[index].event {
            Event::Start(Container::Link { target, .. }) => {
                let end = events[index + 1..]
                    .iter()
                    .position(|item| matches!(item.event, Event::End(ContainerEnd::Link)))
                    .map_or(events.len(), |offset| index + 1 + offset);
                links.push(LinkSpec {
                    label: visible_event_text(&events[index + 1..end]),
                    target: link_target(target),
                });
                index = end.saturating_add(1);
                continue;
            }
            Event::WikiLink(target) => links.push(LinkSpec {
                label: format!("[[{target}]]"),
                target: LinkTarget::WikiLink(target.to_string()),
            }),
            Event::InlineTag(InlineTag {
                name,
                value: Some(target),
                closing: false,
                ..
            }) if name == "link" => {
                let end = events[index + 1..]
                    .iter()
                    .position(|item| {
                        matches!(
                            &item.event,
                            Event::InlineTag(InlineTag {
                                name,
                                closing: true,
                                ..
                            }) if name == "link"
                        )
                    })
                    .map_or(events.len(), |offset| index + 1 + offset);
                links.push(LinkSpec {
                    label: visible_event_text(&events[index + 1..end]),
                    target: link_target(target),
                });
                index = end.saturating_add(1);
                continue;
            }
            _ => {}
        }
        index += 1;
    }
}

fn visible_event_text(events: &[mbdown::SpannedEvent<'_>]) -> String {
    let mut text = String::new();
    for item in events {
        match &item.event {
            Event::Text(value)
            | Event::Code(value)
            | Event::Html(value)
            | Event::InlineHtml(value) => text.push_str(value),
            Event::Hashtag(tag) => {
                text.push('#');
                text.push_str(tag);
            }
            Event::WikiLink(target) => text.push_str(&format!("[[{target}]]")),
            Event::Embed(target) => text.push_str(&format!("![[{target}]]")),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    text
}
