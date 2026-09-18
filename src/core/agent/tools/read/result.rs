//! Paginated reads of oversized text results owned by the current session.

use std::fmt::Write as _;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::paging::{line_window, plain_response_len, read_utf8_page};
use super::{ParseContext, ReadParser, ReadPayload, Target};

pub(crate) struct ResultParser;

#[async_trait::async_trait]
impl ReadParser for ResultParser {
    fn name(&self) -> &'static str {
        "session_result"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::Result { .. })
    }

    async fn parse(
        &self,
        _ctx: &ParseContext,
        target: &Target,
        _input: &Value,
    ) -> Result<ReadPayload> {
        let Target::Result { uri, path, range } = target else {
            bail!("session_result parser received non-result target");
        };
        let (offset, limit) = line_window(*range);
        let page_path = path.clone();
        let page = tokio::task::spawn_blocking(move || {
            read_utf8_page(&page_path, offset, limit, plain_response_len)
        })
        .await
        .context("joining paginated session result read")??
        .context("session result is not valid UTF-8")?;
        let mut output = format!("[{uri}]");
        for (index, text) in page.lines.iter().enumerate() {
            write!(output, "\n{}:{text}", page.start + index + 1)?;
        }
        let first = if page.start < page.end {
            page.start + 1
        } else {
            0
        };
        write!(
            output,
            "\n\n[Showing lines {first}-{} of {}]",
            page.end,
            page.total_lines.unwrap_or(page.end)
        )?;
        Ok(ReadPayload::Text(output))
    }
}
