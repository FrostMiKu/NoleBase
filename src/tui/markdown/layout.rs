//! Terminal layout for the shared Markdown event stream.
use super::{table::Table, ImagePlacement, RenderedMarkup};
use crate::core::markup::{self, Event, SpannedEvent};
use crate::tui::theme::Theme;
use pulldown_cmark::{CodeBlockKind, Event as Md, HeadingLevel, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) fn render(events: &[SpannedEvent<'_>], width: usize, theme: Theme) -> RenderedMarkup {
    let mut layout = Layout {
        output: RenderedMarkup {
            lines: Vec::new(),
            links: Vec::new(),
            tags: Vec::new(),
            images: Vec::new(),
        },
        line: Line::default(),
        width: width.max(1),
        theme,
        styles: vec![Style::default()],
        lists: Vec::new(),
        quote: 0,
        prefix: String::new(),
        table: None,
    };
    layout.events(events);
    layout.flush();
    // Image reservations are real content even though their lines are blank.
    let image_end = layout
        .output
        .images
        .iter()
        .map(|image| image.row + image.height)
        .max()
        .unwrap_or(0);
    while layout.output.lines.len() > image_end
        && layout
            .output
            .lines
            .last()
            .is_some_and(|line| line.spans.is_empty())
    {
        layout.output.lines.pop();
    }
    layout.output
}

struct Layout {
    output: RenderedMarkup,
    line: Line<'static>,
    width: usize,
    theme: Theme,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    quote: usize,
    prefix: String,
    table: Option<Table>,
}

impl Layout {
    fn style(&self) -> Style {
        *self.styles.last().unwrap()
    }
    fn push_style(&mut self, style: Style) {
        self.styles.push(self.style().patch(style));
    }
    fn flush(&mut self) {
        if !self.line.spans.is_empty() {
            let line = std::mem::take(&mut self.line);
            if let Some(table) = &mut self.table {
                table.cells.push(line);
            } else {
                self.output.lines.push(line);
            }
        }
    }
    fn blank(&mut self) {
        self.flush();
        if self.table.is_none()
            && !self.output.lines.is_empty()
            && self
                .output
                .lines
                .last()
                .is_some_and(|line| !line.spans.is_empty())
        {
            self.output.lines.push(Line::default());
        }
    }
    fn emit(&mut self, text: &str, style: Style) {
        // Neither raw HTML nor prose can inject terminal controls.
        let clean: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .collect();
        for (index, part) in clean.split('\n').enumerate() {
            if index > 0 {
                self.flush();
            }
            let mut chunk = String::new();
            let mut used = self.line.width();
            if used == 0 && !self.prefix.is_empty() && self.table.is_none() {
                let prefix = self
                    .prefix
                    .chars()
                    .take(self.width.saturating_sub(1))
                    .collect::<String>();
                self.line.spans.push(Span::styled(
                    prefix,
                    Style::default().fg(self.theme.markdown_quote),
                ));
                used = self.line.width();
            }
            for ch in part.chars() {
                let cells = UnicodeWidthChar::width(ch).unwrap_or(0);
                if self.table.is_none()
                    && used + cells > self.width
                    && (used > 0 || !chunk.is_empty())
                {
                    if !chunk.is_empty() {
                        self.line
                            .spans
                            .push(Span::styled(std::mem::take(&mut chunk), style));
                    }
                    self.flush();
                    used = 0;
                }
                chunk.push(ch);
                used += cells;
            }
            if !chunk.is_empty() {
                self.line.spans.push(Span::styled(chunk, style));
            }
        }
    }
    fn events(&mut self, events: &[SpannedEvent<'_>]) {
        let mut index = 0;
        while index < events.len() {
            match &events[index].event {
                Event::Hashtag(name) => {
                    self.emit(
                        &format!("#{name}"),
                        self.style()
                            .fg(self.theme.markdown_hashtag)
                            .add_modifier(Modifier::BOLD),
                    );
                }
                Event::WikiLink(target) => self.emit(
                    &format!("[[{target}]]"),
                    self.link_style(self.theme.markdown_wikilink),
                ),
                Event::Embed(target) => {
                    if is_image(target) {
                        self.image(target, "", target);
                    } else {
                        self.emit(
                            &format!("![[{target}]]"),
                            self.link_style(self.theme.markdown_link),
                        );
                    }
                }
                Event::Markdown(Md::Start(Tag::Image {
                    dest_url, title, ..
                })) => {
                    let end = events[index + 1..]
                        .iter()
                        .position(|e| matches!(e.event, Event::Markdown(Md::End(TagEnd::Image))))
                        .map_or(index, |n| index + 1 + n);
                    self.image(
                        dest_url,
                        title,
                        &markup::visible_text(&events[index + 1..end]),
                    );
                    index = end;
                }
                Event::Markdown(Md::Start(Tag::CodeBlock(kind))) => {
                    let end = events[index + 1..]
                        .iter()
                        .position(|e| {
                            matches!(e.event, Event::Markdown(Md::End(TagEnd::CodeBlock)))
                        })
                        .map_or(events.len(), |n| index + 1 + n);
                    let source = markup::visible_text(&events[index + 1..end]);
                    let info = match kind {
                        CodeBlockKind::Fenced(info) => info.as_ref(),
                        _ => "text",
                    };
                    self.code(info.split_whitespace().next().unwrap_or("text"), &source);
                    index = end;
                }
                Event::Markdown(Md::Start(Tag::Item)) => {
                    let next = events[index + 1..].iter().find(|event| {
                        !matches!(event.event, Event::Markdown(Md::Start(Tag::Paragraph)))
                    });
                    let task = next.and_then(|event| match event.event {
                        Event::Markdown(Md::TaskListMarker(done)) => Some(done),
                        _ => None,
                    });
                    self.start_item(task);
                }
                Event::Markdown(event) => self.markdown(event),
            }
            index += 1;
        }
    }
    fn start_item(&mut self, task: Option<bool>) {
        self.flush();
        let list_marker = if let Some(Some(number)) = self.lists.last_mut() {
            let text = format!("{number}. ");
            *number += 1;
            text
        } else {
            "• ".to_owned()
        };
        let marker = task.map_or(list_marker, |done| {
            if done {
                "☑ ".to_owned()
            } else {
                "☐ ".to_owned()
            }
        });
        let indent = " ".repeat(1 + self.lists.len().saturating_sub(1) * 3);
        self.emit(
            &format!("{indent}{marker}"),
            Style::default().fg(self.theme.markdown_list),
        );
    }
    fn link_style(&self, color: ratatui::style::Color) -> Style {
        self.style()
            .fg(color)
            .underline_color(self.theme.markdown_link)
            .add_modifier(Modifier::UNDERLINED)
    }
    fn image(&mut self, source: &str, title: &str, alt: &str) {
        self.flush();
        let row = self.output.lines.len();
        self.output.images.push(ImagePlacement {
            source: source.to_owned(),
            title: title.to_owned(),
            alt: alt.to_owned(),
            row,
            column: 0,
            width: self.width,
            height: 12,
        });
        self.output
            .lines
            .extend(std::iter::repeat_n(Line::default(), 12));
    }
    fn markdown(&mut self, event: &Md<'_>) {
        match event {
            Md::Text(text) | Md::Html(text) | Md::InlineHtml(text) => self.emit(text, self.style()),
            Md::Code(text) => self.emit(text, self.style().fg(self.theme.markdown_code)),
            Md::SoftBreak | Md::HardBreak => self.flush(),
            Md::Rule => {
                self.blank();
                self.emit(
                    &"─".repeat(self.width),
                    Style::default().fg(self.theme.markdown_rule),
                );
                self.blank();
            }
            Md::TaskListMarker(_) => {}
            Md::FootnoteReference(name) => self.emit(
                &format!("[{name}]"),
                self.link_style(self.theme.markdown_link),
            ),
            Md::InlineMath(source) => {
                let rendered = super::math::render(source, self.width);
                self.emit(
                    &rendered.unwrap_or_else(|| format!("${source}$")),
                    self.style().fg(self.theme.markdown_code),
                );
            }
            Md::DisplayMath(source) => {
                self.blank();
                let rendered = super::math::render(source.trim_matches('\n'), self.width);
                self.emit(
                    &rendered.unwrap_or_else(|| format!("$${source}$$")),
                    self.style().fg(self.theme.markdown_code),
                );
                self.blank();
            }
            Md::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    self.blank();
                    let color = match level {
                        HeadingLevel::H1 => self.theme.markdown_heading_1,
                        HeadingLevel::H2 => self.theme.markdown_heading_2,
                        HeadingLevel::H3 => self.theme.markdown_heading_3,
                        _ => self.theme.markdown_heading_minor,
                    };
                    let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
                    self.push_style(if *level == HeadingLevel::H1 {
                        style.add_modifier(Modifier::UNDERLINED)
                    } else {
                        style
                    });
                }
                Tag::Paragraph => {
                    if self.lists.is_empty() {
                        self.flush();
                    }
                }
                Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
                Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
                Tag::Strikethrough => {
                    self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
                }
                Tag::Link { .. } => self.push_style(self.link_style(self.theme.markdown_link)),
                Tag::BlockQuote(_) => {
                    self.blank();
                    self.quote += 1;
                    self.prefix = "  ▌ ".repeat(self.quote);
                    self.push_style(Style::default().fg(self.theme.markdown_quote));
                }
                Tag::List(first) => {
                    self.flush();
                    self.lists.push(*first);
                }
                Tag::Table(alignments) => {
                    self.blank();
                    self.table = Some(Table {
                        alignments: alignments.clone(),
                        ..Default::default()
                    });
                }
                Tag::Item => unreachable!("list items are handled with their task marker"),
                Tag::TableHead | Tag::TableRow => self.flush(),
                Tag::TableCell => {
                    self.line = Line::default();
                }
                Tag::FootnoteDefinition(name) => {
                    self.blank();
                    self.emit(&format!("[{name}] "), self.style());
                }
                _ => {}
            },
            Md::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    self.styles.pop();
                    self.blank();
                }
                TagEnd::Paragraph => {
                    if self.lists.is_empty() {
                        self.blank();
                    }
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                    self.styles.pop();
                }
                TagEnd::BlockQuote(_) => {
                    self.blank();
                    self.quote = self.quote.saturating_sub(1);
                    self.prefix = "  ▌ ".repeat(self.quote);
                    self.styles.pop();
                }
                TagEnd::Item => self.flush(),
                TagEnd::List(_) => {
                    self.flush();
                    self.lists.pop();
                }
                TagEnd::TableCell => {
                    if self.line.spans.is_empty() {
                        if let Some(table) = &mut self.table {
                            table.cells.push(Line::default());
                        }
                    } else {
                        self.flush();
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(table) = &mut self.table {
                        table.rows.push(std::mem::take(&mut table.cells));
                    }
                }
                TagEnd::Table => {
                    if let Some(table) = self.table.take() {
                        self.output
                            .lines
                            .extend(super::table::render(table, self.width));
                    }
                    self.blank();
                }
                _ => {}
            },
        }
    }
    fn code(&mut self, language: &str, source: &str) {
        self.blank();
        let mut status = "";
        if language == "mermaid" {
            match super::diagram::render(source, self.width) {
                Ok(diagram) => {
                    self.emit(&diagram, self.style());
                    self.blank();
                    return;
                }
                Err(error) => {
                    status = match error {
                        mermaid_text::Error::UnsupportedDiagram(_) => "unsupported",
                        mermaid_text::Error::TooWide { .. } => "too wide",
                        mermaid_text::Error::ParseError(ref message)
                            if message.contains("not yet supported") =>
                        {
                            "unsupported"
                        }
                        _ => "syntax error",
                    }
                }
            }
        }
        let prefix_width = UnicodeWidthStr::width(self.prefix.as_str());
        let width = self.width.saturating_sub(prefix_width).max(1);
        for mut line in super::code::render(source, language, status, width, self.theme) {
            if !self.prefix.is_empty() {
                line.spans.insert(
                    0,
                    Span::styled(
                        self.prefix.clone(),
                        Style::default().fg(self.theme.markdown_quote),
                    ),
                );
            }
            self.output.lines.push(line);
        }
        self.blank();
    }
}

fn is_image(target: &str) -> bool {
    target.rsplit('.').next().is_some_and(|ext| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg"
        )
    })
}
