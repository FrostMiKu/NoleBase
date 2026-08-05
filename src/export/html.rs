use std::path::Path;

use anyhow::{Context, Result};
use mbdown::{
    Alignment as TableAlignment, BorderMode, ColumnWidth, Container, ContainerEnd, Event,
    HeadingLevel, Node, WidthMode,
};

use crate::attachment::AttachmentStore;

use super::assets::{resolve_image, Assets};

/// Monospace stack for code, raw HTML, and math. CJK members keep Chinese
/// text inside code readable. Kept separate from [`FONT_STACK_CSS`] because
/// PDF font preflight resolves the body stack independently.
const CODE_FONT_STACK_CSS: &str = "\"ui-monospace\",\"SFMono-Regular\",\"Menlo\",\"Consolas\",\"Liberation Mono\",\"Courier New\",\"PingFang SC\",\"Noto Sans Mono CJK SC\",monospace";

/// Screen-first stylesheet. The body/code font stacks are appended by
/// [`render`] so [`FONT_STACK_CSS`] stays the single source of truth for PDF
/// font preflight. `@media print` resets layout for paper: full width, black
/// on white, and page-break avoidance for blocks that must stay intact.
const CSS: &str = r#"*{box-sizing:border-box}body{max-width:52rem;margin:2.5rem auto;padding:0 1.5rem;line-height:1.65;color:#1f2328;background:#fff;overflow-wrap:break-word}h1,h2,h3,h4,h5,h6{line-height:1.25;margin:1.5em 0 .5em;font-weight:400}h1{font-size:2em;padding-bottom:.3em;border-bottom:1px solid #d8dee4}h2{font-size:1.5em;padding-bottom:.25em;border-bottom:1px solid #eaeef2}h3{font-size:1.25em}h4{font-size:1.1em}h5{font-size:1em}h6{font-size:.875em;color:#57606a}p{margin:.6em 0}a{color:#0969da}code{background:#f6f8fa;border:1px solid #e4e7ea;border-radius:4px;padding:.12em .3em;font-size:.9em}pre{background:#f6f8fa;border:1px solid #e4e7ea;border-radius:6px;padding:12px;overflow-wrap:anywhere;white-space:pre-wrap;line-height:1.45}pre code{background:none;border:0;padding:0;font-size:1em}blockquote{border-left:4px solid #d0d7de;margin:.8em 0;padding:.2em 1em;color:#57606a}table{border-collapse:collapse;width:100%;margin:.8em 0}th,td{border:1px solid #d0d7de;padding:6px 10px;text-align:left}th{background:#f6f8fa}img{max-width:100%;height:auto}hr{border:0;border-top:1px solid #d0d7de;margin:1.5em 0}.nole-box{border:1px solid #999;padding:1ch;border-radius:4px}.nole-columns{display:flex;gap:2ch;flex-wrap:wrap}.nole-column{min-width:0}.nole-center{text-align:center}.nole-right{text-align:right}.nole-tag{font-weight:600;color:#57606a}.raw-html{white-space:pre-wrap}.image-placeholder{color:#6e7781;border:1px dashed #b6bdc4;border-radius:4px;padding:.15em .5em;font-style:italic}.task-marker{margin-right:.4em}.wiki-link{color:#57606a}.unresolved-link{color:#57606a}.mermaid-fallback{color:#6e7781}.footnote-ref{font-size:.75em}.footnote-definition{display:block;margin-top:.6em;font-size:.875em;color:#57606a}@media print{body{max-width:none;margin:0;padding:0;color:#000}a{color:#000}h1,h2{border-bottom-color:#bbb}pre,blockquote,table,.nole-box,.nole-columns{page-break-inside:avoid}img{max-width:100%}}"#;
pub(crate) const FONT_STACK_CSS: &str = "\"PingFang SC\",\"Hiragino Sans GB\",\"Microsoft YaHei\",\"Noto Sans CJK SC\",\"Noto Sans CJK JP\",\"Noto Sans\",\"Noto Sans Symbols 2\",\"Noto Emoji\",system-ui,sans-serif";

pub(crate) struct RenderedHtml {
    pub html: String,
    pub assets: Assets,
    /// Non-fatal rendering degradations (failed image embeds, unrenderable
    /// Mermaid), each carrying the offending target and a human-readable
    /// reason. Failures are surfaced here and inside the HTML itself
    /// (`data-diagnostic`/`data-target`/`data-reason` attributes plus visible
    /// placeholders); they never abort the whole export.
    pub diagnostics: Vec<RenderDiagnostic>,
}

/// A non-fatal export rendering degradation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderDiagnostic {
    pub kind: RenderDiagnosticKind,
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderDiagnosticKind {
    /// An image (reference or embed) could not be resolved, decoded, or embedded.
    Image,
    /// A `mermaid` code block could not be rendered to text.
    Mermaid,
}

pub(crate) fn render(
    source: &str,
    title: &str,
    root: &Path,
    note: &Path,
    attachments: &AttachmentStore,
) -> Result<RenderedHtml> {
    let document = mbdown::parse(source).context("parsing MBDown for export")?;
    let mut renderer = Renderer {
        root,
        note,
        attachments,
        body: String::new(),
        assets: Assets::default(),
        links: Vec::new(),
        headings: Vec::new(),
        image: None,
        inline_tags: Vec::new(),
        table: None,
        code: None,
        diagnostics: Vec::new(),
    };
    renderer.nodes(document.nodes())?;
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{CSS}code,pre,.raw-html,.math{{font-family:{CODE_FONT_STACK_CSS}}}body{{font-family:{FONT_STACK_CSS}}}</style></head><body>{}</body></html>",
        escape_text(title), renderer.body
    );
    Ok(RenderedHtml {
        html,
        assets: renderer.assets,
        diagnostics: renderer.diagnostics,
    })
}

struct PendingImage {
    target: String,
    alt: String,
}

enum InlineElement {
    Span,
    Link(LinkRender),
    Raw,
}

/// How a link target renders in the standalone export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkRender {
    /// Safe to emit as `href` (`#fragment`, `http(s)`, `mailto:`): offline-safe
    /// and portable in a self-contained file.
    Anchor,
    /// A local note path that cannot resolve in a standalone file: rendered as
    /// honest plain text inside a marker span instead of a leaking `file://`
    /// URL into the private Nole root.
    Local,
    /// Anything the export cannot vouch for (`javascript:`, `data:`, empty):
    /// inert plain text, no element, nothing executable.
    Plain,
}

struct InlineFrame {
    name: String,
    element: InlineElement,
}

struct TableState {
    alignments: Vec<TableAlignment>,
    column: usize,
    head: bool,
}

struct Renderer<'a> {
    root: &'a Path,
    note: &'a Path,
    attachments: &'a AttachmentStore,
    body: String,
    assets: Assets,
    links: Vec<LinkRender>,
    headings: Vec<u8>,
    image: Option<PendingImage>,
    inline_tags: Vec<InlineFrame>,
    table: Option<TableState>,
    code: Option<(bool, String)>,
    diagnostics: Vec<RenderDiagnostic>,
}

impl Renderer<'_> {
    fn nodes(&mut self, nodes: &[Node<'_>]) -> Result<()> {
        for node in nodes {
            match node {
                Node::Markdown(markdown) => self.events(markdown.events())?,
                Node::Box { spec, children } => {
                    let mut style = String::new();
                    if spec.border == BorderMode::None {
                        style.push_str("border:none;");
                    }
                    if let Some(color) = spec.border_color.as_deref().and_then(safe_color) {
                        style.push_str(&format!("border-color:{color};"));
                    }
                    if let Some(color) = spec.bg.as_deref().and_then(safe_color) {
                        style.push_str(&format!("background:{color};"));
                    }
                    match spec.width {
                        WidthMode::Full => style.push_str("width:100%;"),
                        WidthMode::Exact(width) => {
                            style.push_str(&format!("width:{width}ch;max-width:100%;"))
                        }
                        WidthMode::Fit => {}
                    }
                    style.push_str(&format!(
                        "padding:{}ch {}ch;",
                        spec.padding.y, spec.padding.x
                    ));
                    self.body
                        .push_str(&format!("<section class=\"nole-box\" style=\"{style}\">"));
                    if !spec.title.is_empty() {
                        self.body
                            .push_str(&format!("<strong>{}</strong>", escape_text(&spec.title)));
                    }
                    self.nodes(children)?;
                    self.body.push_str("</section>");
                }
                Node::Center { children } => {
                    self.wrapped("<div class=\"nole-center\">", "</div>", children)?
                }
                Node::Right { children } => {
                    self.wrapped("<div class=\"nole-right\">", "</div>", children)?
                }
                Node::Indent { spec, children } => self.wrapped(
                    &format!("<div style=\"margin-left:{}ch\">", spec.first),
                    "</div>",
                    children,
                )?,
                Node::Columns { spec, children } => {
                    let mut style = format!(
                        "gap:{}ch;padding:{}ch {}ch;",
                        spec.gap, spec.padding.y, spec.padding.x
                    );
                    if let Some(color) = spec.bg.as_deref().and_then(safe_color) {
                        style.push_str(&format!("background:{color};"));
                    }
                    self.wrapped(
                        &format!("<div class=\"nole-columns\" style=\"{style}\">"),
                        "</div>",
                        children,
                    )?;
                }
                Node::Column { spec, children } => {
                    let width = match spec.width {
                        ColumnWidth::Fixed(value) => format!("flex:0 0 {value}ch;"),
                        ColumnWidth::Flex(value) => format!("flex:{value} 1 0;"),
                    };
                    let mut style =
                        format!("{width}padding:{}ch {}ch;", spec.padding.y, spec.padding.x);
                    if let Some(color) = spec.bg.as_deref().and_then(safe_color) {
                        style.push_str(&format!("background:{color};"));
                    }
                    self.wrapped(
                        &format!("<div class=\"nole-column\" style=\"{style}\">"),
                        "</div>",
                        children,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn wrapped(&mut self, start: &str, end: &str, children: &[Node<'_>]) -> Result<()> {
        self.body.push_str(start);
        self.nodes(children)?;
        self.body.push_str(end);
        Ok(())
    }

    fn events(&mut self, events: &[mbdown::SpannedEvent<'_>]) -> Result<()> {
        for item in events {
            if self.image.is_some() {
                if matches!(item.event, Event::End(ContainerEnd::Image)) {
                    let pending = self.image.take().expect("image state exists");
                    let alt = if pending.alt.trim().is_empty() {
                        pending.target.clone()
                    } else {
                        pending.alt
                    };
                    self.image(&pending.target, false, &alt)?;
                } else if let Some(pending) = self.image.as_mut() {
                    append_image_alt(&mut pending.alt, &item.event);
                }
                continue;
            }
            if let Some((_mermaid, buffer)) = self.code.as_mut() {
                match &item.event {
                    Event::End(ContainerEnd::CodeBlock) => {
                        let (mermaid, source) = self.code.take().expect("code state exists");
                        let (rendered, rendered_mermaid) = if mermaid {
                            match mbdown::mermaid_text::render_with_width(&source, Some(100)) {
                                Ok(rendered) => (rendered, true),
                                Err(error) => {
                                    self.diagnostics.push(RenderDiagnostic {
                                        kind: RenderDiagnosticKind::Mermaid,
                                        target: source
                                            .lines()
                                            .map(str::trim)
                                            .find(|line| !line.is_empty())
                                            .unwrap_or("")
                                            .to_string(),
                                        reason: error.to_string(),
                                    });
                                    (source, false)
                                }
                            }
                        } else {
                            (source, false)
                        };
                        let class = if rendered_mermaid {
                            " class=\"mermaid-text\""
                        } else if mermaid {
                            " class=\"mermaid-fallback\""
                        } else {
                            ""
                        };
                        self.body.push_str(&format!(
                            "<pre{class}><code>{}</code></pre>",
                            escape_text(&rendered)
                        ));
                    }
                    Event::Text(text) | Event::Code(text) => buffer.push_str(text),
                    Event::SoftBreak | Event::HardBreak => buffer.push('\n'),
                    _ => {}
                }
                continue;
            }
            match &item.event {
                Event::Start(container) => self.start(container)?,
                Event::End(end) => self.end(*end),
                Event::Text(text) => self.text(text),
                Event::Hashtag(tag) => {
                    let text = format!("#{tag}");
                    self.body.push_str(&format!(
                        "<span class=\"nole-tag\">{}</span>",
                        escape_text(&text)
                    ));
                }
                Event::Embed(target) => self.image(target, true, target)?,
                Event::WikiLink(target) => {
                    let text = format!("[[{target}]]");
                    self.body.push_str(&format!(
                        "<span class=\"wiki-link\">{}</span>",
                        escape_text(&text)
                    ));
                }
                Event::InlineTag(tag) => self.inline_tag(tag),
                Event::Code(text) => {
                    self.body
                        .push_str(&format!("<code>{}</code>", escape_text(text)));
                }
                Event::Html(text) | Event::InlineHtml(text) => {
                    self.body.push_str(&format!(
                        "<span class=\"raw-html\">{}</span>",
                        escape_text(text)
                    ));
                }
                Event::FootnoteReference(name) => {
                    self.body.push_str(&format!(
                        "<sup class=\"footnote-ref\" id=\"fnref-{}\"><a href=\"#fn-{}\">[{}]</a></sup>",
                        escape_attr(name),
                        escape_attr(name),
                        escape_text(name)
                    ));
                }
                Event::SoftBreak => {
                    self.body.push('\n');
                }
                Event::HardBreak => {
                    self.body.push_str("<br>");
                }
                Event::Rule => self.body.push_str("<hr>"),
                Event::TaskListMarker(checked) => {
                    let marker = if *checked { '☑' } else { '☐' };
                    self.body
                        .push_str(&format!("<span class=\"task-marker\">{marker}</span>"));
                }
                Event::InlineMath(text) => {
                    self.body.push_str(&format!(
                        "<span class=\"math\" data-math=\"inline\">${}$</span>",
                        escape_text(text)
                    ));
                }
                Event::DisplayMath(text) => {
                    self.body.push_str(&format!(
                        "<pre class=\"math\" data-math=\"display\">$${}$$</pre>",
                        escape_text(text)
                    ));
                }
            }
        }
        Ok(())
    }

    fn start(&mut self, container: &Container<'_>) -> Result<()> {
        match container {
            Container::Paragraph => self.body.push_str("<p>"),
            Container::Heading(level) => {
                let level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                self.headings.push(level);
                self.body.push_str(&format!("<h{level}>"));
            }
            Container::BlockQuote => self.body.push_str("<blockquote>"),
            Container::CodeBlock(info) => {
                self.code = Some((
                    info.as_deref()
                        .is_some_and(|value| value.split_whitespace().next() == Some("mermaid")),
                    String::new(),
                ))
            }
            Container::HtmlBlock => self.body.push_str("<div class=\"raw-html\">"),
            Container::List(first) => self.body.push_str(&first.map_or_else(
                || "<ul>".to_string(),
                |value| format!("<ol start=\"{value}\">"),
            )),
            Container::Item => self.body.push_str("<li>"),
            Container::FootnoteDefinition(name) => self.body.push_str(&format!(
                "<aside class=\"footnote-definition\" id=\"fn-{}\" data-footnote=\"{}\">",
                escape_attr(name),
                escape_attr(name)
            )),
            Container::DefinitionList => self.body.push_str("<dl>"),
            Container::DefinitionListTitle => self.body.push_str("<dt>"),
            Container::DefinitionListDefinition => self.body.push_str("<dd>"),
            Container::Table(alignments) => {
                self.table = Some(TableState {
                    alignments: alignments.clone(),
                    column: 0,
                    head: false,
                });
                self.body.push_str("<table>");
            }
            Container::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.head = true;
                }
                self.body.push_str("<thead>");
            }
            Container::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.column = 0;
                }
                self.body.push_str("<tr>");
            }
            Container::TableCell => {
                let (head, alignment) =
                    self.table
                        .as_mut()
                        .map_or((false, TableAlignment::None), |table| {
                            let alignment = table
                                .alignments
                                .get(table.column)
                                .copied()
                                .unwrap_or(TableAlignment::None);
                            table.column = table.column.saturating_add(1);
                            (table.head, alignment)
                        });
                let tag = if head { "th" } else { "td" };
                let alignment = match alignment {
                    TableAlignment::None => "",
                    TableAlignment::Left => " style=\"text-align:left\"",
                    TableAlignment::Center => " style=\"text-align:center\"",
                    TableAlignment::Right => " style=\"text-align:right\"",
                };
                self.body.push_str(&format!("<{tag}{alignment}>"));
            }
            Container::Emphasis => self.body.push_str("<em>"),
            Container::Strong => self.body.push_str("<strong>"),
            Container::Strikethrough => self.body.push_str("<s>"),
            Container::Superscript => self.body.push_str("<sup>"),
            Container::Subscript => self.body.push_str("<sub>"),
            Container::Link { target, title } => {
                let render = link_render(target);
                match render {
                    LinkRender::Anchor => self.body.push_str(&format!(
                        "<a href=\"{}\" title=\"{}\" rel=\"noopener noreferrer\">",
                        escape_attr(target),
                        escape_attr(title)
                    )),
                    LinkRender::Local => self.body.push_str(
                        "<span class=\"unresolved-link\" title=\"local note link is not available in the exported file\">",
                    ),
                    LinkRender::Plain => {}
                }
                self.links.push(render);
            }
            Container::Image { target, .. } => {
                self.image = Some(PendingImage {
                    target: target.to_string(),
                    alt: String::new(),
                });
            }
            Container::MetadataBlock => self.body.push_str("<section class=\"metadata\">"),
        }
        Ok(())
    }

    fn end(&mut self, end: ContainerEnd) {
        let heading_end;
        let tag = match end {
            ContainerEnd::Paragraph => "</p>",
            ContainerEnd::Heading => {
                heading_end = format!("</h{}>", self.headings.pop().unwrap_or(1));
                &heading_end
            }
            ContainerEnd::BlockQuote => "</blockquote>",
            ContainerEnd::CodeBlock => "",
            ContainerEnd::HtmlBlock => "</div>",
            ContainerEnd::List(ordered) => {
                if ordered {
                    "</ol>"
                } else {
                    "</ul>"
                }
            }
            ContainerEnd::Item => "</li>",
            ContainerEnd::FootnoteDefinition => "</aside>",
            ContainerEnd::DefinitionList => "</dl>",
            ContainerEnd::DefinitionListTitle => "</dt>",
            ContainerEnd::DefinitionListDefinition => "</dd>",
            ContainerEnd::Table => {
                self.table = None;
                "</table>"
            }
            ContainerEnd::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.head = false;
                }
                "</thead>"
            }
            ContainerEnd::TableRow => "</tr>",
            ContainerEnd::TableCell => {
                if self.table.as_ref().is_some_and(|table| table.head) {
                    "</th>"
                } else {
                    "</td>"
                }
            }
            ContainerEnd::Emphasis => "</em>",
            ContainerEnd::Strong => "</strong>",
            ContainerEnd::Strikethrough => "</s>",
            ContainerEnd::Superscript => "</sup>",
            ContainerEnd::Subscript => "</sub>",
            ContainerEnd::Link => match self.links.pop().unwrap_or(LinkRender::Plain) {
                LinkRender::Anchor => "</a>",
                LinkRender::Local => "</span>",
                LinkRender::Plain => "",
            },
            ContainerEnd::Image => "",
            ContainerEnd::MetadataBlock => "</section>",
        };
        self.body.push_str(tag);
    }

    fn image(&mut self, target: &str, embed: bool, alt: &str) -> Result<()> {
        let label = if alt.trim().is_empty() { target } else { alt };
        if target.starts_with("http://") || target.starts_with("https://") {
            self.body.push_str(&format!(
                "<a href=\"{}\" rel=\"noopener noreferrer\">{}</a>",
                escape_attr(target),
                escape_text(label)
            ));
            return Ok(());
        }
        match resolve_image(self.root, self.note, target, self.attachments) {
            Ok((bytes, mime)) => {
                let key = self.assets.insert(bytes, mime)?;
                self.body.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\">",
                    escape_attr(&key),
                    escape_attr(label)
                ));
            }
            Err(error) => {
                let kind = if embed { "Embed" } else { "Image" };
                let reason = format!("{error:#}");
                self.body.push_str(&format!(
                    "<span class=\"image-placeholder\" data-diagnostic=\"{kind}\" data-target=\"{}\" data-reason=\"{}\" title=\"{}\">{kind} {}</span>",
                    escape_attr(target),
                    escape_attr(&reason),
                    escape_attr(&reason),
                    escape_text(label)
                ));
                self.diagnostics.push(RenderDiagnostic {
                    kind: RenderDiagnosticKind::Image,
                    target: target.to_string(),
                    reason,
                });
            }
        }
        Ok(())
    }

    fn text(&mut self, text: &str) {
        self.body.push_str(&escape_text(text));
    }

    fn inline_tag(&mut self, tag: &mbdown::InlineTag<'_>) {
        if tag.closing {
            let Some(frame) = self.inline_tags.last() else {
                self.body.push_str(&escape_text(&tag.raw));
                return;
            };
            if frame.name != tag.name {
                self.body.push_str(&escape_text(&tag.raw));
                return;
            }
            let frame = self.inline_tags.pop().expect("inline frame exists");
            match frame.element {
                InlineElement::Span => self.body.push_str("</span>"),
                InlineElement::Link(render) => match render {
                    LinkRender::Anchor => self.body.push_str("</a>"),
                    LinkRender::Local => self.body.push_str("</span>"),
                    LinkRender::Plain => {}
                },
                InlineElement::Raw => {
                    self.body.push_str(&escape_text(&tag.raw));
                }
            }
            return;
        }

        if tag.name == "link" {
            let render = tag.value.as_deref().map_or(LinkRender::Plain, link_render);
            match render {
                LinkRender::Anchor => self.body.push_str(&format!(
                    "<a href=\"{}\" rel=\"noopener noreferrer\">",
                    escape_attr(tag.value.as_deref().expect("anchor has a target"))
                )),
                LinkRender::Local => self.body.push_str(
                    "<span class=\"unresolved-link\" title=\"local note link is not available in the exported file\">",
                ),
                LinkRender::Plain => {}
            }
            self.inline_tags.push(InlineFrame {
                name: tag.name.clone(),
                element: InlineElement::Link(render),
            });
            return;
        }

        let mut style = String::new();
        let known = match tag.name.as_str() {
            "b" => {
                style.push_str("font-weight:bold");
                true
            }
            "i" => {
                style.push_str("font-style:italic");
                true
            }
            "u" => {
                style.push_str("text-decoration:underline");
                true
            }
            "s" => {
                style.push_str("text-decoration:line-through");
                true
            }
            "dim" => {
                style.push_str("opacity:.65");
                true
            }
            "color" | "fg" => {
                if let Some(color) = tag.value.as_deref().and_then(safe_color) {
                    style.push_str(&format!("color:{color}"));
                }
                true
            }
            "bg" => {
                if let Some(color) = tag.value.as_deref().and_then(safe_color) {
                    style.push_str(&format!("background:{color}"));
                }
                true
            }
            name if tag.value.is_none() && safe_color(name).is_some() => {
                let color = safe_color(name).expect("checked safe color");
                style.push_str(&format!("color:{color}"));
                true
            }
            _ => false,
        };
        if known {
            self.body
                .push_str(&format!("<span style=\"{}\">", escape_attr(&style)));
            self.inline_tags.push(InlineFrame {
                name: tag.name.clone(),
                element: InlineElement::Span,
            });
        } else {
            self.body.push_str(&escape_text(&tag.raw));
            self.inline_tags.push(InlineFrame {
                name: tag.name.clone(),
                element: InlineElement::Raw,
            });
        }
    }
}
fn append_image_alt(alt: &mut String, event: &Event<'_>) {
    match event {
        Event::Text(text)
        | Event::Code(text)
        | Event::Html(text)
        | Event::InlineHtml(text)
        | Event::InlineMath(text)
        | Event::DisplayMath(text) => alt.push_str(text),
        Event::Hashtag(tag) => {
            alt.push('#');
            alt.push_str(tag);
        }
        Event::Embed(target) => alt.push_str(&format!("![[{target}]]")),
        Event::WikiLink(target) => alt.push_str(&format!("[[{target}]]")),
        Event::FootnoteReference(name) => alt.push_str(&format!("[{name}]")),
        Event::SoftBreak | Event::HardBreak => alt.push(' '),
        Event::Rule => alt.push('—'),
        Event::TaskListMarker(checked) => alt.push(if *checked { '☑' } else { '☐' }),
        Event::Start(_) | Event::End(_) | Event::InlineTag(_) => {}
    }
}

pub(crate) fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
pub(crate) fn escape_attr(value: &str) -> String {
    escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) fn safe_color(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    if let Some(digits) = normalized.strip_prefix('#') {
        if matches!(digits.len(), 3 | 6) && digits.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Some(normalized);
        }
    }
    let named = match normalized.as_str() {
        "black" => "#000000",
        "red" => "#800000",
        "green" => "#008000",
        "yellow" => "#808000",
        "blue" => "#000080",
        "magenta" => "#800080",
        "cyan" => "#008080",
        "white" => "#ffffff",
        "gray" | "grey" => "#808080",
        "bright-red" => "#ff0000",
        "bright-green" => "#00ff00",
        "bright-yellow" => "#ffff00",
        "bright-blue" => "#0000ff",
        "bright-magenta" => "#ff00ff",
        "bright-cyan" => "#00ffff",
        "bright-white" => "#ffffff",
        _ => return normalized.parse::<u8>().ok().map(indexed_color),
    };
    Some(named.to_string())
}

fn indexed_color(index: u8) -> String {
    const ANSI: [&str; 16] = [
        "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0",
        "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    match index {
        0..=15 => ANSI[usize::from(index)].to_string(),
        16..=231 => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let value = index - 16;
            let red = LEVELS[usize::from(value / 36)];
            let green = LEVELS[usize::from((value % 36) / 6)];
            let blue = LEVELS[usize::from(value % 6)];
            format!("#{red:02x}{green:02x}{blue:02x}")
        }
        232..=255 => {
            let level = 8_u8.saturating_add((index - 232).saturating_mul(10));
            format!("#{level:02x}{level:02x}{level:02x}")
        }
    }
}

/// Classifies a link target for the standalone export.
///
/// Only targets that are safe and portable in a self-contained file become
/// real anchors: `#fragment`s, `http(s)`, and `mailto:`. Local note paths are
/// rendered as honest plain text (marked with `.unresolved-link`) instead of
/// leaking `file://` URLs into the private Nole root, and anything carrying a
/// scheme the export cannot vouch for (`javascript:`, `data:`, …) stays inert
/// text so the export never executes scripted or remote content.
fn link_render(target: &str) -> LinkRender {
    if target.starts_with('#')
        || target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("mailto:")
    {
        LinkRender::Anchor
    } else if target.is_empty() || target.contains(':') {
        LinkRender::Plain
    } else {
        LinkRender::Local
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn renderer_fixture() -> (tempfile::TempDir, PathBuf, AttachmentStore) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("data/nested")).unwrap();
        let store = AttachmentStore::new(root.join("attachments"));
        store.ensure_layout().unwrap();
        (directory, root, store)
    }

    fn save_png(path: impl AsRef<Path>) {
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn escapes_inert_content() {
        assert_eq!(escape_text("<script>&"), "&lt;script&gt;&amp;");
        assert!(safe_color("expression(x)").is_none());
    }

    #[test]
    fn local_links_never_leak_file_urls() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "# Links\n\n[private note](../other.md) [web](https://example.test) [anchor](#top) [bad](javascript:alert(1))\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(!html.contains("file://"));
        assert!(!html.contains(root.to_string_lossy().as_ref()));
        assert!(html.contains("<span class=\"unresolved-link\""));
        assert!(html.contains(">private note</span>"));
        assert!(html.contains("href=\"https://example.test\""));
        assert!(html.contains("href=\"#top\""));
        assert!(!html.contains("href=\"javascript:"));
        assert!(!html.contains("<a href=\"../other.md\""));
        assert_eq!(html.matches("<a href=\"").count(), 2);
    }

    #[test]
    fn inline_link_tags_classify_the_same_way() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "[link=local note.md]local[/link] [link=https://example.test]web[/link] [link=javascript:alert(1)]plain[/link]\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(html.contains("<span class=\"unresolved-link\""));
        assert!(html.contains("href=\"https://example.test\""));
        assert!(!html.contains("href=\"javascript:"));
        assert!(!html.contains("file://"));
        assert!(!html.contains("[link="));
    }

    #[test]
    fn image_failures_produce_diagnostics_and_placeholders() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        std::fs::write(root.join("data/nested/broken.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        save_png(root.join("data/nested/ok.png"));
        let source = "![Missing alt](missing.png)\n\n![Broken alt](broken.png)\n\n![Ok](ok.png)\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        assert_eq!(rendered.diagnostics.len(), 2);
        assert!(rendered
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.kind == RenderDiagnosticKind::Image));
        assert_eq!(rendered.diagnostics[0].target, "missing.png");
        assert!(rendered.diagnostics[0].reason.contains("missing.png"));
        assert_eq!(rendered.diagnostics[1].target, "broken.png");
        assert!(rendered.diagnostics[1].reason.contains("broken.png"));
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(html.contains("data-diagnostic=\"Image\""));
        assert!(html.contains("data-target=\"missing.png\""));
        assert!(html.contains("data-target=\"broken.png\""));
        assert!(html.contains("data-reason=\""));
        assert!(html.contains("Image Missing alt"));
        assert!(html.contains("Image Broken alt"));
        assert_eq!(html.matches("src=\"data:image/png;base64,").count(), 1);
        assert!(html.contains("alt=\"Ok\""));
    }

    #[test]
    fn mermaid_failure_is_explicit_and_diagnosed() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "```mermaid\n\n```\n\n```mermaid\ngraph LR; A[Start] --> B[End]\n```\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        assert_eq!(rendered.diagnostics.len(), 1);
        assert_eq!(rendered.diagnostics[0].kind, RenderDiagnosticKind::Mermaid);
        assert!(rendered.diagnostics[0].reason.contains("empty"));
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(html.contains("class=\"mermaid-fallback\""));
        assert!(html.contains("class=\"mermaid-text\""));
        assert!(html.contains("Start"));
        assert!(html.contains("End"));
    }

    #[test]
    fn math_wikilinks_and_unsupported_footnotes_remain_detectable() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source =
            "A note [[target]] with $x^2$ here[^1].\n\n$$\\frac{1}{2}$$\n\n[^1]: The definition.\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(html.contains("<span class=\"wiki-link\">[[target]]</span>"));
        assert!(html.contains("<span class=\"math\" data-math=\"inline\">$x^2$</span>"));
        assert!(html.contains("<pre class=\"math\" data-math=\"display\">$$\\frac{1}{2}$$</pre>"));
        // MBDown currently leaves footnote syntax as ordinary visible text.
        assert!(html.contains("here[^1]"));
        assert!(html.contains("[^1]: The definition."));
    }

    #[test]
    fn code_uses_monospace_stack_and_export_has_screen_and_print_styles() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "# T\n\n`inline` and\n\n```rust\nfn main() {}\n```\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(html.contains("code,pre,.raw-html,.math{font-family:"));
        assert!(html.contains("\"ui-monospace\""));
        assert!(html.contains("<code>inline</code>"));
        assert!(html.contains("<pre><code>fn main() {}"), "html: {html}");
        assert!(html.contains("body{font-family:"));
        assert!(html.contains("@media print{"));
        assert!(html.contains("page-break-inside:avoid"));
        assert!(html.contains("default-src 'none'"));
        assert!(html.contains("<h1>T</h1>"));
    }

    #[test]
    fn remote_images_stay_offline_links_not_embeds() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "![remote](https://example.test/pic.png)\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(html.contains("<a href=\"https://example.test/pic.png\""));
        assert!(!html.contains("src=\"https://"));
        assert!(rendered.diagnostics.is_empty());
    }
}
