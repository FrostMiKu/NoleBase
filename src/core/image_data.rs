//! Unified image validation, decoding, and normalization shared by the UI
//! (`media.rs`), the agent's `read` tool, and provider message preparation.
//!
//! The UI and the agent must observe the same limits and the same decoding
//! behavior; keeping the implementation here (instead of a second copy) makes
//! that a module-level invariant. Format detection is always by content magic
//! bytes; extensions and MIME headers remain display hints only.

use std::io::Cursor;

use anyhow::{bail, Context, Result};
use image::ImageReader;

/// Maximum encoded image size (bytes) accepted for display and model input.
pub(crate) const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum decoded image dimension (pixels) in either axis.
pub(crate) const MAX_IMAGE_DIMENSION: u32 = 4096;
/// Maximum decoded allocation guard (bytes) applied by the image decoder.
pub(crate) const MAX_IMAGE_ALLOC: u64 = 64 * 1024 * 1024;

/// Decode `bytes` into an RGBA-rasterable image with the repository's shared
/// limits, returning the decoded image and the detected source format.
///
/// Zero-size and over-limit images fail here; decoding is CPU-bound pixel
/// work, so callers must run this inside `spawn_blocking`.
pub(crate) fn decode_image(bytes: &[u8]) -> Result<(image::DynamicImage, image::ImageFormat)> {
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        bail!("image exceeds the {MAX_IMAGE_BYTES} byte limit");
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("detecting image format")?;
    let format = reader.format().unwrap_or(image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC);
    reader.limits(limits);
    let image = reader.decode().context("decoding image")?;
    if image.width() == 0 || image.height() == 0 {
        bail!("image has zero dimensions");
    }
    Ok((image, format))
}

/// Detect an image format from the short magic-byte prefix while keeping file
/// reads bounded. Used to decide whether a target should
/// be treated as an image before a bounded full read.
pub(crate) fn detect_image_format(prefix: &[u8]) -> Option<image::ImageFormat> {
    image::guess_format(prefix).ok()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn raster() -> image::DynamicImage {
        image::DynamicImage::new_rgb8(8, 4)
    }

    fn encode(image: &image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn decode_image_reports_format_and_dimensions() {
        let png = encode(&raster(), image::ImageFormat::Png);
        let (decoded, format) = decode_image(&png).unwrap();
        assert_eq!(format, image::ImageFormat::Png);
        assert_eq!((decoded.width(), decoded.height()), (8, 4));
    }

    #[test]
    fn detect_image_format_uses_magic_bytes_only() {
        let png = encode(&raster(), image::ImageFormat::Png);
        let jpeg = encode(&raster(), image::ImageFormat::Jpeg);
        assert_eq!(
            detect_image_format(&png[..16]),
            Some(image::ImageFormat::Png)
        );
        assert_eq!(
            detect_image_format(&jpeg[..16]),
            Some(image::ImageFormat::Jpeg)
        );
        assert_eq!(detect_image_format(b"plain text..."), None);
    }

    #[test]
    fn zero_sized_and_over_limit_inputs_fail() {
        assert!(decode_image(&[]).is_err());
        let oversized = vec![0u8; (MAX_IMAGE_BYTES + 1) as usize];
        assert!(decode_image(&oversized).is_err());
    }
}
