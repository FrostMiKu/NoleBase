//! Office document and PDF file parser for the unified `read` tool.
//!
//! Extracted markdown is cached by source key and paged with the structured
//! response shape shared with the attachment and web parsers.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::fs as async_fs;

use super::document::{self, DocumentFormat, DocumentSourceKey};
use super::paging::{add_structured_page, json_response_len, line_window, page_extracted_text};
use super::{ParseContext, ReadParser, ReadPayload, Target, MAX_DOCUMENT_BYTES};

pub(crate) struct DocumentFileParser;

#[async_trait::async_trait]
impl ReadParser for DocumentFileParser {
    fn name(&self) -> &'static str {
        "document_file"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::File { path, .. } if DocumentFormat::from_path(path).is_some())
    }

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        _input: &Value,
    ) -> Result<ReadPayload> {
        let Target::File { path, range } = target else {
            bail!("document_file parser received non-file target");
        };
        let metadata = async_fs::metadata(path)
            .await
            .with_context(|| format!("reading document {}", path.display()))?;
        if metadata.len() > MAX_DOCUMENT_BYTES {
            bail!("document exceeds the {MAX_DOCUMENT_BYTES} byte extraction limit");
        }
        let (offset, limit) = line_window(*range);
        let key = DocumentSourceKey::Local {
            path: path.clone(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        };
        let document_path = path.clone();
        let document = ctx
            .documents
            .get_or_extract(key, || async move {
                let bytes = async_fs::read(&document_path)
                    .await
                    .with_context(|| format!("reading document {}", document_path.display()))?;
                if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
                    bail!("document exceeds the {MAX_DOCUMENT_BYTES} byte extraction limit");
                }
                let detected = DocumentFormat::from_bytes_or_path(&bytes, &document_path)
                    .with_context(|| format!("unsupported document {}", document_path.display()))?;
                document::extract_markdown(bytes, detected).await
            })
            .await?;
        let page = page_extracted_text(&document.markdown, offset, limit, json_response_len)?;
        let mut payload = json!({ "format": document.format.label() });
        add_structured_page(&mut payload, page, offset, limit);
        Ok(ReadPayload::Structured(payload))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use super::super::test_support::{simple_pdf, simple_rtf};
    use super::super::Read;
    use crate::core::agent::{SnapshotStore, Tool};

    #[tokio::test(flavor = "current_thread")]
    async fn local_pdf_paths_extract_selected_text_lines() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("report.pdf"),
            simple_pdf("PDF marker"),
        )
        .unwrap();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": "report.pdf", "range": "1-20"}))
            .await
            .unwrap();
        assert!(output.content.contains("kind: file"));
        assert!(output.content.contains("format: pdf"));
        assert!(output.content.contains("PDF marker"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_documents_page_extract_markdown_and_invalidate_on_change() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("report.rtf");
        fs::write(&path, simple_rtf(&["first", "second", "third"])).unwrap();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let first = read
            .execute(&json!({"path": "report.rtf", "range": "1-1"}))
            .await
            .unwrap();
        let second = read
            .execute(&json!({"path": "report.rtf", "range": "3-3"}))
            .await
            .unwrap();
        assert!(first.content.contains("format: rtf"));
        assert!(first.content.contains("items[1]: first"));
        assert!(second.content.contains("items[1]: second"));

        fs::write(&path, simple_rtf(&["replacement line", "new tail"])).unwrap();
        let changed = read
            .execute(&json!({"path": "report.rtf", "range": "1-1"}))
            .await
            .unwrap();
        assert!(changed.content.contains("items[1]: replacement line"));
    }
}
