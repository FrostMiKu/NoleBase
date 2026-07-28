//! MBDown markup -> styled ratatui lines.
//!
//! MBDown owns the language and syntax tree. MBTUI owns Nole's Ratatui layout.

use mbdown::{Container, ContainerEnd, Event, InlineTag, Node};
use mbtui::Renderer;
use ratatui::style::Color;
use ratatui::text::{Line, Text};
use unicode_width::UnicodeWidthChar;
#[cfg(test)]
use unicode_width::UnicodeWidthStr;

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
    match mbdown::parse(source) {
        Ok(document) => {
            let rendered = Renderer::with_theme(width.max(1), theme.markdown_theme())
                .with_image_height(12)
                .render(&document);
            let lines = rendered.text.lines;
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
                        links.extend(semantic.regions.into_iter().map(|region| RenderedLink {
                            target: LinkTarget::EmbeddedFile(target.clone().into()),
                            row: region.row,
                            column: region.column,
                            width: region.width,
                        }));
                    }
                    _ => {}
                }
            }
            RenderedMarkup {
                lines,
                links,
                tags,
                images: rendered.images,
            }
        }
        Err(error) => RenderedMarkup {
            lines: Text::raw(format!("MBDown parse error: {error}")).lines,
            links: Vec::new(),
            tags: Vec::new(),
            images: Vec::new(),
        },
    }
}

struct LinkSpec {
    label: String,
    target: LinkTarget,
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
                    target: LinkTarget::External(target.to_string()),
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
                    target: LinkTarget::External(target.clone()),
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
pub fn rendered_row_for_source_line(
    source: &str,
    line_no: usize,
    width: usize,
    theme: Theme,
) -> usize {
    let rendered = to_lines_at_width(source, width, theme);
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
        assert!(file.links.iter().any(|link| {
            link.target == LinkTarget::EmbeddedFile("attachments/report.pdf".into())
        }));
        assert!(text(&file.lines).contains("![[attachments/report.pdf]]"));
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
