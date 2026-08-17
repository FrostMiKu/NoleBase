//! MBDown markup -> styled ratatui lines.
//!
//! MBDown owns the language and syntax tree. MBTUI owns Nole's Ratatui layout.

use std::borrow::Cow;
use std::path::Path;

use mbdown::{Container, ContainerEnd, Event, InlineTag, Node};
use mbtui::Renderer;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::attachment::AttachmentUri;
use crate::model::LinkTarget;
use crate::theme::Theme;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedMarkup {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<RenderedLink>,
    pub tags: Vec<RenderedTag>,
    pub images: Vec<mbtui::ImagePlacement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLink {
    pub target: LinkTarget,
    pub row: usize,
    pub column: usize,
    pub width: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedTag {
    pub name: String,
    pub row: usize,
    pub column: usize,
    pub width: usize,
}

/// Render markup for an exact terminal display width.
pub fn to_lines_at_width(source: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    render_at_width(source, width, theme).lines
}

pub fn render_at_width(source: &str, width: usize, theme: Theme) -> RenderedMarkup {
    let expanded = expand_tabs(source);
    match mbdown::parse(expanded.as_ref()) {
        Ok(document) => {
            let code_specs = if matches!(expanded, Cow::Borrowed(_)) {
                code_block_specs(document.nodes())
            } else {
                mbdown::parse(source)
                    .map(|original| code_block_specs(original.nodes()))
                    .unwrap_or_default()
            };
            let rendered = Renderer::with_theme(width.max(1), theme.markdown_theme())
                .with_image_height(12)
                .render(&document);
            let mut lines = rendered.text.lines;
            let specs = link_specs(document.nodes());
            let mut links = locate_links(&lines, &specs, theme.markdown_link);
            let mut tags = Vec::new();
            for semantic in rendered.semantics {
                match semantic.kind {
                    mbtui::SemanticKind::Hashtag { name } => {
                        tags.extend(semantic.regions.into_iter().map(|region| RenderedTag {
                            name: name.clone(),
                            row: region.row,
                            column: region.column,
                            width: region.width,
                        }));
                    }
                    mbtui::SemanticKind::Embed { target } => {
                        let target = embed_link_target(&target);
                        links.extend(semantic.regions.into_iter().map(|region| RenderedLink {
                            target: target.clone(),
                            row: region.row,
                            column: region.column,
                            width: region.width,
                        }));
                    }
                    _ => {}
                }
            }
            links.extend(decorate_code_blocks(&mut lines, &code_specs, theme));
            RenderedMarkup {
                lines,
                links,
                tags,
                images: rendered.images,
            }
        }
        Err(_) => RenderedMarkup {
            lines: plain_text_lines(expanded.as_ref(), width.max(1)),
            links: Vec::new(),
            tags: Vec::new(),
            images: Vec::new(),
        },
    }
}

fn plain_text_lines(source: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for source_line in source.split('\n') {
        let source_line = source_line.strip_suffix('\r').unwrap_or(source_line);
        let mut row = String::new();
        let mut row_width = 0;
        for character in source_line.chars() {
            let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
            if row_width + character_width > width && !row.is_empty() {
                lines.push(Line::raw(std::mem::take(&mut row)));
                row_width = 0;
            }
            row.push(character);
            row_width += character_width;
        }
        lines.push(Line::raw(row));
    }
    lines
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

struct LinkSpec {
    label: String,
    target: LinkTarget,
}

const COPY_BUTTON: &str = " Copy ";
const CODE_BLOCK_PADDING: usize = 2;

#[derive(Clone)]
struct CodeBlockSpec {
    language: String,
    source: String,
}

fn code_block_specs(nodes: &[Node<'_>]) -> Vec<CodeBlockSpec> {
    let mut blocks = Vec::new();
    collect_code_block_specs(nodes, &mut blocks);
    blocks
}

fn collect_code_block_specs(nodes: &[Node<'_>], blocks: &mut Vec<CodeBlockSpec>) {
    for node in nodes {
        match node {
            Node::Markdown(markdown) => collect_event_code_blocks(markdown.events(), blocks),
            Node::Box { children, .. }
            | Node::Center { children }
            | Node::Right { children }
            | Node::Indent { children, .. }
            | Node::Columns { children, .. }
            | Node::Column { children, .. } => collect_code_block_specs(children, blocks),
        }
    }
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

/// Overlay a right-aligned action on each rendered fenced-code header and
/// expose it through the existing clickable-region pipeline used by every view.
fn decorate_code_blocks(
    lines: &mut [Line<'static>],
    specs: &[CodeBlockSpec],
    theme: Theme,
) -> Vec<RenderedLink> {
    let background = Some(theme.markdown_code_block_background);
    let label_color = Some(theme.markdown_code_label);
    let button_width = UnicodeWidthStr::width(COPY_BUTTON);
    let mut used = Vec::new();
    let mut actions = Vec::new();

    for spec in specs {
        let mut candidate = None;
        'rows: for (row, line) in lines.iter().enumerate() {
            let mut column = 0;
            for (span_index, span) in line.spans.iter().enumerate() {
                let span_width = UnicodeWidthStr::width(span.content.as_ref());
                let label = span.content.trim();
                if span.style.bg == background
                    && span.style.fg == label_color
                    && !label.is_empty()
                    && spec.language.starts_with(label)
                    && !used.contains(&(row, column))
                {
                    let mut region_end = span_index;
                    while region_end + 1 < line.spans.len()
                        && line.spans[region_end + 1].style.bg == background
                    {
                        region_end += 1;
                    }
                    let trailing = &line.spans[region_end];
                    let trailing_width = UnicodeWidthStr::width(trailing.content.as_ref());
                    if trailing.content.chars().all(char::is_whitespace)
                        && trailing_width >= button_width + CODE_BLOCK_PADDING
                    {
                        candidate = Some((row, column, region_end, trailing_width));
                        break 'rows;
                    }
                }
                column += span_width;
            }
        }

        let Some((row, label_column, trailing_index, trailing_width)) = candidate else {
            continue;
        };
        used.push((row, label_column));
        let trailing_start = lines[row].spans[..trailing_index]
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>();
        let before = trailing_width - button_width - CODE_BLOCK_PADDING;
        let button_column = trailing_start + before;
        let trailing_style = lines[row].spans[trailing_index].style;
        lines[row].spans.splice(
            trailing_index..=trailing_index,
            [
                Span::styled(" ".repeat(before), trailing_style),
                Span::styled(
                    COPY_BUTTON,
                    Style::default()
                        .fg(theme.text_muted)
                        .bg(theme.markdown_code_block_background)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ".repeat(CODE_BLOCK_PADDING), trailing_style),
            ],
        );
        actions.push(RenderedLink {
            target: LinkTarget::CopyCode(spec.source.clone()),
            row,
            column: button_column,
            width: button_width,
        });
    }
    actions
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
fn embed_link_target(raw: &str) -> LinkTarget {
    if AttachmentUri::is_attachment_uri(raw) {
        LinkTarget::Attachment(raw.to_string())
    } else {
        LinkTarget::LocalFile(raw.into())
    }
}

#[derive(Clone)]
struct RenderedCell {
    character: char,
    row: usize,
    column: usize,
    width: usize,
    clickable: bool,
}

fn link_specs(nodes: &[Node<'_>]) -> Vec<LinkSpec> {
    let mut links = Vec::new();
    collect_link_specs(nodes, &mut links);
    links
}

fn collect_link_specs(nodes: &[Node<'_>], links: &mut Vec<LinkSpec>) {
    for node in nodes {
        match node {
            Node::Markdown(markdown) => collect_event_links(markdown.events(), links),
            Node::Box { children, .. }
            | Node::Center { children }
            | Node::Right { children }
            | Node::Indent { children, .. }
            | Node::Columns { children, .. }
            | Node::Column { children, .. } => collect_link_specs(children, links),
        }
    }
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

fn locate_links(
    lines: &[Line<'_>],
    specs: &[LinkSpec],
    link_underline_color: Color,
) -> Vec<RenderedLink> {
    let cells = rendered_cells(lines, link_underline_color)
        .into_iter()
        .filter(|cell| cell.clickable)
        .collect::<Vec<_>>();
    let mut used = vec![false; cells.len()];
    let mut links = Vec::new();
    for spec in specs.iter().filter(|spec| !spec.label.is_empty()) {
        let needle = spec.label.chars().collect::<Vec<_>>();
        let matched = find_cells(&cells, &needle, &used).map(|range| {
            for used in &mut used[range.clone()] {
                *used = true;
            }
            cells[range].to_vec()
        });
        if let Some(matched) = matched {
            links.extend(link_segments(&matched, &spec.target));
        }
    }
    links
}

fn rendered_cells(lines: &[Line<'_>], link_underline_color: Color) -> Vec<RenderedCell> {
    let mut cells = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let mut column = 0;
        for span in &line.spans {
            let clickable = span.style.underline_color == Some(link_underline_color);
            for character in span.content.chars() {
                let width = UnicodeWidthChar::width(character).unwrap_or(0);
                if width > 0 {
                    cells.push(RenderedCell {
                        character,
                        row,
                        column,
                        width,
                        clickable,
                    });
                    column += width;
                }
            }
        }
    }
    cells
}

fn find_cells(
    cells: &[RenderedCell],
    needle: &[char],
    used: &[bool],
) -> Option<std::ops::Range<usize>> {
    if needle.is_empty() || cells.len() < needle.len() {
        return None;
    }
    (0..=cells.len() - needle.len())
        .find(|start| {
            cells[*start..*start + needle.len()]
                .iter()
                .map(|cell| cell.character)
                .eq(needle.iter().copied())
                && !used[*start..*start + needle.len()].iter().any(|used| *used)
        })
        .map(|start| start..start + needle.len())
}

fn link_segments(cells: &[RenderedCell], target: &LinkTarget) -> Vec<RenderedLink> {
    let mut links: Vec<RenderedLink> = Vec::new();
    for cell in cells {
        if let Some(last) = links.last_mut().filter(|last| {
            last.row == cell.row && last.column.saturating_add(last.width) == cell.column
        }) {
            last.width += cell.width;
        } else {
            links.push(RenderedLink {
                target: target.clone(),
                row: cell.row,
                column: cell.column,
                width: cell.width,
            });
        }
    }
    links
}

/// Map a one-based source line to its first terminal row after MBTUI layout.
#[cfg(test)]
pub fn rendered_row_for_source_line(
    source: &str,
    line_no: usize,
    width: usize,
    theme: Theme,
) -> usize {
    let rendered = to_lines_at_width(source, width, theme);
    rendered_row_for_source_line_in(source, line_no, &rendered, width, theme)
}

/// Map a source line using an existing complete render.
pub fn rendered_row_for_source_line_in(
    source: &str,
    line_no: usize,
    rendered: &[Line<'_>],
    width: usize,
    theme: Theme,
) -> usize {
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
        theme,
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
    use crate::theme::catppuccin as ctp;

    fn to_lines_at_width(source: &str, width: usize) -> Vec<Line<'static>> {
        super::to_lines_at_width(source, width, Theme::default())
    }

    fn render_at_width(source: &str, width: usize) -> RenderedMarkup {
        super::render_at_width(source, width, Theme::default())
    }

    fn rendered_row_for_source_line(source: &str, line_no: usize, width: usize) -> usize {
        super::rendered_row_for_source_line(source, line_no, width, Theme::default())
    }

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
    fn renders_nested_lists_with_hierarchical_indent() {
        let output = text(&to_lines_at_width("- AAA\n  - BBB\n- CCC", WIDTH));
        assert_eq!(output, " • AAA\n    • BBB\n • CCC");
    }

    #[test]
    fn expands_tabs_before_rendering_terminal_text() {
        let lines = to_lines_at_width(
            "```text\n1:- 今天健完身\n3:\t- @所有人\n4:\t  1. 注意简表\n```",
            48,
        );
        assert!(lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| !span.content.contains('\t')));
        let output = text(&lines);
        assert!(output.contains("3:  - @所有人"), "{output:?}");
        assert!(output.contains("4:    1. 注意简表"), "{output:?}");
        assert!(output
            .lines()
            .all(|line| UnicodeWidthStr::width(line) <= 48));
    }

    #[test]
    fn code_block_copy_action_is_right_aligned_and_keeps_exact_source() {
        let rendered = render_at_width("```rust\nfn main() {\n\tprintln!(\"hi\");\n}\n```", 48);
        let action = rendered
            .links
            .iter()
            .find(|link| matches!(link.target, LinkTarget::CopyCode(_)))
            .expect("code copy action");
        assert_eq!(
            action.target,
            LinkTarget::CopyCode("fn main() {\n\tprintln!(\"hi\");\n}\n".to_string())
        );
        assert_eq!(action.width, UnicodeWidthStr::width(COPY_BUTTON));
        assert_eq!(action.column + action.width + CODE_BLOCK_PADDING, 48);
        let header = rendered.lines[action.row]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(header.contains("rust"));
        assert!(header.contains(COPY_BUTTON));
        let button = rendered.lines[action.row]
            .spans
            .iter()
            .find(|span| span.content == COPY_BUTTON)
            .expect("copy button span");
        assert_eq!(button.style.fg, Some(Theme::default().text_muted));
    }

    #[test]
    fn invalid_mbdown_falls_back_to_width_bounded_plain_text() {
        let source = "# Visible source\n\n[box border=double]\nbody\n[/box]";
        let rendered = render_at_width(source, WIDTH);

        assert_eq!(text(&rendered.lines), source);
        assert!(rendered.links.is_empty());
        assert!(rendered.tags.is_empty());
        assert!(rendered.images.is_empty());

        let wrapped = render_at_width("[box border=double] abcdefghijklmnop", 10);
        assert!(wrapped
            .lines
            .iter()
            .all(|line| UnicodeWidthStr::width(line.to_string().as_str()) <= 10));
        assert!(!text(&wrapped.lines).contains("MBDown parse error"));
    }

    #[test]
    fn mermaid_fallback_shows_a_concise_error_status() {
        let output = text(&to_lines_at_width(
            "```mermaid\nclassDiagram\nclass Agent~T~ {}\n```",
            80,
        ));
        let label = output
            .lines()
            .find(|line| line.contains("mermaid"))
            .unwrap();
        assert!(label.trim_end().ends_with("unsupported"));
        assert!(!output.contains("generics not yet supported"));
        assert!(output.contains("class Agent~T~ {}"));
    }

    #[test]
    fn renders_mermaid_fences_as_unicode_diagrams() {
        let lines = to_lines_at_width(
            "```mermaid\nsequenceDiagram\n    Alice->>Bob: Hello\n```",
            60,
        );
        let output = text(&lines);

        assert!(output.contains("Alice"));
        assert!(output.contains("Bob"));
        assert!(output.contains("Hello"));
        assert!(!output.contains("sequenceDiagram"));
        assert!(!output.contains("mermaid"));
        assert!(output
            .lines()
            .all(|line| UnicodeWidthStr::width(line) <= 60));
    }

    #[test]
    fn renders_cjk_mermaid_without_wide_cell_placeholders() {
        for (source, labels) in [
            (
                "```mermaid\nflowchart LR\n    A[开始] --> B{是否满意?}\n    B -->|否| C[继续改进]\n```",
                &["开始", "是否满意?", "继续改进"][..],
            ),
            (
                "```mermaid\nsequenceDiagram\n    participant U as 用户\n    participant N as Nole\n    U->>N: 输入指令\n    N-->>U: 展示响应\n```",
                &["用户", "输入指令", "展示响应"][..],
            ),
            (
                "```mermaid\ngantt\n    dateFormat YYYY-MM-DD\n    section 核心功能\n    Markdown 支持: 2026-01-01, 30d\n    AI 集成: 2026-01-15, 30d\n```",
                &["核心功能", "Markdown 支持", "AI 集成"][..],
            ),
        ] {
            let lines = to_lines_at_width(source, 80);
            let output = text(&lines);

            for label in labels {
                assert!(output.contains(label), "missing {label:?}:\n{output}");
            }
            assert!(!output.contains("开 始"));
            assert!(!output.contains("输 入"));
            assert!(!output.contains("支 持"));
            assert!(!output.contains("mermaid"));
            assert!(output
                .lines()
                .all(|line| UnicodeWidthStr::width(line) <= 80));
        }
    }

    #[test]
    fn renders_exact_mermaid_bad_cases_at_supported_widths() {
        let fixtures = [
            (
                "graph LR\n    A[开始] --> B{条件判断}\n    B -->|是| C(执行操作 A)\n    B -->|否| D(执行操作 B)\n    C --> E[结束]\n    D --> E\n    C --> F((检查点))\n    F --> B\n",
                &["开始", "条件判断", "执行操作 A", "执行操作 B", "结束", "检查点"][..],
            ),
            (
                "stateDiagram-v2\n    [*] --> Idle\n    Idle --> Processing: 接收任务\n    Processing --> Running: 开始执行\n    Running --> Success: 完成\n    Running --> Error: 失败\n    Success --> [*]\n    Error --> Retry: 重试\n    Retry --> Processing\n    Error --> [*]: 放弃\n",
                &["Idle", "Processing", "Running", "Success", "Error", "Retry", "接收任务", "开始执行", "完成", "失败", "重试", "放弃"][..],
            ),
            (
                "mindmap\n  root((Nole))\n    核心\n      编辑器\n        MBDown 渲染\n        Mermaid 图表\n        BBCode 样式\n      文件管理\n        data 笔记\n        daily 日志\n        archives 归档\n    AI Agent\n      工具\n        文件读写\n        搜索\n        网络请求\n      记忆\n        MEMORY.md\n        每日笔记\n    主题\n      TOML 配置\n      自定义颜色\n",
                &["Nole", "核心", "编辑器", "MBDown 渲染", "文件管理", "AI Agent", "MEMORY.md", "主题", "自定义颜色"][..],
            ),
        ];

        for width in [60, 80, 120] {
            for (source, labels) in fixtures {
                let fenced = format!("```mermaid\n{source}```");
                let output = text(&to_lines_at_width(&fenced, width));

                let fell_back = output.contains("mermaid");
                assert!(
                    !fell_back || width == 60,
                    "renderer unexpectedly fell back at width {width}:\n{output}"
                );
                for label in labels {
                    assert!(
                        output.contains(label),
                        "missing {label:?} at width {width}:\n{output}"
                    );
                }
                assert!(
                    output
                        .lines()
                        .all(|line| UnicodeWidthStr::width(line) <= width),
                    "diagram or fallback exceeded width {width}:\n{output}"
                );
            }
        }
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
    fn renders_mbdown_hashtags_and_wikilinks() {
        let lines = to_lines_at_width("See #开发/日志 and [[项目计划]]", WIDTH);
        assert_eq!(span_with(&lines, "#开发/日志").style.fg, Some(ctp::PINK));
        let wikilink = span_with(&lines, "[[项目计划]]");
        assert_eq!(wikilink.style.fg, Some(ctp::SKY));
        assert!(wikilink.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn reports_markdown_image_rows_without_embedding_terminal_escapes() {
        let rendered = render_at_width(
            "Before\n\n![Diagram](assets/diagram.png \"Overview\")\n\nAfter",
            30,
        );
        assert_eq!(rendered.images.len(), 1);
        let image = &rendered.images[0];
        assert_eq!(image.source, "assets/diagram.png");
        assert_eq!(image.title, "Overview");
        assert_eq!(image.alt, "Diagram");
        assert_eq!((image.column, image.width, image.height), (0, 30, 12));
        assert!(rendered.lines[image.row..image.row + image.height]
            .iter()
            .all(|line| line.spans.is_empty()));
    }

    #[test]
    fn renders_image_and_file_embeds_through_distinct_outputs() {
        let image = render_at_width("![[assets/pic.png]]", 30);
        assert_eq!(image.images.len(), 1);
        assert_eq!(image.images[0].source, "assets/pic.png");
        assert!(image.links.is_empty());

        let file = render_at_width("Open ![[attachments/report.pdf]]", 40);
        assert!(file.images.is_empty());
        assert!(file
            .links
            .iter()
            .any(|link| { link.target == LinkTarget::LocalFile("attachments/report.pdf".into()) }));
        assert!(text(&file.lines).contains("![[attachments/report.pdf]]"));
    }

    #[test]
    fn classifies_relative_files_and_uri_links_by_markdown_semantics() {
        let rendered = render_at_width(
            "[report](reports/result.pdf) [parent](../shared.pdf) [site](https://example.test) [mail](mailto:a@example.test) [heading](#details)",
            120,
        );

        assert!(rendered
            .links
            .iter()
            .any(|link| { link.target == LinkTarget::LocalFile("reports/result.pdf".into()) }));
        assert!(rendered
            .links
            .iter()
            .any(|link| { link.target == LinkTarget::LocalFile("../shared.pdf".into()) }));
        for target in ["https://example.test", "mailto:a@example.test", "#details"] {
            assert!(rendered
                .links
                .iter()
                .any(|link| { link.target == LinkTarget::External(target.to_string()) }));
        }
    }

    #[test]
    fn attachment_links_are_clickable_and_never_external() {
        let uri = format!(
            "nole://attachment/{}",
            "550e8400-e29b-41d4-a716-446655440000"
        );
        let rendered = render_at_width(&format!("[report]({uri})"), WIDTH);
        assert!(rendered
            .links
            .iter()
            .any(|link| { link.target == LinkTarget::Attachment(uri.clone()) && link.width == 6 }));

        // A malformed attachment URI still classifies as an attachment link so
        // it can never reach a web opener; activation reports the parse error.
        let rendered = render_at_width("[bad](nole://attachment/not-a-uuid)", WIDTH);
        assert!(rendered.links.iter().any(|link| {
            matches!(&link.target, LinkTarget::Attachment(target)
                if target == "nole://attachment/not-a-uuid")
        }));

        let rendered = render_at_width(&format!("[link={uri}]open[/link]"), WIDTH);
        assert!(rendered
            .links
            .iter()
            .any(|link| { link.target == LinkTarget::Attachment(uri.clone()) }));
    }

    #[test]
    fn attachment_embeds_become_attachment_links() {
        let uri = format!(
            "nole://attachment/{}",
            "550e8400-e29b-41d4-a716-446655440000"
        );
        let rendered = render_at_width(&format!("![[{uri}]]"), WIDTH);
        assert!(rendered.images.is_empty());
        assert!(rendered
            .links
            .iter()
            .any(|link| { link.target == LinkTarget::Attachment(uri.clone()) }));
        assert!(text(&rendered.lines).contains(&format!("![[{uri}]]")));
    }

    #[test]
    fn weather_table_with_emoji_keeps_every_row_aligned() {
        let source = concat!(
            "## 🌤️ 明天（7月28日）北京天气预报\n\n",
            "| 项目 | 内容 |\n",
            "|------|------|\n",
            "| **日期** | 2026年7月28日（周二） |\n",
            "| **天气** | ☀️ **天晴 (Fine)** |\n",
            "| **气温** | **24°C ~ 34°C** |\n",
            "| **来源** | 香港天文台 7月27日12:15发布 |",
        );
        let output = text(&to_lines_at_width(source, 60));
        let table = output
            .lines()
            .filter(|line| matches!(line.chars().next(), Some('╭' | '│' | '├' | '╰')))
            .collect::<Vec<_>>();

        assert!(!table.is_empty());
        assert!(
            table.iter().all(|line| UnicodeWidthStr::width(*line) == 60),
            "{output}"
        );
    }

    #[test]
    fn locates_markdown_wiki_and_bbcode_link_cells() {
        let rendered = render_at_width(
            "[site](https://example.test) [[项目计划]] [link=mailto:a@example.test]mail[/link]",
            WIDTH,
        );
        assert_eq!(rendered.links.len(), 3);
        assert!(rendered.links.iter().any(|link| {
            link.target == LinkTarget::External("https://example.test".to_string())
                && link.width == 4
        }));
        assert!(rendered.links.iter().any(|link| {
            link.target == LinkTarget::WikiLink("项目计划".to_string()) && link.width == 12
        }));
        assert!(rendered.links.iter().any(|link| {
            link.target == LinkTarget::External("mailto:a@example.test".to_string())
                && link.width == 4
        }));
    }

    #[test]
    fn wrapped_links_create_one_clickable_segment_per_row() {
        let rendered = render_at_width("[abcdefghij](https://example.test)", 5);
        assert_eq!(rendered.links.len(), 2);
        assert_eq!(rendered.links[0].row, 0);
        assert_eq!(rendered.links[0].width, 5);
        assert_eq!(rendered.links[1].row, 1);
        assert_eq!(rendered.links[1].width, 5);
    }

    #[test]
    fn link_locations_ignore_identical_underlined_non_link_text() {
        let rendered = render_at_width("# site\n\n[site](https://example.test)", WIDTH);
        assert_eq!(rendered.links.len(), 1);
        assert_eq!(rendered.links[0].row, 2);
    }

    #[test]
    fn renders_boxes_and_responsive_columns() {
        let box_lines = to_lines_at_width(
            "[box title=Info width=24 border=single bg=17]\nHello\n[/box]",
            40,
        );
        let boxed = text(&box_lines);
        assert!(boxed.contains("╭─ Info"));
        assert!(boxed.contains("Hello"));
        assert!(boxed.lines().any(|line| line.starts_with('│')));
        assert!(boxed.lines().any(|line| line.starts_with('╰')));
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
