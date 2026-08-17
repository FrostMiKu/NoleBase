//! Generic UTF-8 text file parser for the unified `read` tool.
//!
//! This fallback is registered last so more specific file parsers (for
//! example PDF) can claim a target first. It scans the complete file to retain
//! a strong edit identity and exact line count while returning only a bounded
//! line window, and gates edits through the shared `ReadTracker`.

use std::fmt::Write as _;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::fs as async_fs;

use super::paging::{continuation_selector, line_window, plain_response_len, read_utf8_page};
use super::{listed_path, ParseContext, ReadParser, ReadPayload, Target};

pub(crate) struct TextFileParser;

#[async_trait::async_trait]
impl ReadParser for TextFileParser {
    fn name(&self) -> &'static str {
        "text_file"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::File { .. })
    }

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        input: &Value,
    ) -> Result<ReadPayload> {
        let Target::File { path, range } = target else {
            bail!("text_file parser received non-file target");
        };
        let metadata = async_fs::metadata(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if !metadata.is_file() {
            bail!("path is not a regular file: {}", path.display());
        }
        let (offset, limit) = line_window(*range, input)?;
        let page_path = path.clone();
        let page = tokio::task::spawn_blocking(move || {
            read_utf8_page(&page_path, offset, limit, plain_response_len)
        })
        .await
        .context("joining paginated file read")??
        .with_context(|| format!("file is not valid UTF-8: {}", path.display()))?;
        let target = listed_path(&ctx.root, path);
        let mut output = match &page.tag {
            Some(tag) => format!("[{target}#{tag}]"),
            None => format!("[{target}]"),
        };
        for (index, text) in page.lines.iter().enumerate() {
            write!(output, "\n{}:{text}", page.start + index + 1)?;
        }
        let first = if page.start < page.end {
            page.start + 1
        } else {
            0
        };
        match page.total_lines {
            Some(total_lines) => write!(
                output,
                "\n\n[Showing lines {first}-{} of {total_lines}",
                page.end
            )?,
            None => write!(output, "\n\n[Showing lines {first}-{}", page.end)?,
        }
        if page.has_more {
            write!(
                output,
                ". Continue with {}",
                continuation_selector(&target, page.end, limit)
            )?;
        }
        output.push(']');
        if let (Some(identity), Some(tag)) = (page.identity, page.tag) {
            let total_lines = page
                .total_lines
                .expect("local text reads always scan the complete file");
            let tracked_tag = ctx.reads.mark_file(
                path.clone(),
                identity,
                tag.clone(),
                page.start,
                page.end,
                total_lines,
            )?;
            debug_assert_eq!(tag, tracked_tag);
        }
        Ok(ReadPayload::Text(output))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use super::super::test_support::large_text;
    use super::super::Read;
    use crate::agent::{ReadTracker, Tool};

    #[tokio::test(flavor = "current_thread")]
    async fn local_files_over_one_megabyte_are_read_in_pages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.txt");
        let content = large_text(50_000);
        assert!(content.len() > 1_000_000);
        fs::write(&path, &content).unwrap();
        let tracker = Arc::new(ReadTracker::default());
        let read = Read::new(directory.path(), tracker.clone(), reqwest::Client::new()).unwrap();

        let output = read
            .execute(&json!({"path": "large.txt:49991-49995"}))
            .await
            .unwrap();
        assert!(output.contains("49991:line 49990"));
        assert!(output.contains("[Showing lines 49991-49995"));
        assert!(output.contains("Continue with large.txt:49996-50000"));
        assert!(tracker
            .file_state(&fs::canonicalize(path).unwrap())
            .unwrap()
            .is_some());
        let final_page = read
            .execute(&json!({"path": "large.txt:49999-50003"}))
            .await
            .unwrap();
        assert!(final_page.contains("[Showing lines 49999-50000 of 50000"));
        assert!(!final_page.contains("Continue with"));
    }
}
