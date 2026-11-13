//! Standard Markdown and Nole extensions rendered as styled terminal lines.

mod code;
mod diagram;
mod layout;
mod math;
mod table;
mod wrap;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::core::markup::{code_block_specs, expand_tabs, link_specs, CodeBlockSpec, LinkSpec};
use crate::core::model::LinkTarget;
use crate::tui::theme::Theme;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedMarkup {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<RenderedLink>,
    pub tags: Vec<RenderedTag>,
    pub images: Vec<ImagePlacement>,
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
    let document = crate::core::markup::parse(expanded.as_ref());
    let code_specs = code_block_specs(&crate::core::markup::parse(source));
    let mut rendered = layout::render(&document, width, theme);
    let specs = link_specs(&document);
    rendered.tags = locate_tags(&rendered.lines, &document, theme);
    rendered
        .links
        .extend(locate_links(&rendered.lines, &specs, theme.markdown_link));
    rendered.links.extend(decorate_code_blocks(
        &mut rendered.lines,
        &code_specs,
        theme,
    ));
    rendered
}

/// A terminal image region; pixel loading and drawing live in `tui::media`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImagePlacement {
    pub source: String,
    pub title: String,
    pub alt: String,
    pub row: usize,
    pub column: usize,
    pub width: usize,
    pub height: usize,
}

const COPY_BUTTON: &str = " Copy ";

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
                        && trailing_width >= button_width + code::PADDING_X
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
        let before = trailing_width - button_width - code::PADDING_X;
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
                Span::styled(" ".repeat(code::PADDING_X), trailing_style),
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

#[derive(Clone)]
struct RenderedCell {
    character: char,
    row: usize,
    column: usize,
    width: usize,
    clickable: bool,
}

fn locate_links(
    lines: &[Line<'_>],
    specs: &[LinkSpec],
    link_underline_color: Color,
) -> Vec<RenderedLink> {
    let cells = rendered_cells(lines, |style| {
        style.underline_color == Some(link_underline_color)
    })
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

fn locate_tags(
    lines: &[Line<'_>],
    events: &[crate::core::markup::SpannedEvent<'_>],
    theme: Theme,
) -> Vec<RenderedTag> {
    let cells: Vec<_> = rendered_cells(lines, |style| {
        style.fg == Some(theme.markdown_hashtag) && style.add_modifier.contains(Modifier::BOLD)
    })
    .into_iter()
    .filter(|cell| cell.clickable)
    .collect();
    let mut used = vec![false; cells.len()];
    let mut tags = Vec::new();
    for event in events {
        let crate::core::markup::Event::Hashtag(name) = &event.event else {
            continue;
        };
        let label: Vec<_> = format!("#{name}").chars().collect();
        if let Some(range) = find_cells(&cells, &label, &used) {
            used[range.clone()].fill(true);
            for segment in link_segments(&cells[range], &LinkTarget::WikiLink(String::new())) {
                tags.push(RenderedTag {
                    name: name.to_string(),
                    row: segment.row,
                    column: segment.column,
                    width: segment.width,
                });
            }
        }
    }
    tags
}

fn rendered_cells(lines: &[Line<'_>], selected: impl Fn(Style) -> bool) -> Vec<RenderedCell> {
    let mut cells = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let mut column = 0;
        for span in &line.spans {
            let clickable = selected(span.style);
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

/// Map a one-based source line to its first terminal row after terminal Markdown layout.
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
    use ratatui::style::Modifier;

    use super::*;

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
    fn task_items_use_one_checkbox_and_keep_nested_indentation() {
        let source = "- [ ] first\n- [x] second\n- regular\n  - [ ] nested\n\n- [ ] loose";
        let output = text(&to_lines_at_width(source, WIDTH));
        assert_eq!(
            output.lines().collect::<Vec<_>>(),
            [
                " ☐ first",
                " ☑ second",
                " • regular",
                "    ☐ nested",
                " ☐ loose"
            ]
        );
    }

    #[test]
    fn wrapped_code_keeps_padding_and_header_spacing() {
        let source = "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz";
        let rendered = render_at_width(&format!("```text\n{source}\n```"), 24);
        let action = rendered
            .links
            .iter()
            .find(|link| matches!(link.target, LinkTarget::CopyCode(_)))
            .expect("copy action");
        assert!(rendered.lines[action.row + 1].to_string().trim().is_empty());
        let body = &rendered.lines[action.row + 2..rendered.lines.len() - 1];
        assert!(body[0].to_string().starts_with("  1 │ "));
        let recovered: String = body
            .iter()
            .map(|line| {
                let row = line.to_string();
                assert_eq!(UnicodeWidthStr::width(row.as_str()), 24);
                assert!(row.starts_with("  ") && row.ends_with("  "));
                row.chars()
                    .skip(6)
                    .take(16)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        assert_eq!(recovered, source);
    }

    #[test]
    fn code_wrapping_keeps_graphemes_and_quote_padding() {
        let rendered = render_at_width("> ```text\n> 中文👩‍💻abcdef中文👩‍💻abcdef\n> ```", 20);
        for line in &rendered.lines {
            assert!(line.width() <= 20, "{line:?}");
        }
        let output = text(&rendered.lines);
        assert!(output.contains("👩‍💻"));
        assert!(output
            .lines()
            .filter(|line| !line.is_empty())
            .all(|line| line.starts_with("  ▌ ")));
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
        assert_eq!(action.column + action.width + code::PADDING_X, 48);
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
    fn bbcode_does_not_disable_standard_markdown_rendering() {
        let rendered = render_at_width("# Heading\n\n[box border=double]\nbody\n[/box]", 30);
        assert!(span_with(&rendered.lines, "Heading")
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert!(text(&rendered.lines).contains("[box border=double]"));
        assert!(text(&rendered.lines).contains("[/box]"));
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
                "mindmap\n  root((Nole))\n    核心\n      编辑器\n        Markdown 渲染\n        Mermaid 图表\n        Markdown 样式\n      文件管理\n        data 笔记\n        daily 日志\n        archives 归档\n    AI Agent\n      工具\n        文件读写\n        搜索\n        网络请求\n      记忆\n        MEMORY.md\n        每日笔记\n    主题\n      TOML 配置\n      自定义颜色\n",
                &["Nole", "核心", "编辑器", "Markdown 渲染", "文件管理", "AI Agent", "MEMORY.md", "主题", "自定义颜色"][..],
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
    fn bbcode_colors_remain_literal_text() {
        let source = "[color=#12abef]foreground[/color] [bg=196]background[/bg]";
        let lines = to_lines_at_width(source, WIDTH);
        assert_eq!(text(&lines), source);
        assert!(lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.style.bg.is_none()));
    }

    #[test]
    fn renders_hashtags_and_wikilinks() {
        let lines = to_lines_at_width("See #开发/日志 and [[项目计划]]", WIDTH);
        assert_eq!(
            span_with(&lines, "#开发/日志").style.fg,
            Some(Theme::default().markdown_hashtag)
        );
        let wikilink = span_with(&lines, "[[项目计划]]");
        assert_eq!(wikilink.style.fg, Some(Theme::default().markdown_wikilink));
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
        assert!(rendered.links.is_empty());
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
    fn table_keeps_short_columns_compact_and_long_text_on_one_row() {
        let description = "A longer description that fits with a compact label";
        let rendered = render_at_width(
            &format!("| 类别 | Description |\n|---|---|\n| 短 | {description} |"),
            64,
        );
        assert_eq!(rendered.lines.len(), 5);
        let row = rendered.lines[3].to_string();
        assert!(row.contains(description), "{row}");
        let cells: Vec<_> = row.split('│').collect();
        assert_eq!(UnicodeWidthStr::width(cells[1]), 6);
        assert!(UnicodeWidthStr::width(cells[2]) > 40);
    }

    #[test]
    fn tables_fit_narrow_windows_and_keep_wide_graphemes() {
        for width in [8, 12, 20, 32] {
            let rendered =
                render_at_width("| 字 | Emoji |\n|---|---|\n| 中文说明 | 👩‍💻👩‍💻👩‍💻 |", width);
            assert!(rendered.lines.iter().all(|line| line.width() <= width));
            assert!(text(&rendered.lines).contains("👩‍💻"));
        }
    }

    #[test]
    fn locates_markdown_and_wiki_link_cells() {
        let rendered = render_at_width(
            "[site](https://example.test) [[项目计划]] [mail](mailto:a@example.test)",
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
    fn table_semantics_follow_the_final_cell_layout() {
        let rendered =
            render_at_width("| Tag | File |\n|---|---|\n| #开发 | ![[report.pdf]] |", 50);
        let tag = rendered
            .tags
            .iter()
            .find(|tag| tag.name == "开发")
            .expect("table tag");
        let link = rendered
            .links
            .iter()
            .find(|link| link.target == LinkTarget::LocalFile("report.pdf".into()))
            .expect("table file link");
        assert_eq!(tag.row, 3);
        assert_eq!(link.row, 3);
        assert_eq!(tag.column, 2);
        assert!(link.column > tag.column + tag.width);
        assert!(rendered.lines[tag.row].to_string().contains("#开发"));
    }

    #[test]
    fn link_locations_ignore_identical_underlined_non_link_text() {
        let rendered = render_at_width("# site\n\n[site](https://example.test)", WIDTH);
        assert_eq!(rendered.links.len(), 1);
        assert_eq!(rendered.links[0].row, 2);
    }

    #[test]
    fn bbcode_containers_remain_literal_text() {
        let source = "[box title=Info]\nHello\n[/box]\n\n[cols]Left[/cols]";
        assert_eq!(text(&to_lines_at_width(source, WIDTH)), source);
    }

    #[test]
    fn output_contains_no_terminal_escape_sequences() {
        let output = text(&to_lines_at_width("[red][b]safe[/b][/red]", WIDTH));
        assert_eq!(output, "[red][b]safe[/b][/red]");
        assert!(!output.contains('\x1b'));

        let linked = to_lines_at_width("**bold** [label](https://example.test)", WIDTH);
        assert_eq!(text(&linked), "bold label");
        assert!(span_with(&linked, "bold")
            .style
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn source_lines_map_to_terminal_rows() {
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
