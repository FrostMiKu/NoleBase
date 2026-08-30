//! Attachment parser for the unified `read` tool.
//!
//! Attachment reads inspect content and return structured results; edit
//! snapshots belong to the text-file editing pipeline.
//! Physical object paths stay private; the URI is the only address exposed to
//! the model. Images return native validated pixels, documents extract Markdown,
//! textual content pages like a file, and everything else returns metadata.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::fs as async_fs;

use crate::agent::images::image_block_from_bytes;
use crate::attachment::{AttachmentMetadata, AttachmentUri};
use crate::image_data::MAX_IMAGE_BYTES;
use crate::provider::ImageSource;

use super::document::{self, DocumentFormat, DocumentSourceKey};
use super::paging::{
    add_structured_page, json_response_len, line_window, page_extracted_text, read_utf8_page,
};
use super::{ParseContext, ReadParser, ReadPayload, Target, MAX_DOCUMENT_BYTES};

pub(crate) struct AttachmentParser;

#[async_trait::async_trait]
impl ReadParser for AttachmentParser {
    fn name(&self) -> &'static str {
        "attachment"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::Attachment { .. })
    }

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        _input: &Value,
    ) -> Result<ReadPayload> {
        let Target::Attachment { uri, range } = target else {
            bail!("attachment parser received non-attachment target");
        };
        let metadata = ctx
            .attachments
            .metadata(uri.id())
            .with_context(|| format!("reading attachment {uri}"))?;
        let mime = metadata.mime_type.as_deref().unwrap_or("");
        if mime.starts_with("image/") {
            if range.is_some() {
                bail!("range is not supported for image targets");
            }
            let store = ctx.attachments.clone();
            let id = uri.id();
            let bytes =
                tokio::task::spawn_blocking(move || store.read_limited(id, MAX_IMAGE_BYTES))
                    .await
                    .context("joining attachment image read")?
                    .with_context(|| format!("reading attachment image {uri}"))?;
            let display_name = metadata.display_name.clone();
            let uri_string = uri.to_string();
            let block = tokio::task::spawn_blocking(move || {
                image_block_from_bytes(
                    ImageSource::Attachment { uri: uri_string },
                    display_name,
                    bytes,
                )
            })
            .await
            .context("joining attachment image decode")??;
            return Ok(ReadPayload::Image(block));
        }
        let attachment_path = Path::new(&metadata.display_name);
        if DocumentFormat::from_path(attachment_path).is_some() {
            if metadata.size > MAX_DOCUMENT_BYTES {
                bail!("document exceeds the {MAX_DOCUMENT_BYTES} byte extraction limit");
            }
            let (offset, limit) = line_window(*range);
            let document_path = ctx
                .attachments
                .open(uri.id())
                .with_context(|| format!("opening attachment {uri}"))?;
            let identity = async_fs::metadata(&document_path)
                .await
                .with_context(|| format!("reading attachment {uri}"))?;
            let key = DocumentSourceKey::Attachment {
                id: uri.id().to_string(),
                len: identity.len(),
                modified: identity.modified().ok(),
            };
            let display_name = metadata.display_name.clone();
            let document = ctx
                .documents
                .get_or_extract(key, || async move {
                    let bytes = async_fs::read(&document_path)
                        .await
                        .with_context(|| format!("reading attachment document {display_name}"))?;
                    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
                        bail!("document exceeds the {MAX_DOCUMENT_BYTES} byte extraction limit");
                    }
                    let detected =
                        DocumentFormat::from_bytes_or_path(&bytes, Path::new(&display_name))
                            .with_context(|| {
                                format!("unsupported attachment document {display_name}")
                            })?;
                    document::extract_markdown(bytes, detected).await
                })
                .await?;
            let page = page_extracted_text(&document.markdown, offset, limit, json_response_len)?;
            let mut payload = attachment_metadata_json(*uri, &metadata);
            payload["format"] = json!(document.format.label());
            add_structured_page(&mut payload, page, offset, limit);
            return Ok(ReadPayload::Structured(payload));
        }
        if !is_textual_mime(mime) {
            return Ok(ReadPayload::Structured(attachment_metadata_json(
                *uri, &metadata,
            )));
        }
        let (offset, limit) = line_window(*range);
        let page_path = ctx
            .attachments
            .open(uri.id())
            .with_context(|| format!("opening attachment {uri}"))?;
        let page = tokio::task::spawn_blocking(move || {
            read_utf8_page(&page_path, offset, limit, json_response_len)
        })
        .await
        .context("joining paginated attachment read")??;
        let Some(page) = page else {
            return Ok(ReadPayload::Structured(attachment_metadata_json(
                *uri, &metadata,
            )));
        };
        let mut payload = attachment_metadata_json(*uri, &metadata);
        add_structured_page(&mut payload, page, offset, limit);
        Ok(ReadPayload::Structured(payload))
    }
}

fn is_textual_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json" | "application/xml" | "application/javascript" | "application/yaml"
        )
}

/// Structured metadata shared by every attachment read result. Physical object
/// paths stay private; the URI is the only address exposed to the model.
fn attachment_metadata_json(uri: AttachmentUri, metadata: &AttachmentMetadata) -> Value {
    json!({
        "name": metadata.display_name,
        "uri": uri.to_string(),
        "mime_type": metadata.mime_type,
        "size": metadata.size,
        "imported_at": metadata.imported_at.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;

    use serde_json::{json, Value};

    use super::super::test_support::{large_text, simple_pdf};
    use super::super::{Read, ATTACHMENTS_DIR};
    use crate::agent::{SnapshotStore, Tool};
    use crate::attachment::AttachmentStore;
    use crate::provider::ImageSource;

    #[tokio::test(flavor = "current_thread")]
    async fn attachment_pdf_uris_extract_selected_text_lines() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let uri = store
            .import_bytes(&simple_pdf("Attachment PDF marker"), Some("report.pdf"))
            .unwrap()
            .uri()
            .to_string();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": uri, "range": "1-20"}))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["format"], "pdf");
        assert!(parsed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("Attachment PDF marker")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn text_attachments_read_paginated_without_an_edit_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let content = (1..=5)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let uri = store
            .import_bytes(content.as_bytes(), Some("notes.txt"))
            .unwrap()
            .uri()
            .to_string();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let output = read
            .execute(&json!({ "path": uri, "range": "3-4" }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["kind"], "attachment");
        assert_eq!(parsed["target"], uri);
        assert_eq!(parsed["name"], "notes.txt");
        assert_eq!(parsed["mime_type"], "text/plain");
        assert_eq!(parsed["size"], content.len() as u64);
        assert_eq!(parsed["range"], "3-4");
        assert_eq!(parsed["returned"], 2);
        assert_eq!(parsed["total"], 5);
        assert_eq!(parsed["has_more"], true);
        assert_eq!(parsed["items"], json!(["line 3", "line 4"]));
        assert!(parsed.get("next").is_none());
        // Structured attachment content uses metadata pagination. Hashline
        // snapshots and edit-gating tags belong to editable text files.
        assert!(parsed.get("tag").is_none());
        assert!(!output.contains("[notes.txt#"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn text_attachments_over_one_megabyte_are_read_in_pages() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let content = large_text(50_000);
        let uri = store
            .import_bytes(content.as_bytes(), Some("large.txt"))
            .unwrap()
            .uri()
            .to_string();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let output = read
            .execute(&json!({"path": uri, "range": "49991-49995"}))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["range"], "49991-49995");
        assert_eq!(parsed["returned"], 5);
        assert!(parsed.get("next").is_none());
        assert_eq!(parsed["total"], 50_000);
        assert_eq!(parsed["items"][0], "line 49990 xxxxxxxxxxxxxxxxxxxx");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attachment_images_return_a_native_image_block() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let image = image::DynamicImage::new_rgb8(8, 4);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let uri = store
            .import_bytes(&bytes.into_inner(), Some("diagram.png"))
            .unwrap()
            .uri()
            .to_string();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let output = read.execute_output(&json!({ "path": uri })).await.unwrap();
        assert!(output
            .clone()
            .into_inline_text()
            .unwrap()
            .starts_with(&format!("Read image {uri} (8x4, image/png, ")));
        assert_eq!(output.images.len(), 1);
        let block = &output.images[0];
        assert_eq!(block.width, 8);
        assert_eq!(block.height, 4);
        assert!(block.bytes.is_some());
        assert!(matches!(&block.source, ImageSource::Attachment { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn binary_attachments_return_metadata_without_utf8_errors() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let bytes: Vec<u8> = vec![0xFF, 0x00, 0x01, 0xFE, 0x7F];
        let uri = store
            .import_bytes(&bytes, Some("blob.bin"))
            .unwrap()
            .uri()
            .to_string();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let parsed: Value =
            serde_json::from_str(&read.execute(&json!({ "path": uri })).await.unwrap()).unwrap();
        assert_eq!(parsed["kind"], "attachment");
        assert_eq!(parsed["name"], "blob.bin");
        assert_eq!(parsed["size"], 5);
        assert_eq!(parsed["mime_type"], Value::Null);
        assert!(parsed.get("lines").is_none());
        assert!(parsed.get("width").is_none());
        assert!(parsed.get("content").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn absent_attachments_error_without_physical_paths() {
        let directory = tempfile::tempdir().unwrap();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let uri = "nole://attachment/00000000-0000-4000-8000-000000000000".to_string();
        let error = read.execute(&json!({ "path": uri })).await.unwrap_err();
        // anyhow Display shows only the outer context; the "no such attachment"
        // cause is visible in the Debug chain.
        assert!(format!("{error:?}").contains("no such attachment"));
        assert!(!format!("{error:?}").contains("objects"));
    }
}
