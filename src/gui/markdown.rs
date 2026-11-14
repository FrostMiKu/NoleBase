//! Paragraph boundaries from Nole's shared Markdown parser.
use crate::core::markup::{self, Event};
use gpui_kit::SharedString;
use pulldown_cmark::{Alignment, Event as Markdown, LinkType, Tag, TagEnd};

#[derive(Clone)]
pub struct Table {
    pub alignments: Vec<Alignment>,
    pub rows: Vec<Vec<SharedString>>,
}

pub fn parse_table(source: &str) -> Option<Table> {
    let events = markup::parse(source);
    if !matches!(
        events.first().map(|item| &item.event),
        Some(Event::Markdown(Markdown::Start(Tag::Table(_))))
    ) {
        return None;
    }
    let mut table = None;
    let mut row = Vec::new();
    let mut start = 0;
    let mut cell_event_start = 0;
    for (index, item) in events.iter().enumerate() {
        match &item.event {
            Event::Markdown(Markdown::Start(Tag::Table(alignments))) => {
                table = Some(Table {
                    alignments: alignments.clone(),
                    rows: Vec::new(),
                });
            }
            Event::Markdown(Markdown::Start(Tag::TableCell)) => {
                start = item.span.start;
                cell_event_start = index;
            }
            Event::Markdown(Markdown::End(TagEnd::TableCell)) => {
                let definitions = link_definitions(&events[cell_event_start..=index]);
                row.push(format!("{}{}", source[start..item.span.end].trim(), definitions).into());
            }
            Event::Markdown(Markdown::End(TagEnd::TableHead | TagEnd::TableRow)) => {
                if let Some(table) = &mut table {
                    table.rows.push(std::mem::take(&mut row));
                }
            }
            _ => {}
        }
    }
    table
}

fn link_definitions(events: &[markup::SpannedEvent<'_>]) -> String {
    let mut definitions = String::new();
    let mut labels = std::collections::HashSet::new();
    for item in events {
        if let Event::Markdown(Markdown::Start(Tag::Link {
            link_type: LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut,
            id,
            dest_url,
            title,
            ..
        })) = &item.event
        {
            if labels.insert(id.as_ref()) {
                definitions.push_str(&format!("\n\n[{id}]: <{dest_url}>"));
                if !title.is_empty() {
                    definitions.push(' ');
                    definitions.push_str(&serde_json::to_string(title.as_ref()).unwrap());
                }
            }
        }
    }
    definitions
}

/// Keep complete top-level blocks together so code, tables, and nested lists
/// retain their context while the daily stream renders only visible blocks.
pub fn blocks(source: &str) -> Vec<SharedString> {
    let mut result = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let events = markup::parse(source);
    let mut block_event_start = 0;
    for (index, item) in events.iter().enumerate() {
        match &item.event {
            Event::Markdown(Markdown::Start(_)) => depth += 1,
            Event::Markdown(Markdown::End(_)) => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0 && item.span.end > start {
            let end = item.span.end;
            let block = source[start..end].trim();
            if !block.is_empty() {
                let definitions = link_definitions(&events[block_event_start..=index]);
                result.push(format!("{block}{definitions}").into());
            }
            start = end;
            block_event_start = index + 1;
        }
    }
    let tail = source[start..].trim();
    if !tail.is_empty() && !markup::parse(tail).is_empty() {
        result.push(tail.to_owned().into());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn table_cells_preserve_inline_markdown_and_escaped_pipes() {
        let table =
            parse_table("| Name | Details |\n| :--- | ---: |\n| **One** | a\\|b |\n").unwrap();
        assert_eq!(table.alignments, vec![Alignment::Left, Alignment::Right]);
        assert_eq!(table.rows[1][0].as_ref(), "**One**");
        assert_eq!(table.rows[1][1].as_ref(), "a\\|b");
    }
    #[test]
    fn virtualization_preserves_nested_blocks_and_code_fences() {
        let parts = blocks("# Heading\n\nFirst paragraph.\n\n- One\n  - Nested\n- Two\n\n```rust\nlet x = 1;\n\nlet y = 2;\n```\n");
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[2].as_ref(), "- One\n  - Nested\n- Two");
        assert!(parts[3].starts_with("```rust") && parts[3].ends_with("```"));
    }
}

#[derive(Clone, Copy)]
pub enum PreviewKind {
    Heading(u8),
    Paragraph,
    Quote,
    Code,
    List,
    Table,
    Rule,
}
#[derive(Clone)]
pub struct PreviewBlock {
    pub kind: PreviewKind,
    pub text: SharedString,
    pub table: Option<Table>,
}

/// Bounded preview trees follow quiet-md's thumbnail budget approach;
/// Nole's shared parser remains the authority for Markdown and extensions.
pub fn preview(source: &str) -> Vec<PreviewBlock> {
    use pulldown_cmark::{Tag, TagEnd};
    let mut result = Vec::new();
    let mut depth = 0usize;
    let mut kind = PreviewKind::Paragraph;
    let mut text = String::new();
    let mut remaining = 1200usize;
    let mut first = true;
    let mut table_start = 0;
    let mut list_counters: Vec<Option<u64>> = Vec::new();
    for item in markup::parse(source) {
        match &item.event {
            Event::Markdown(Markdown::Start(tag)) => {
                if depth == 0 {
                    kind = match tag {
                        Tag::Heading { level, .. } => PreviewKind::Heading(*level as u8),
                        Tag::BlockQuote(_) => PreviewKind::Quote,
                        Tag::CodeBlock(_) => PreviewKind::Code,
                        Tag::List(_) => PreviewKind::List,
                        Tag::Table(_) => PreviewKind::Table,
                        _ => PreviewKind::Paragraph,
                    };
                }
                if matches!(tag, Tag::Table(_)) {
                    table_start = item.span.start;
                }
                if let Tag::List(start) = tag {
                    list_counters.push(*start);
                }
                if matches!(tag, Tag::Item) && text.chars().count() < 390 {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&"  ".repeat(list_counters.len().saturating_sub(1).min(8)));
                    if let Some(Some(number)) = list_counters.last_mut() {
                        text.push_str(&format!("{number}. "));
                        *number += 1;
                    } else {
                        text.push_str("• ");
                    }
                }
                depth += 1;
            }
            Event::Markdown(Markdown::End(tag)) => {
                depth = depth.saturating_sub(1);
                if matches!(tag, TagEnd::List(_)) {
                    list_counters.pop();
                }
                if matches!(tag, TagEnd::Paragraph) && depth > 0 && text.chars().count() < 400 {
                    text.push('\n');
                }
                if matches!(kind, PreviewKind::Table) && text.chars().count() < 398 {
                    match tag {
                        TagEnd::TableCell => text.push_str(" · "),
                        TagEnd::TableHead | TagEnd::TableRow => {
                            text.truncate(text.trim_end_matches(" · ").len());
                            text.push('\n');
                        }
                        _ => {}
                    }
                }
            }
            Event::Markdown(Markdown::Rule) => kind = PreviewKind::Rule,
            Event::Markdown(Markdown::TaskListMarker(checked)) => {
                if text.ends_with("• ") {
                    text.truncate(text.len() - "• ".len());
                }
                if text.chars().count() < 398 {
                    text.push_str(if *checked { "☑ " } else { "☐ " });
                }
            }
            Event::Markdown(Markdown::SoftBreak | Markdown::HardBreak)
                if matches!(kind, PreviewKind::Code) && text.chars().count() < 400 =>
            {
                text.push('\n');
            }

            _ => text.push_str(&markup::visible_text_prefix(
                std::slice::from_ref(&item),
                400usize.saturating_sub(text.chars().count()),
            )),
        }
        if depth == 0 {
            let content: String = text.trim().chars().take(remaining.min(400)).collect();
            let skip_title = first && matches!(kind, PreviewKind::Heading(1));
            if !content.is_empty() || matches!(kind, PreviewKind::Rule) {
                first = false;
            }
            if !skip_title && (!content.is_empty() || matches!(kind, PreviewKind::Rule)) {
                remaining -= content.chars().count();
                result.push(PreviewBlock {
                    kind,
                    text: content.into(),
                    table: if matches!(kind, PreviewKind::Table) {
                        let source: String = source[table_start..item.span.end]
                            .chars()
                            .take(1600)
                            .collect();
                        parse_table(&source).map(|mut table| {
                            table.alignments.truncate(4);
                            table.rows.truncate(6);
                            for row in &mut table.rows {
                                row.truncate(4);
                                for cell in row {
                                    *cell = markup::visible_text_prefix(&markup::parse(cell), 80)
                                        .into();
                                }
                            }
                            table
                        })
                    } else {
                        None
                    },
                });
            }
            text.clear();
            if result.len() == 8 || remaining == 0 {
                break;
            }
        }
    }
    result
}

#[cfg(test)]
mod preview_tests {
    use super::*;
    #[test]
    fn thumbnail_content_stays_bounded_for_large_documents() {
        let source = format!(
            "# Title\n\n{}",
            ("Paragraph ".repeat(200) + "\n\n").repeat(1000)
        );
        let blocks = preview(&source);
        assert!(blocks.len() <= 8);
        assert!(
            blocks
                .iter()
                .map(|block| block.text.chars().count())
                .sum::<usize>()
                <= 1200
        );
        assert!(blocks.iter().all(|block| block.text.chars().count() <= 400));
    }
    #[test]
    fn thumbnail_preserves_heading_quote_and_code_structure() {
        let blocks =
            preview("# Title\n\n## Section\n\n> A quote\n\n```rust\nlet answer = 42;\n```\n");
        assert!(matches!(blocks[0].kind, PreviewKind::Heading(2)));
        assert!(matches!(blocks[1].kind, PreviewKind::Quote));
        assert!(matches!(blocks[2].kind, PreviewKind::Code));
        assert_eq!(blocks[2].text.as_ref(), "let answer = 42;");
    }
}

#[cfg(test)]
mod structure_tests {
    use super::*;

    #[test]
    fn thumbnail_keeps_task_completion_and_table_rows_readable() {
        let blocks = preview("- [x] Done\n- [ ] Next\n\n| Name | Status |\n| --- | --- |\n| One | Ready |\n| Two | Waiting |\n");
        assert_eq!(blocks[0].text.as_ref(), "☑ Done\n☐ Next");
        assert!(matches!(blocks[1].kind, PreviewKind::Table));
        assert_eq!(
            blocks[1].text.as_ref(),
            "Name · Status\nOne · Ready\nTwo · Waiting"
        );
    }

    #[test]
    fn thumbnail_preserves_code_line_breaks() {
        let blocks = preview("```rust\nlet one = 1;\nlet two = 2;\n```\n");
        assert_eq!(blocks[0].text.as_ref(), "let one = 1;\nlet two = 2;");
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;

    #[test]
    fn virtual_blocks_and_table_cells_keep_document_link_targets() {
        let source = "# Title\n\nRead [the guide][guide].\n\nAnother [guide].\n\n| Link |\n| --- |\n| [guide] |\n\n[guide]: https://example.com/guide \"Guide\"\n";
        let parts = blocks(source);
        for part in &parts[1..3] {
            let events = markup::parse(part);
            assert!(events.iter().any(|item| matches!(&item.event, Event::Markdown(Markdown::Start(Tag::Link { dest_url, title, .. })) if dest_url.as_ref() == "https://example.com/guide" && title.as_ref() == "Guide")));
        }
        let table = parse_table(&parts[3]).unwrap();
        let events = markup::parse(&table.rows[1][0]);
        assert!(events.iter().any(|item| matches!(&item.event, Event::Markdown(Markdown::Start(Tag::Link { dest_url, .. })) if dest_url.as_ref() == "https://example.com/guide")));
    }

    #[test]
    fn thumbnail_tables_keep_cells_and_bound_the_visible_preview() {
        let source = format!(
            "| Name | Details |\n| --- | --- |\n{}",
            "| **One** | Some details |\n".repeat(200)
        );
        let blocks = preview(&source);
        let table = blocks[0].table.as_ref().unwrap();
        assert_eq!(table.rows[1][0].as_ref(), "One");
        assert_eq!(table.rows[1][1].as_ref(), "Some details");
        assert!(table.rows.len() <= 6);
        assert!(table.rows.iter().all(|row| row.len() <= 4));
    }
}
