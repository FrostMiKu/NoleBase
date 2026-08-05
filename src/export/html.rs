use std::path::Path;

use anyhow::{Context, Result};
use base64::Engine;
use mbdown::{
    Alignment as TableAlignment, BorderMode, ColumnWidth, Container, ContainerEnd, Event,
    HeadingLevel, Node, WidthMode,
};
use sha2::{Digest, Sha256};

use crate::attachment::AttachmentStore;

use super::assets::{resolve_image, Assets};
use super::highlight;
use super::katex;

/// Monospace stack for code, raw HTML, and math. CJK members keep Chinese
/// text inside code readable. Kept separate from [`FONT_STACK_CSS`] because
/// PDF font preflight resolves the body stack independently.
const CODE_FONT_STACK_CSS: &str = "\"ui-monospace\",\"SFMono-Regular\",\"Menlo\",\"Consolas\",\"Liberation Mono\",\"Courier New\",\"PingFang SC\",\"Noto Sans Mono CJK SC\",monospace";

/// Screen-first stylesheet. The body/code font stacks are appended by
/// [`render`] so [`FONT_STACK_CSS`] stays the single source of truth for PDF
/// font preflight. `@media print` resets layout for paper: full width, black
/// on white, and page-break avoidance for blocks that must stay intact.
const CSS: &str = r#"*{box-sizing:border-box}:root{color-scheme:light;--fg:#1f2328;--bg:#fff;--bg-subtle:#f6f8fa;--border:#d0d7de;--muted:#57606a;--on-dark:#fff;--on-dark-muted:#fff;--link:#0969da;--link-on-dark:#fff;--on-light:#000;--on-light-muted:#000;--link-on-light:#000;--text:var(--fg);--text-muted:var(--muted);--text-link:var(--link)}body{max-width:52rem;margin:2.5rem auto;padding:0 1.5rem;line-height:1.65;color:var(--text);background:var(--bg);overflow-wrap:break-word}h1,h2,h3,h4,h5,h6{line-height:1.25;margin:1.5em 0 .5em;font-weight:400}h1{font-size:2em;padding-bottom:.3em;border-bottom:1px solid #d8dee4}h2{font-size:1.5em;padding-bottom:.25em;border-bottom:1px solid #eaeef2}h3{font-size:1.25em}h4{font-size:1.1em}h5{font-size:1em}h6{font-size:.875em;color:var(--text-muted)}p{margin:.6em 0}a{color:var(--text-link)}code{background:var(--bg-subtle);border:1px solid #e4e7ea;border-radius:4px;padding:.12em .3em;font-size:.9em;color:var(--fg)}pre{background:var(--bg-subtle);border:1px solid #e4e7ea;border-radius:6px;padding:12px;overflow-wrap:anywhere;white-space:pre-wrap;line-height:1.45;color:var(--fg)}pre code{background:none;border:0;padding:0;font-size:1em}blockquote{border-left:4px solid var(--border);margin:.8em 0;padding:.2em 1em;color:var(--text-muted)}table{border-collapse:collapse;width:100%;margin:.8em 0}th,td{border:1px solid var(--border);padding:6px 10px;text-align:left}th{background:var(--bg-subtle);color:var(--fg)}img{max-width:100%;height:auto}hr{border:0;border-top:1px solid var(--border);margin:1.5em 0}.nole-box{border:1px solid #999;padding:1ch;border-radius:4px}.nole-columns{display:flex;gap:2ch;flex-wrap:wrap}.nole-column{min-width:0}.nole-center{text-align:center}.nole-right{text-align:right}.nole-tag{font-weight:600;color:var(--text-muted)}.raw-html{white-space:pre-wrap}.image-placeholder{color:var(--text-muted);border:1px dashed #b6bdc4;border-radius:4px;padding:.15em .5em;font-style:italic}.task-marker{margin-right:.4em}.wiki-link{color:var(--text-muted)}.unresolved-link{color:var(--text-muted)}.mermaid{background:var(--bg-subtle);border:1px solid var(--border);border-radius:6px;padding:12px;color:var(--fg)}.footnote-ref{font-size:.75em}.footnote-definition{display:block;margin-top:.6em;font-size:.875em;color:var(--text-muted)}.nole-bg-dark{--text:var(--on-dark);--text-muted:var(--on-dark-muted);--text-link:var(--link-on-dark);color:var(--text)}.nole-bg-light{--text:var(--on-light);--text-muted:var(--on-light-muted);--text-link:var(--link-on-light);color:var(--text)}.nole-bg-dark a,.nole-bg-light a{text-decoration:underline}@media print{body{max-width:none;margin:0;padding:0;color:#000}a{color:#000}h1,h2{border-bottom-color:#bbb}pre,blockquote,table,.nole-box,.nole-columns{page-break-inside:avoid}img{max-width:100%}.nole-bg-dark,.nole-bg-dark *{color:#000!important;background:none!important}}"#;
pub(crate) const FONT_STACK_CSS: &str = "\"PingFang SC\",\"Hiragino Sans GB\",\"Microsoft YaHei\",\"Noto Sans CJK SC\",\"Noto Sans CJK JP\",\"Noto Sans\",\"Noto Sans Symbols 2\",\"Noto Emoji\",system-ui,sans-serif";

/// Fixed-version Mermaid browser runtime, vendored under `assets/mermaid/` and
/// inlined into exports that contain fenced `mermaid` blocks so the standalone
/// document stays fully offline. MIT-licensed; the header comment in the
/// vendored file is the attribution notice. The file is minified UMD: it
/// defines `window.mermaid` and contains no `</script` sequence, so it can be
/// inlined verbatim without breaking the surrounding HTML.
const MERMAID_RUNTIME_JS: &str = include_str!("../../assets/mermaid/mermaid.min.js");
const MERMAID_LICENSE: &str = include_str!("../../assets/mermaid/LICENSE");

/// Runs after the runtime, initializing Mermaid with strict sanitization and
/// rendering every `.mermaid` container. The escaped source stays in the
/// container until rendering replaces it; if the runtime is missing, a diagram
/// fails, or anything else throws, the raw source text is restored so no
/// content is ever lost. Only containers we emitted are touched.
const MERMAID_INIT_JS: &str = r#"(function(){var nodes=document.querySelectorAll('.mermaid');if(nodes.length===0)return;var sources=[];for(var i=0;i<nodes.length;i++)sources.push(nodes[i].textContent);function restore(){for(var i=0;i<nodes.length;i++){if(!nodes[i].querySelector('svg')||nodes[i].querySelector('svg[aria-roledescription="error"]')){nodes[i].textContent=sources[i];nodes[i].removeAttribute('data-processed');}}}try{mermaid.initialize({startOnLoad:false,securityLevel:'strict'});mermaid.run({nodes:Array.prototype.slice.call(nodes)}).then(restore,restore);}catch(error){restore();}})();"#;

/// An inline runtime payload for the standalone export.
struct InlinePayload {
    /// Exact bytes that appear between `<script>`/`</script>` (or
    /// `<style>`/`</style>`) tags. Script payloads MUST NOT contain the
    /// literal sequence `</script`.
    content: &'static str,
    /// `true` → emitted inside `<head>`, `false` → emitted at the end of
    /// `<body>`.
    in_head: bool,
    /// `true` → a `<style>` payload (covered by `style-src 'unsafe-inline'`),
    /// `false` → a `<script>` payload hashed into the CSP.
    is_style: bool,
}

/// Composes the CSP `script-src` directive and the ordered inline payload
/// tags.
///
/// Every script payload is SHA-256-hashed (base64) into the CSP so scripts
/// stay controlled without relaxing to `'unsafe-inline'`. Style payloads are
/// emitted as-is. Returns `(script_src_directive, head_html, body_html)`;
/// the `script-src` directive is empty when there are no scripts, in which
/// case the caller omits it from the policy entirely.
fn assemble_runtime(payloads: &[InlinePayload]) -> (String, String, String) {
    let mut script_src = String::new();
    let mut head = String::new();
    let mut body = String::new();
    for payload in payloads {
        let tag = if payload.is_style {
            format!("<style>{}</style>", payload.content)
        } else {
            let digest = Sha256::digest(payload.content.as_bytes());
            let hash = format!(
                "'sha256-{}'",
                base64::engine::general_purpose::STANDARD.encode(digest)
            );
            if script_src.is_empty() {
                // The directive name must precede the first hash; without it
                // the hashes parse as an unknown directive and `default-src
                // 'none'` would block every inline script.
                script_src.push_str("script-src ");
            } else {
                script_src.push(' ');
            }
            script_src.push_str(&hash);
            format!("<script>{}</script>", payload.content)
        };
        if payload.in_head {
            head.push_str(&tag);
        } else {
            body.push_str(&tag);
        }
    }
    (script_src, head, body)
}

pub(crate) struct RenderedHtml {
    pub html: String,
    pub assets: Assets,
    /// Non-fatal rendering degradations (failed image embeds), each carrying
    /// the offending target and a human-readable reason. Failures are
    /// surfaced here and inside the HTML itself
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
        has_math: false,
        has_mermaid: false,
        has_highlight: false,
    };
    renderer.nodes(document.nodes())?;
    let mut license_notices = String::new();
    if renderer.has_math {
        license_notices.push_str("<!-- KaTeX license\n");
        license_notices.push_str(katex::KATEX_LICENSE);
        license_notices.push_str("\n-->\n");
    }
    if renderer.has_mermaid {
        license_notices.push_str("<!-- Mermaid license\n");
        license_notices.push_str(MERMAID_LICENSE);
        license_notices.push_str("\n-->\n");
    }
    if renderer.has_highlight {
        license_notices.push_str("<!-- highlight.js license\n");
        license_notices.push_str(highlight::HIGHLIGHT_LICENSE);
        license_notices.push_str("\n-->\n");
    }
    let mut payloads: Vec<InlinePayload> = Vec::new();
    if renderer.has_math {
        // KaTeX runtime: stylesheet (every font embedded as a data URI) in the
        // head, engine and bootstrap at the end of the body, in dependency
        // order. The bootstrap reads the raw TeX source from each container.
        payloads.push(InlinePayload {
            content: katex::embedded_css(),
            in_head: true,
            is_style: true,
        });
        payloads.push(InlinePayload {
            content: katex::KATEX_JS,
            in_head: false,
            is_style: false,
        });
        payloads.push(InlinePayload {
            content: katex::KATEX_INIT_JS,
            in_head: false,
            is_style: false,
        });
    }
    if renderer.has_mermaid {
        payloads.push(InlinePayload {
            content: MERMAID_RUNTIME_JS,
            in_head: false,
            is_style: false,
        });
        payloads.push(InlinePayload {
            content: MERMAID_INIT_JS,
            in_head: false,
            is_style: false,
        });
    }
    if renderer.has_highlight {
        // highlight.js runtime: theme stylesheet in the head, engine and
        // bootstrap at the end of the body, in dependency order. The
        // bootstrap highlights every `pre code[class^="language-"]` whose
        // language the pinned build knows; unknown and missing languages
        // keep their plain escaped source.
        payloads.push(InlinePayload {
            content: highlight::HIGHLIGHT_THEME_CSS,
            in_head: true,
            is_style: true,
        });
        payloads.push(InlinePayload {
            content: highlight::HIGHLIGHT_JS,
            in_head: false,
            is_style: false,
        });
        payloads.push(InlinePayload {
            content: highlight::HIGHLIGHT_INIT_JS,
            in_head: false,
            is_style: false,
        });
    }
    let (script_src, head_runtime, body_runtime) = assemble_runtime(&payloads);
    let csp = if script_src.is_empty() {
        // No inline scripts: `default-src 'none'` already keeps scripts inert.
        "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:".to_string()
    } else {
        // Inline scripts are pinned by SHA-256 hashes, so `script-src` needs
        // no `'unsafe-inline'`.
        format!("default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; {script_src}")
    };
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{CSS}code,pre,.raw-html,.math{{font-family:{CODE_FONT_STACK_CSS}}}body{{font-family:{FONT_STACK_CSS}}}</style>{license_notices}{head_runtime}</head><body>{}{body_runtime}</body></html>",
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

/// A fenced code block being collected: whether it is a Mermaid diagram, the
/// validated highlight language token (`None` for Mermaid and for info
/// strings without a safe token), and the buffered source text.
struct CodeBlock {
    mermaid: bool,
    language: Option<String>,
    source: String,
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
    code: Option<CodeBlock>,
    diagnostics: Vec<RenderDiagnostic>,
    has_math: bool,
    has_mermaid: bool,
    has_highlight: bool,
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
                    let bg = spec.bg.as_deref().and_then(safe_color);
                    if let Some(color) = &bg {
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
                    let class = match bg {
                        Some(color) => format!("nole-box {}", bg_tone_class(&color)),
                        None => "nole-box".to_string(),
                    };
                    self.body
                        .push_str(&format!("<section class=\"{class}\" style=\"{style}\">"));
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
                    let bg = spec.bg.as_deref().and_then(safe_color);
                    if let Some(color) = &bg {
                        style.push_str(&format!("background:{color};"));
                    }
                    let class = match bg {
                        Some(color) => format!("nole-columns {}", bg_tone_class(&color)),
                        None => "nole-columns".to_string(),
                    };
                    self.wrapped(
                        &format!("<div class=\"{class}\" style=\"{style}\">"),
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
                    let bg = spec.bg.as_deref().and_then(safe_color);
                    if let Some(color) = &bg {
                        style.push_str(&format!("background:{color};"));
                    }
                    let class = match bg {
                        Some(color) => format!("nole-column {}", bg_tone_class(&color)),
                        None => "nole-column".to_string(),
                    };
                    self.wrapped(
                        &format!("<div class=\"{class}\" style=\"{style}\">"),
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
            if let Some(block) = self.code.as_mut() {
                match &item.event {
                    Event::End(ContainerEnd::CodeBlock) => {
                        let block = self.code.take().expect("code state exists");
                        if block.mermaid {
                            // Browser-rendered via the inlined Mermaid runtime:
                            // the escaped source is the container body, so it
                            // stays visible when scripting is unavailable or a
                            // diagram fails, and it can never break out of the
                            // element or execute.
                            self.has_mermaid = true;
                            self.body.push_str(&format!(
                                "<pre class=\"mermaid\">{}</pre>",
                                escape_text(&block.source)
                            ));
                        } else {
                            // Highlighted in the browser by the inlined
                            // highlight.js runtime when the info string
                            // carried a safe language token; otherwise a plain
                            // block. The source is always escaped server-side,
                            // so it stays visible and inert when scripting is
                            // unavailable.
                            self.has_highlight = true;
                            match &block.language {
                                Some(language) => self.body.push_str(&format!(
                                    "<pre><code class=\"language-{language}\">{}</code></pre>",
                                    escape_text(&block.source)
                                )),
                                None => self.body.push_str(&format!(
                                    "<pre><code>{}</code></pre>",
                                    escape_text(&block.source)
                                )),
                            }
                        }
                    }
                    Event::Text(text) | Event::Code(text) => block.source.push_str(text),
                    Event::SoftBreak | Event::HardBreak => block.source.push('\n'),
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
                    // The container body is the raw TeX source (mbdown strips
                    // the `$…$` delimiters), HTML-escaped: KaTeX reads it
                    // verbatim, and it stays visible as a fallback whenever
                    // the inline runtime is unavailable.
                    self.has_math = true;
                    self.body.push_str(&format!(
                        "<span class=\"math\" data-math=\"inline\">{}</span>",
                        escape_text(text)
                    ));
                }
                Event::DisplayMath(text) => {
                    self.has_math = true;
                    self.body.push_str(&format!(
                        "<div class=\"math\" data-math=\"display\">{}</div>",
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
                let info = info.as_deref().unwrap_or("");
                let mermaid = info.split_whitespace().next() == Some("mermaid");
                // Mermaid blocks never take a highlight language: they are
                // rendered by the Mermaid runtime under `class="mermaid"`.
                let language = if mermaid {
                    None
                } else {
                    highlight::language_token(info).map(str::to_owned)
                };
                self.code = Some(CodeBlock {
                    mermaid,
                    language,
                    source: String::new(),
                })
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

        // A marker is only honored when its exact syntax is supported: the
        // value-less style tags must not carry a value, and the color tags
        // only accept values `safe_color` can honor. Anything else (unknown
        // names, invalid or empty color values, stray attributes) falls back
        // to the escaped source marker below so no export input silently
        // drops text or turns into an empty element.
        let mut style = String::new();
        let mut tone_class = "";
        let known = match tag.name.as_str() {
            "b" if tag.value.is_none() => {
                style.push_str("font-weight:bold");
                true
            }
            "i" if tag.value.is_none() => {
                style.push_str("font-style:italic");
                true
            }
            "u" if tag.value.is_none() => {
                style.push_str("text-decoration:underline");
                true
            }
            "s" if tag.value.is_none() => {
                style.push_str("text-decoration:line-through");
                true
            }
            "dim" if tag.value.is_none() => {
                style.push_str("opacity:.65");
                true
            }
            "color" | "fg" => match tag.value.as_deref().and_then(safe_color) {
                Some(color) => {
                    style.push_str(&format!("color:{color}"));
                    true
                }
                None => false,
            },
            "bg" => match tag.value.as_deref().and_then(safe_color) {
                Some(color) => {
                    style.push_str(&format!("background:{color}"));
                    tone_class = bg_tone_class(&color);
                    true
                }
                None => false,
            },
            name if tag.value.is_none() && safe_color(name).is_some() => {
                let color = safe_color(name).expect("checked safe color");
                style.push_str(&format!("color:{color}"));
                true
            }
            _ => false,
        };
        if known {
            let class = if tone_class.is_empty() {
                String::new()
            } else {
                format!(" class=\"{tone_class}\"")
            };
            self.body
                .push_str(&format!("<span{class} style=\"{}\">", escape_attr(&style)));
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

/// WCAG relative luminance of a normalized `#rgb`/`#rrggbb` color, 0.0
/// (black) to 1.0 (white). Callers pass the output of [`safe_color`], which
/// is always lowercase hex of length 3 or 6.
fn relative_luminance(hex: &str) -> f64 {
    let digits = hex.trim_start_matches('#');
    let bytes = digits.as_bytes();
    let nibble = |byte: u8| -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => 0,
        }
    };
    let channels: [u8; 3] = match bytes.len() {
        // `#rgb` expands to `#rrggbb`: each nibble n stands for n*16+n = n*17.
        3 => [
            nibble(bytes[0]) * 17,
            nibble(bytes[1]) * 17,
            nibble(bytes[2]) * 17,
        ],
        6 => [
            nibble(bytes[0]) * 16 + nibble(bytes[1]),
            nibble(bytes[2]) * 16 + nibble(bytes[3]),
            nibble(bytes[4]) * 16 + nibble(bytes[5]),
        ],
        _ => return 0.0,
    };
    let linear = |channel: u8| -> f64 {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(channels[0]) + 0.7152 * linear(channels[1]) + 0.0722 * linear(channels[2])
}

fn contrast_ratio(first: f64, second: f64) -> f64 {
    let lighter = first.max(second);
    let darker = first.min(second);
    (lighter + 0.05) / (darker + 0.05)
}

/// Tone class for an element carrying a user-supplied background color.
/// Compare pure black and white against the actual background and choose the
/// higher WCAG contrast ratio. One of those candidates always reaches at
/// least 4.58:1, including medium colors where a luminance cutoff fails.
/// Nested tone classes reset the inherited text/link/muted CSS variables.
fn bg_tone_class(hex: &str) -> &'static str {
    let background = relative_luminance(hex);
    let white_contrast = contrast_ratio(background, 1.0);
    let black_contrast = contrast_ratio(background, 0.0);
    if white_contrast > black_contrast {
        "nole-bg-dark"
    } else {
        "nole-bg-light"
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

    /// The exact bytes between each `<script>`/`</script>` pair. Escaped
    /// source can never match these literals, so this only sees real scripts.
    fn script_payloads(html: &str) -> Vec<&str> {
        let mut payloads = Vec::new();
        let mut rest = html;
        while let Some(start) = rest.find("<script>") {
            let after = &rest[start + "<script>".len()..];
            let Some(end) = after.find("</script>") else {
                break;
            };
            payloads.push(&after[..end]);
            rest = &after[end + "</script>".len()..];
        }
        payloads
    }

    /// The `content` attribute of the CSP meta tag.
    fn csp_value(html: &str) -> &str {
        let start = html
            .find("Content-Security-Policy")
            .expect("CSP meta present");
        let value_start =
            html[start..].find("content=\"").expect("CSP content") + start + "content=\"".len();
        let value_end = html[value_start..].find('"').expect("CSP closing quote") + value_start;
        &html[value_start..value_end]
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
    fn unsupported_inline_markers_render_as_visible_escaped_text() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "A [hl]highlighted[/hl] B [note=1]kept[/note] C [color=not-a-color]plain[/color] D [b=1]value[/b] E [ghost]unclosed tail\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        // Unknown opening markers and their matching closers stay visible.
        assert!(html.contains("[hl]"), "html: {html}");
        assert!(html.contains("[/hl]"));
        assert!(html.contains("[note=1]"));
        assert!(html.contains("[/note]"));
        // Known names with values the renderer cannot honor fall back too.
        assert!(html.contains("[color=not-a-color]"));
        assert!(html.contains("[/color]"));
        assert!(html.contains("[b=1]"));
        assert!(html.contains("[/b]"));
        // The inner content still renders, and an unclosed unknown marker
        // does not swallow what follows it.
        assert!(html.contains("highlighted"));
        assert!(html.contains("kept"));
        assert!(html.contains("plain"));
        assert!(html.contains("value"));
        assert!(html.contains("[ghost]"));
        assert!(html.contains("unclosed tail"));
        // No empty-style span is emitted for an unhonorable marker.
        assert!(!html.contains("<span style=\"\">"));
    }

    #[test]
    fn unsupported_marker_values_cannot_inject_script() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "[x=<script>alert(1)</script>]y[/x]\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        // Angle brackets survive only as entities; no executable markup.
        assert!(html.contains("&lt;script&gt;"), "html: {html}");
        assert!(!html.contains("<script>"));
        // The marker pieces and the content are preserved as plain text.
        assert!(html.contains("[x="));
        assert!(html.contains("]y[/x]"));
        assert!(html.contains("alert(1)"));
    }

    #[test]
    fn supported_inline_markers_keep_their_semantics() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "[b]bold[/b] [i]italic[/i] [u]under[/u] [s]strike[/s] [dim]dim[/dim] [red]name[/red] [color=#ff0000]hex[/color] [fg=bright-red]fg[/fg] [bg=blue]bg[/bg] [link=https://example.test]web[/link]\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(
            html.contains("style=\"font-weight:bold\">bold</span>"),
            "html: {html}"
        );
        assert!(html.contains("style=\"font-style:italic\">italic</span>"));
        assert!(html.contains("style=\"text-decoration:underline\">under</span>"));
        assert!(html.contains("style=\"text-decoration:line-through\">strike</span>"));
        assert!(html.contains("style=\"opacity:.65\">dim</span>"));
        assert!(html.contains("style=\"color:#800000\">name</span>"));
        assert!(html.contains("style=\"color:#ff0000\">hex</span>"));
        assert!(html.contains("style=\"color:#ff0000\">fg</span>"));
        assert!(html.contains("class=\"nole-bg-dark\" style=\"background:#000080\">bg</span>"));
        assert!(html.contains("href=\"https://example.test\""));
        assert!(html.contains(">web</a>"));
        // Supported markers are consumed, not echoed back as source text.
        assert!(!html.contains("[b]"));
        assert!(!html.contains("[red]"));
        assert!(!html.contains("[color="));
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
    fn mermaid_blocks_become_containers_with_inlined_runtime() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "```mermaid\ngraph LR; A[Start] --> B[End]\n```\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        // Server-side Mermaid diagnostics no longer exist: every block is
        // handed to the browser runtime as an escaped-source container.
        assert!(
            rendered.diagnostics.is_empty(),
            "{:?}",
            rendered.diagnostics
        );
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(html.contains("<pre class=\"mermaid\">graph LR; A[Start] --&gt; B[End]"));
        // Fixed-version runtime and init script are inlined (no CDN), and the
        // CSP pins them by SHA-256 instead of 'unsafe-inline'.
        assert!(html.contains("mermaid v10.9.3"));
        assert!(html.contains("mermaid.initialize({startOnLoad:false,securityLevel:'strict'})"));
        assert!(html.contains("mermaid.run({nodes:"));
        assert!(html.contains("svg[aria-roledescription=\"error\"]"));
        assert!(html.contains("removeAttribute('data-processed')"));
        assert!(html.contains("Copyright (c) 2014 - 2022 Knut Sveidqvist"));
        assert!(html.contains("script-src 'sha256-"));
        assert!(!html.contains("mermaid-text"));
        assert!(!html.contains("mermaid-fallback"));
        assert_eq!(script_payloads(&html).len(), 2);
    }

    #[test]
    fn csp_hashes_pin_every_inline_script() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "```mermaid\ngraph LR; A --> B\n```\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        let csp = csp_value(&html);
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("font-src data:"));
        // The hashes must sit under a real `script-src` directive: without the
        // keyword they parse as an unknown directive and `default-src 'none'`
        // blocks every inline script.
        assert_eq!(csp.matches("script-src").count(), 1, "csp: {csp}");
        assert!(csp.contains("script-src 'sha256-"), "csp: {csp}");
        let payloads = script_payloads(&html);
        assert!(!payloads.is_empty());
        for payload in payloads {
            let digest = Sha256::digest(payload.as_bytes());
            let hash = base64::engine::general_purpose::STANDARD.encode(digest);
            assert!(
                csp.contains(&format!("'sha256-{hash}'")),
                "inline script hash missing from CSP"
            );
        }
    }

    #[test]
    fn mermaid_source_cannot_break_out_of_container() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "```mermaid\nA[\"</pre><script>alert(1)</script><img src=x onerror=alert(2)>\"] --> B\n```\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        // The hostile source is escaped text inside the container, never
        // executable markup: no live closing tag, script, or handler.
        assert!(html.contains("&lt;/pre&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("&lt;img src=x onerror=alert(2)&gt;"));
        assert!(!html.contains("<script>alert"));
        assert_eq!(html.matches("<pre class=\"mermaid\">").count(), 1);
    }

    #[test]
    fn exports_without_mermaid_omit_runtime_and_script_src() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "# T\n\nPlain text with `inline` code, no fenced blocks.\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(script_payloads(&html).is_empty());
        assert!(!html.contains("script-src"));
        assert!(!html.contains("mermaid v10.9.3"));
        assert!(html.contains("<code>inline</code>"));
    }

    #[test]
    fn rust_fenced_code_gets_language_class_and_inlined_highlight_runtime() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "```rust\nfn main() {\n    let answer = 42;\n}\n```\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);

        // The first info token becomes a `language-*` class and the source
        // stays server-side escaped; token spans are added in the browser.
        assert!(
            html.contains("<pre><code class=\"language-rust\">fn main() {"),
            "html: {html}"
        );
        assert!(html.contains("let answer = 42;"));
        assert!(html.contains("</code></pre>"));

        // Pinned runtime, theme, and init are inlined (no CDN), and the full
        // third-party license notice is embedded next to the other notices.
        assert!(html.contains("Highlight.js v11.11.1"));
        assert!(html.contains("hljs.getLanguage("));
        assert!(html.contains("pre code[class^=\"language-\"]"));
        assert!(html.contains("<style>pre code.hljs{"));
        assert!(html.contains(".hljs-keyword"));
        assert!(html.contains("<!-- highlight.js license"));
        assert!(html.contains("BSD 3-Clause License"));
        assert!(html.contains("Copyright (c) 2006, Ivan Sagalaev."));
        assert!(!html.contains("https://cdn"));
        assert!(!html.contains("src=\"http"));

        // Exactly two inline scripts (engine + init); the CSP pins both by
        // SHA-256 and never falls back to 'unsafe-inline'.
        assert_eq!(script_payloads(&html).len(), 2, "engine + init");
        let csp = csp_value(&html);
        assert!(csp.contains("default-src 'none'"));
        assert_eq!(csp.matches("script-src").count(), 1, "csp: {csp}");
        let script_src = csp.split("script-src ").nth(1).unwrap();
        assert!(!script_src.contains("'unsafe-inline'"), "csp: {csp}");
        for payload in script_payloads(&html) {
            let digest = Sha256::digest(payload.as_bytes());
            let hash = base64::engine::general_purpose::STANDARD.encode(digest);
            assert!(
                csp.contains(&format!("'sha256-{hash}'")),
                "inline script hash missing from CSP"
            );
        }
    }

    #[test]
    fn unknown_and_missing_languages_stay_plain_escaped_code() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source =
            "```lolcode\nHAI\n```\n\n```\nplain block\n```\n\n```mermaid\ngraph LR; A --> B\n```\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        // A safe ASCII token still becomes a class (the bootstrap skips
        // languages the pinned build does not know); a missing info string
        // emits no class attribute at all.
        assert!(
            html.contains("<pre><code class=\"language-lolcode\">HAI"),
            "html: {html}"
        );
        assert!(html.contains("<pre><code>plain block"));
        assert!(!html.contains("class=\"language-\""));
        // Mermaid blocks keep their own container and never take a language.
        assert!(html.contains("<pre class=\"mermaid\">graph LR; A --&gt; B"));
    }

    #[test]
    fn malicious_info_strings_cannot_inject_attributes() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "```rust\" onmouseover=\"alert(1)\nfn main() {}\n```\n\n```javascript:alert(1)\nlet x = 1;\n```\n\n```<script>alert(1)</script>\npayload\n```\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        // No attribute, handler, or script can come out of an untrusted info
        // string: every block falls back to a plain `<pre><code>`.
        assert!(!html.contains("onmouseover"), "html: {html}");
        assert!(!html.contains("<script>alert"));
        assert!(!html.contains("class=\"language-"));
        assert!(html.contains("<pre><code>fn main() {}"));
        assert!(html.contains("<pre><code>let x = 1;"));
        assert!(html.contains("<pre><code>payload"));
    }

    #[test]
    fn exports_without_fenced_code_omit_the_highlight_runtime() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "# T\n\nJust text and `inline` code.\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(script_payloads(&html).is_empty());
        assert!(!html.contains("script-src"));
        assert!(!html.contains("highlight.js license"));
        assert!(!html.contains("hljs.getLanguage("));
        assert!(!html.contains(".hljs-keyword"));
        assert!(html.contains("<code>inline</code>"));
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
        assert!(html.contains("<span class=\"math\" data-math=\"inline\">x^2</span>"));
        assert!(html.contains("<div class=\"math\" data-math=\"display\">\\frac{1}{2}</div>"));
        // MBDown currently leaves footnote syntax as ordinary visible text.
        assert!(html.contains("here[^1]"));
        assert!(html.contains("[^1]: The definition."));
    }

    #[test]
    fn katex_runtime_renders_math_with_embedded_resources_and_strict_csp() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "Inline $x^2$ and hostile $<script>alert(1)</script>$.\n\n$$\\frac{1}{2}$$\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);

        // Source fallback: the containers hold the raw TeX source,
        // HTML-escaped, so formulas stay readable when scripting is disabled
        // and hostile source can never execute or break out of the element.
        assert!(html.contains("<span class=\"math\" data-math=\"inline\">x^2</span>"));
        assert!(html.contains("<div class=\"math\" data-math=\"display\">\\frac{1}{2}</div>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert"));

        // Initialization: engine plus bootstrap, in dependency order; the
        // bootstrap targets `.math[data-math]` with locked-down options.
        let payloads = script_payloads(&html);
        assert_eq!(payloads.len(), 2, "katex engine + bootstrap, no mermaid");
        assert!(payloads[0].contains("version:\"0.18.1\""));
        let bootstrap = payloads[1];
        assert!(bootstrap.contains("katex.render("));
        assert!(bootstrap.contains("document.querySelectorAll(\".math[data-math]\")"));
        assert!(bootstrap.contains("throwOnError: false"));
        assert!(bootstrap.contains("strict: \"warn\""));
        assert!(bootstrap.contains("trust: false"));
        assert!(bootstrap.contains("displayMode: display"));
        assert!(html.contains("Copyright (c) 2013-2020 Khan Academy"));

        // Resource embedding: no CDN or external references; every KaTeX font
        // is a data: URI inside the inlined stylesheet.
        assert!(html.contains("data:font/woff2;base64,"));
        assert!(!html.contains("url(fonts/"));
        assert!(!html.contains("https://cdn"));
        assert!(!html.contains("src=\"http"));

        // Strict CSP: default-src 'none', data: fonts, and script-src pins
        // every inline script by SHA-256 — never 'unsafe-inline'.
        let csp = csp_value(&html);
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("font-src data:"));
        let script_src = csp
            .split("script-src ")
            .nth(1)
            .expect("script-src directive present");
        assert!(!script_src.contains("'unsafe-inline'"));
        for payload in &payloads {
            let digest = Sha256::digest(payload.as_bytes());
            let hash = base64::engine::general_purpose::STANDARD.encode(digest);
            assert!(
                csp.contains(&format!("'sha256-{hash}'")),
                "inline script hash missing from CSP"
            );
        }
    }

    #[test]
    fn exports_without_math_omit_the_katex_runtime() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "# T\n\nPlain text, no math.\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        assert!(script_payloads(&html).is_empty());
        assert!(!html.contains("data:font/woff2;base64,"));
        assert!(!html.contains("script-src"));
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
        assert!(
            html.contains("<pre><code class=\"language-rust\">fn main() {}"),
            "html: {html}"
        );
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

    #[test]
    fn bg_tone_classification_maximizes_wcag_contrast() {
        for (background, expected) in [
            ("#00005f", "nole-bg-dark"),
            ("#ffffff", "nole-bg-light"),
            ("#000000", "nole-bg-dark"),
            ("#c0c0c0", "nole-bg-light"),
            ("#00a000", "nole-bg-light"),
            ("#ff0000", "nole-bg-light"),
        ] {
            let class = bg_tone_class(background);
            assert_eq!(class, expected, "background {background}");
            let foreground = if class == "nole-bg-dark" { 1.0 } else { 0.0 };
            assert!(
                contrast_ratio(relative_luminance(background), foreground) >= 4.5,
                "background {background}"
            );
        }
    }

    #[test]
    fn dark_backgrounds_get_light_default_text_and_keep_explicit_colors() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source =
            "[bg=17]dark[/bg] [bg=17][color=white]kept[/color][/bg]\n\n[box bg=17]boxed[/box]\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        // The 256-color index 17 maps to a dark blue (#00005f) background; the
        // rendered span must carry the tone class so CSS picks a light default
        // foreground instead of leaving browser-default black text on dark.
        assert!(
            html.contains("<span class=\"nole-bg-dark\" style=\"background:#00005f\">dark</span>"),
            "html: {html}"
        );
        assert!(html.contains(
            "<span class=\"nole-bg-dark\" style=\"background:#00005f\"><span style=\"color:#ffffff\">kept</span></span>"
        ));
        assert!(html.contains(
            "<section class=\"nole-box nole-bg-dark\" style=\"background:#00005f;padding:"
        ));
        // The dark default foreground is a light color, and explicit safe
        // foregrounds are preserved verbatim.
        assert!(html.contains("--on-dark:#fff"));
        assert!(html.contains("--link-on-dark:#fff"));
        assert!(html.contains("color:#ffffff"));
        assert!(html.contains(".nole-bg-dark{--text:var(--on-dark)"));
    }

    #[test]
    fn light_backgrounds_keep_dark_text_even_inside_dark_containers() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "[bg=17]a[bg=15][link=https://example.test]link[/link][/bg]c[/bg]\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        // White (index 15) inside a dark span must pin dark text again instead
        // of inheriting the light-on-dark color.
        assert!(html.contains(
            "<span class=\"nole-bg-light\" style=\"background:#ffffff\"><a href=\"https://example.test\""
        ));
        assert!(html.contains(">link</a></span>"));
        assert!(html.contains(".nole-bg-light{--text:var(--on-light);--text-muted:var(--on-light-muted);--text-link:var(--link-on-light);color:var(--text)}"));
    }

    #[test]
    fn export_declares_light_scheme_and_readable_surfaces() {
        let (_directory, root, store) = renderer_fixture();
        let note = root.join("data/nested/note.md");
        let source = "# T\n\n`inline`\n\n> quote\n\n| a |\n|---|\n| b |\n\n[bg=17]dark[/bg]\n";
        std::fs::write(&note, source).unwrap();
        let rendered = render(source, "note.md", &root, &note, &store).unwrap();
        let html = rendered.assets.materialize_data_uris(&rendered.html);
        // The page pins a light color scheme with explicit foreground and
        // background, so nothing falls back to browser defaults.
        assert!(html.contains("color-scheme:light"));
        assert!(html.contains("--fg:#1f2328"));
        assert!(html.contains("--bg:#fff"));
        assert!(html.contains("body{max-width:52rem;margin:2.5rem auto;padding:0 1.5rem;line-height:1.65;color:var(--text);background:var(--bg);overflow-wrap:break-word}"));
        // Surfaces that paint their own light background keep dark text even
        // when nested inside a dark container.
        assert!(html.contains("code{background:var(--bg-subtle);"));
        assert!(html.contains("color:var(--fg)}"));
        assert!(html.contains("th{background:var(--bg-subtle);color:var(--fg)}"));
        assert!(html.contains(".mermaid{background:var(--bg-subtle);"));
        assert!(html.contains(".nole-bg-dark{--text:var(--on-dark)"));
        // Print stays readable in black and white: dark backgrounds are
        // dropped and their text forced black.
        assert!(html.contains("@media print{"));
        assert!(html.contains(
            ".nole-bg-dark,.nole-bg-dark *{color:#000!important;background:none!important}"
        ));
        assert!(html.contains("page-break-inside:avoid"));
    }
}
