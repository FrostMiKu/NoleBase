use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use base64::Engine;

use crate::attachment::{AttachmentStore, AttachmentUri};

pub(crate) const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// Upper bound on the cumulative bytes of all embedded resources (images) in
/// one rendered export. Bounds the memory held by the renderer and by the
/// data-URI inlined artifact / PDF image table.
pub(crate) const MAX_TOTAL_ASSET_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Default)]
pub(crate) struct Assets {
    pub bytes: BTreeMap<String, Vec<u8>>,
    pub mime: BTreeMap<String, &'static str>,
    total_bytes: u64,
}

impl Assets {
    pub fn insert(&mut self, bytes: Vec<u8>, mime: &'static str) -> Result<String> {
        let byte_count = u64::try_from(bytes.len()).context("export asset is too large")?;
        let total = self
            .total_bytes
            .checked_add(byte_count)
            .context("export asset size overflow")?;
        if total > MAX_TOTAL_ASSET_BYTES {
            bail!(
                "export images exceed the {}-byte total limit",
                MAX_TOTAL_ASSET_BYTES
            );
        }
        let key = format!("nole-export-asset-{}", self.bytes.len());
        self.mime.insert(key.clone(), mime);
        self.bytes.insert(key.clone(), bytes);
        self.total_bytes = total;
        Ok(key)
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn materialize_data_uris(&self, html: &str) -> String {
        let mut rendered = html.to_string();
        for (key, bytes) in &self.bytes {
            let mime = self
                .mime
                .get(key)
                .copied()
                .unwrap_or("application/octet-stream");
            let uri = format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            );
            rendered = rendered.replace(
                &format!("<img src=\"{key}\""),
                &format!("<img src=\"{uri}\""),
            );
        }
        rendered
    }
}

pub(crate) fn resolve_image(
    root: &Path,
    note: &Path,
    target: &str,
    attachments: &AttachmentStore,
) -> Result<(Vec<u8>, &'static str)> {
    let bytes = if AttachmentUri::is_attachment_uri(target) {
        let uri = AttachmentUri::parse(target)
            .with_context(|| format!("parsing attachment URI {target:?}"))?;
        attachments
            .read_limited(uri.id(), MAX_IMAGE_BYTES)
            .with_context(|| format!("reading attachment image {target:?}"))?
    } else {
        let target = Path::new(target);
        if target.is_absolute() {
            bail!("absolute image paths are not exportable");
        }
        let candidate =
            crate::storage::normalize_lexical(note.parent().unwrap_or(root).join(target))?;
        reject_symlink_components(root, &candidate)?;
        let canonical_root = fs::canonicalize(root).context("canonicalizing Nole root")?;
        let canonical = fs::canonicalize(&candidate)
            .with_context(|| format!("resolving image {}", candidate.display()))?;
        if !canonical.starts_with(&canonical_root) {
            bail!("image is outside the Nole root");
        }
        let relative = canonical.strip_prefix(&canonical_root)?;
        if relative.starts_with("config") || relative.starts_with("attachments") {
            bail!("application-managed image paths are not exportable");
        }
        let metadata = fs::metadata(&canonical)?;
        if !metadata.is_file() {
            bail!("image target is not a regular file");
        }
        if metadata.len() > MAX_IMAGE_BYTES {
            bail!("image exceeds the 8 MiB export limit");
        }
        fs::read(canonical)?
    };
    let format = image::guess_format(&bytes)
        .with_context(|| format!("unsupported image format for image {target:?}"))?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => bail!("unsupported image format"),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .with_context(|| format!("decoding image {target:?}"))?;
    Ok((bytes, mime))
}

pub(crate) fn reject_symlink_components(root: &Path, target: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(root).context("canonicalizing Nole root")?;
    let relative = target
        .strip_prefix(root)
        .or_else(|_| target.strip_prefix(&canonical_root))
        .context("path is outside the Nole root")?;
    let mut current = canonical_root;
    for component in relative.components() {
        let Component::Normal(part) = component else {
            bail!("invalid path component");
        };
        current.push(part);
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            bail!("symlink traversal is not allowed: {}", current.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, AttachmentStore) {
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

    #[test]
    fn resolve_local_png_within_root() {
        let (_directory, root, store) = fixture();
        let note = root.join("data/nested/note.md");
        save_png(root.join("data/nested/pic.png"));
        let (bytes, mime) = resolve_image(&root, &note, "pic.png", &store).unwrap();
        assert_eq!(mime, "image/png");
        assert!(bytes.starts_with(b"\x89PNG"));
    }

    #[test]
    fn resolve_rejects_absolute_and_escaping_paths() {
        let (_directory, root, store) = fixture();
        let note = root.join("data/nested/note.md");
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().join("outside.png");
        save_png(&outside);
        let error = resolve_image(&root, &note, outside.to_str().unwrap(), &store).unwrap_err();
        assert!(error.to_string().contains("absolute image paths"));
        let error = resolve_image(&root, &note, "../../../outside.png", &store).unwrap_err();
        assert!(error.to_string().contains("outside") || error.to_string().contains("escapes"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_rejects_symlink_traversal() {
        let (_directory, root, store) = fixture();
        let note = root.join("data/nested/note.md");
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().join("outside.png");
        save_png(&outside);
        std::os::unix::fs::symlink(&outside, root.join("data/nested/link.png")).unwrap();
        let error = resolve_image(&root, &note, "link.png", &store).unwrap_err();
        assert!(error.to_string().contains("symlink traversal"));
    }

    #[test]
    fn resolve_rejects_non_image_and_corrupt_bytes() {
        let (_directory, root, store) = fixture();
        let note = root.join("data/nested/note.md");
        std::fs::write(root.join("data/nested/plain.txt"), b"not an image").unwrap();
        let error = resolve_image(&root, &note, "plain.txt", &store).unwrap_err();
        assert!(error.to_string().contains("unsupported image format"));
        std::fs::write(root.join("data/nested/corrupt.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let error = resolve_image(&root, &note, "corrupt.png", &store).unwrap_err();
        assert!(error.to_string().contains("decoding image"));
        assert!(error.to_string().contains("corrupt.png"));
    }

    #[test]
    fn resolve_rejects_unsupported_format() {
        let (_directory, root, store) = fixture();
        let note = root.join("data/nested/note.md");
        std::fs::write(
            root.join("data/nested/pic.bmp"),
            b"BM\x00\x00\x00\x00\x00\x00\x00",
        )
        .unwrap();
        let error = resolve_image(&root, &note, "pic.bmp", &store).unwrap_err();
        assert!(error.to_string().contains("unsupported image format"));
    }

    #[test]
    fn resolve_rejects_oversized_images() {
        let (_directory, root, store) = fixture();
        let note = root.join("data/nested/note.md");
        let path = root.join("data/nested/huge.png");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .unwrap();
        let error = resolve_image(&root, &note, "huge.png", &store).unwrap_err();
        assert!(error.to_string().contains("8 MiB"));
    }

    #[test]
    fn resolve_rejects_application_managed_paths() {
        let (_directory, root, store) = fixture();
        let note = root.join("data/nested/note.md");
        std::fs::create_dir_all(root.join("config")).unwrap();
        save_png(root.join("config/x.png"));
        let error = resolve_image(&root, &note, "../../config/x.png", &store).unwrap_err();
        assert!(error.to_string().contains("application-managed"));
    }

    #[test]
    fn resolve_attachment_uri_image() {
        let (_directory, root, store) = fixture();
        let note = root.join("data/nested/note.md");
        let path = root.join("x.png");
        save_png(&path);
        let png = std::fs::read(path).unwrap();
        let managed = store.import_bytes(&png, Some("managed.png")).unwrap();
        let uri = AttachmentUri::from_id(managed.id).to_string();
        let (bytes, mime) = resolve_image(&root, &note, &uri, &store).unwrap();
        assert_eq!(mime, "image/png");
        assert!(bytes.starts_with(b"\x89PNG"));
    }

    #[test]
    fn materialize_data_uris_embeds_each_asset_with_its_mime() {
        let mut assets = Assets::default();
        let key = assets.insert(vec![1, 2, 3], "image/png").unwrap();
        let html = format!("<code>src=\"{key}\"</code><img src=\"{key}\">");
        let rendered = assets.materialize_data_uris(&html);
        assert!(rendered.contains("src=\"data:image/png;base64,AQID\""));
        assert!(rendered.contains(&format!("<code>src=\"{key}\"</code>")));
    }

    #[test]
    fn insert_enforces_total_limit_before_storing_bytes() {
        let mut assets = Assets {
            total_bytes: MAX_TOTAL_ASSET_BYTES,
            ..Assets::default()
        };
        let error = assets.insert(vec![1], "image/png").unwrap_err();
        assert!(error.to_string().contains("total limit"));
        assert!(assets.bytes.is_empty());
    }
}
