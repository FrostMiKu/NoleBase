//! Generic UTF-8 text file parser for the unified `read` tool.
//!
//! This fallback is registered last so more specific file parsers (for
//! example PDF) can claim a target first. It scans the complete file to retain
//! a strong edit identity, exact line count, and bounded normalized text for
//! drift recovery while returning only a bounded line window, and gates edits
//! through the shared `SnapshotStore`.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::fs as async_fs;
use tokio::io::AsyncReadExt as _;

use crate::agent::images::image_block_from_bytes;
use crate::image_data::{detect_image_format, MAX_IMAGE_BYTES};
use crate::provider::ImageSource;

use super::paging::{continuation_selector, line_window, plain_response_len, read_utf8_page};
use super::{listed_path, ParseContext, ReadParser, ReadPayload, Target};

/// Number of leading bytes read to detect an image magic before committing to
/// a full read; covers every format signature the image crate recognizes.
const IMAGE_DETECT_PREFIX: usize = 32;

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
        let prefix = read_prefix(path, IMAGE_DETECT_PREFIX).await?;
        if detect_image_format(&prefix).is_some() {
            if range.is_some() {
                bail!("line selectors are not supported for image targets");
            }
            if metadata.len() > MAX_IMAGE_BYTES {
                bail!(
                    "image file {} is {} bytes, exceeding the {MAX_IMAGE_BYTES} byte limit",
                    path.display(),
                    metadata.len()
                );
            }
            let read_path = path.clone();
            let display_path = path.display().to_string();
            let bytes = tokio::task::spawn_blocking(move || std::fs::read(&read_path))
                .await
                .context("joining image file read")?
                .with_context(|| format!("reading image {display_path}"))?;
            let block_path = path.clone();
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let block = tokio::task::spawn_blocking(move || {
                image_block_from_bytes(ImageSource::LocalFile { path: block_path }, label, bytes)
            })
            .await
            .context("joining image file decode")??;
            return Ok(ReadPayload::Image(block));
        }
        let (offset, limit) = line_window(*range, input)?;
        let page_path = path.clone();
        let mut page = tokio::task::spawn_blocking(move || {
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
            let tracked_tag = ctx.reads.record(
                path.clone(),
                identity,
                tag.clone(),
                total_lines,
                page.full_text.take(),
                (page.start, page.end),
            )?;
            debug_assert_eq!(tag, tracked_tag);
        }
        Ok(ReadPayload::Text(output))
    }
}

/// Read up to `limit` leading bytes without loading the whole file, used to
/// detect an image magic prefix before deciding the full-read strategy.
async fn read_prefix(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut file = async_fs::File::open(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let mut prefix = vec![0u8; limit];
    let read = file
        .read(&mut prefix)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    prefix.truncate(read);
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use super::super::test_support::large_text;
    use super::super::Read;
    use crate::agent::{SnapshotStore, Tool};

    #[tokio::test(flavor = "current_thread")]
    async fn local_files_over_one_megabyte_are_read_in_pages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.txt");
        let content = large_text(50_000);
        assert!(content.len() > 1_000_000);
        fs::write(&path, &content).unwrap();
        let tracker = Arc::new(SnapshotStore::default());
        let read = Read::new(directory.path(), tracker.clone(), reqwest::Client::new()).unwrap();

        let output = read
            .execute(&json!({"path": "large.txt:49991-49995"}))
            .await
            .unwrap();
        assert!(output.contains("49991:line 49990"));
        assert!(output.contains("[Showing lines 49991-49995"));
        assert!(output.contains("Continue with large.txt:49996-50000"));
        assert!(tracker
            .head(&fs::canonicalize(path).unwrap())
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
