//! System clipboard attachment input.

use std::io::Cursor;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clipboard_rs::common::RustImage;
use clipboard_rs::{Clipboard, ClipboardContext, ContentFormat};

pub(crate) enum ClipboardAttachmentContent {
    Files(Vec<PathBuf>),
    Png(Vec<u8>),
}

/// Read only attachment-capable clipboard formats, preferring files over image data.
pub(crate) fn read_attachment_content() -> Result<ClipboardAttachmentContent> {
    let clipboard = ClipboardContext::new()
        .map_err(|error| anyhow::anyhow!("opening system clipboard: {error}"))?;
    clipboard
        .available_formats()
        .map_err(|error| anyhow::anyhow!("reading available clipboard formats: {error}"))?;

    if clipboard.has(ContentFormat::Files) {
        let files = clipboard
            .get_files()
            .map_err(|error| anyhow::anyhow!("reading clipboard file list: {error}"))?
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if files.is_empty() {
            bail!("clipboard file list is empty");
        }
        return Ok(ClipboardAttachmentContent::Files(files));
    }

    if clipboard.has(ContentFormat::Image) {
        let rgba = clipboard
            .get_image()
            .map_err(|error| anyhow::anyhow!("reading clipboard image: {error}"))?
            .to_rgba8()
            .map_err(|error| anyhow::anyhow!("decoding clipboard image: {error}"))?;
        return Ok(ClipboardAttachmentContent::Png(encode_png(rgba)?));
    }

    bail!("clipboard does not contain files or an image")
}

/// Replace the system clipboard with plain text.
pub(crate) fn write_text(text: &str) -> Result<()> {
    let clipboard = ClipboardContext::new()
        .map_err(|error| anyhow::anyhow!("opening system clipboard: {error}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|error| anyhow::anyhow!("writing clipboard text: {error}"))
}

fn encode_png(rgba: image::RgbaImage) -> Result<Vec<u8>> {
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(rgba)
        .write_to(&mut png, image::ImageFormat::Png)
        .context("encoding clipboard image as PNG")?;
    Ok(png.into_inner())
}

/// Import all clipboard attachment content, removing this operation's imports
/// if any later source fails.
pub(crate) fn import_clipboard_attachments(
    store: &crate::attachment::AttachmentStore,
) -> Result<Vec<crate::attachment::AttachmentMetadata>> {
    import_attachment_content(store, read_attachment_content()?)
}

/// Import all attachment content, removing this operation's imports if any
/// later source fails.
pub(crate) fn import_attachment_content(
    store: &crate::attachment::AttachmentStore,
    content: ClipboardAttachmentContent,
) -> Result<Vec<crate::attachment::AttachmentMetadata>> {
    let mut imported = Vec::new();
    let result = (|| -> Result<()> {
        match content {
            ClipboardAttachmentContent::Files(paths) => {
                for path in paths {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| path.display().to_string());
                    imported.push(store.import_path_as(&path, &name)?);
                }
                Ok(())
            }
            ClipboardAttachmentContent::Png(png) => {
                imported.push(store.import_reader(
                    png.as_slice(),
                    "pasted-image.png",
                    "pasted-image.png",
                )?);
                Ok(())
            }
        }
    })();
    if let Err(error) = result {
        for metadata in &imported {
            let _ = store.remove(metadata.id);
        }
        return Err(error);
    }
    if imported.is_empty() {
        bail!("clipboard attachment import produced no attachments");
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachment::{AttachmentQuery, AttachmentStore};
    use std::fs;

    #[test]
    fn imports_files_in_clipboard_order() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("a.txt");
        let second = root.path().join("b.txt");
        fs::write(&first, b"a").unwrap();
        fs::write(&second, b"b").unwrap();
        let store = AttachmentStore::new(root.path().join("attachments"));
        let imported = import_attachment_content(
            &store,
            ClipboardAttachmentContent::Files(vec![first, second]),
        )
        .unwrap();
        assert_eq!(
            imported
                .iter()
                .map(|metadata| metadata.display_name.as_str())
                .collect::<Vec<_>>(),
            ["a.txt", "b.txt"]
        );
    }

    #[test]
    fn failed_file_import_rolls_back_prior_files() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("a.txt");
        fs::write(&first, b"a").unwrap();
        let store = AttachmentStore::new(root.path().join("attachments"));
        assert!(import_attachment_content(
            &store,
            ClipboardAttachmentContent::Files(vec![first, root.path().join("missing.txt")]),
        )
        .is_err());
        assert_eq!(store.list(&AttachmentQuery::default()).unwrap().total, 0);
    }

    #[test]
    fn rgba_clipboard_content_is_png_image_attachment() {
        let root = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(root.path().join("attachments"));
        let png = encode_png(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([255, 0, 0, 255]),
        ))
        .unwrap();
        let metadata = import_attachment_content(&store, ClipboardAttachmentContent::Png(png))
            .unwrap()
            .remove(0);
        assert_eq!(metadata.display_name, "pasted-image.png");
        assert_eq!(metadata.mime_type.as_deref(), Some("image/png"));
        assert!(crate::attachment::markdown_embed(&metadata)
            .starts_with("![pasted-image.png](nole://attachment/"));
    }

    #[test]
    fn empty_file_list_is_not_a_successful_import() {
        let root = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(root.path().join("attachments"));
        assert!(
            import_attachment_content(&store, ClipboardAttachmentContent::Files(Vec::new()),)
                .is_err()
        );
    }
}
