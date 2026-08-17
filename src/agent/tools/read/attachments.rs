//! Attachment parser for the unified `read` tool.
//!
//! Attachment reads are read-only: they never register an edit snapshot.
//! Physical object paths stay private; the URI is the only address exposed to
//! the model. Images return header-only dimensions, documents extract markdown,
//! textual content pages like a file, and everything else returns metadata.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::fs as async_fs;

use crate::attachment::{AttachmentId, AttachmentMetadata, AttachmentStore, AttachmentUri};

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
        input: &Value,
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
            let mut payload = attachment_metadata_json(*uri, &metadata);
            if let Ok((width, height, format)) = image_dimensions(&ctx.attachments, uri.id()) {
                payload["width"] = json!(width);
                payload["height"] = json!(height);
                payload["format"] = json!(format);
            }
            return Ok(ReadPayload::Structured(payload));
        }
        let attachment_path = Path::new(&metadata.display_name);
        if DocumentFormat::from_path(attachment_path).is_some() {
            if metadata.size > MAX_DOCUMENT_BYTES {
                bail!("document exceeds the {MAX_DOCUMENT_BYTES} byte extraction limit");
            }
            let (offset, limit) = line_window(*range, input)?;
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
            add_structured_page(&mut payload, page, &uri.to_string(), offset, limit);
            return Ok(ReadPayload::Structured(payload));
        }
        if !is_textual_mime(mime) {
            return Ok(ReadPayload::Structured(attachment_metadata_json(
                *uri, &metadata,
            )));
        }
        let (offset, limit) = line_window(*range, input)?;
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
        add_structured_page(&mut payload, page, &uri.to_string(), offset, limit);
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

/// Decode image dimensions from the file header only, without loading the
/// object bytes into memory. The store's `open` path is the sanctioned way to
/// reach the real content file for decoding.
fn image_dimensions(store: &AttachmentStore, id: AttachmentId) -> Result<(u32, u32, String)> {
    let path = store.open(id)?;
    let reader = image::ImageReader::open(&path)
        .with_context(|| format!("opening image {}", path.display()))?
        .with_guessed_format()
        .context("detecting image format")?;
    let format = reader
        .format()
        .map(|format| format!("{format:?}").to_lowercase())
        .unwrap_or_else(|| "unknown".to_string());
    let (width, height) = reader
        .into_dimensions()
        .context("reading image dimensions")?;
    Ok((width, height, format))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;

    use serde_json::{json, Value};

    use super::super::test_support::{large_text, simple_pdf};
    use super::super::{Read, ATTACHMENTS_DIR};
    use crate::agent::{ReadTracker, Tool};
    use crate::attachment::AttachmentStore;

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
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": format!("{uri}:1-20")}))
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
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let output = read
            .execute(&json!({ "path": format!("{uri}:3-4") }))
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
        assert_eq!(parsed["next"], format!("{uri}:5-6"));
        // Structured read-only content: no hashline `[path#TAG]` snapshot header
        // and no tag field, because attachment reads never gate edit.
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
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let output = read
            .execute(&json!({"path": format!("{uri}:49991-49995")}))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["range"], "49991-49995");
        assert_eq!(parsed["returned"], 5);
        assert_eq!(parsed["next"], format!("{uri}:49996-50000"));
        assert_eq!(parsed["total"], 50_000);
        assert_eq!(parsed["items"][0], "line 49990 xxxxxxxxxxxxxxxxxxxx");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn image_attachments_return_dimensions() {
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
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let parsed: Value =
            serde_json::from_str(&read.execute(&json!({ "path": uri })).await.unwrap()).unwrap();
        assert_eq!(parsed["kind"], "attachment");
        assert_eq!(parsed["mime_type"], "image/png");
        assert_eq!(parsed["width"], 8);
        assert_eq!(parsed["height"], 4);
        assert_eq!(parsed["format"], "png");
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
            Arc::new(ReadTracker::default()),
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
            Arc::new(ReadTracker::default()),
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
