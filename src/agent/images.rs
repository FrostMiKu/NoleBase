//! Agent-side image materialization: prompt embeds, source resolution for
//! provider requests, and the shared validation/normalization entry point.
//!
//! Pixels never enter the on-disk session: blocks carry weak `ImageSource`
//! references plus dimensions, and the raw bytes stay an in-process `Arc`
//! cache that is re-read from the live source on session restore.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use mbdown::{Container, ContainerEnd, Event, Node};
use reqwest::Client;
use tokio::fs as async_fs;

use crate::agent::tools::read::fetch_web_response;
use crate::attachment::{AttachmentStore, AttachmentUri};
use crate::image_data::{decode_image, MAX_IMAGE_BYTES};
use crate::provider::{ImageBlock, ImageMediaType, ImageSource, Message, MessagePart, MessageRole};

/// Accumulated normalized byte cap for a single provider request, checked
/// before any base64 allocation or provider HTTP call.
pub(crate) const MAX_AGENT_IMAGE_REQUEST_BYTES: u64 = 20 * 1024 * 1024;

/// Exact capability-gated error used when image input is disabled.
pub(crate) const DISABLED_IMAGE_ERROR: &str =
    "image input is disabled; set supports_images = true in config/ai.toml for a vision-capable model";

/// Validate, decode, and normalize `bytes` into an `ImageBlock` carrying the
/// decoded dimensions and an `Arc` pixel cache.
///
/// Format is decided by content magic bytes, never by extension or MIME
/// header. JPEG, PNG, and WebP pass through unchanged; GIF (first frame) and
/// every other decodable raster format are normalized to PNG. Normalized and
/// pass-through bytes must still fit within `MAX_IMAGE_BYTES`.
///
/// This is CPU-bound pixel work; callers must run it inside `spawn_blocking`.
pub(crate) fn image_block_from_bytes(
    source: ImageSource,
    label: String,
    bytes: Vec<u8>,
) -> Result<ImageBlock> {
    if bytes.is_empty() {
        bail!("image source {source:?} for {label:?} is empty");
    }
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        bail!("image {label} exceeds the {MAX_IMAGE_BYTES} byte limit");
    }
    let (image, format) =
        decode_image(&bytes).with_context(|| format!("decoding image {label:?}"))?;
    if image.width() == 0 || image.height() == 0 {
        bail!("image {label:?} has zero dimensions");
    }
    let width = image.width();
    let height = image.height();
    let (encoded, media_type) = match format {
        image::ImageFormat::Jpeg => (bytes, ImageMediaType::Jpeg),
        image::ImageFormat::Png => (bytes, ImageMediaType::Png),
        image::ImageFormat::WebP => (bytes, ImageMediaType::Webp),
        _ => {
            let png =
                encode_png(&image).with_context(|| format!("normalizing {label:?} to PNG"))?;
            if png.len() as u64 > MAX_IMAGE_BYTES {
                bail!("normalized PNG for {label} exceeds the {MAX_IMAGE_BYTES} byte limit");
            }
            (png, ImageMediaType::Png)
        }
    };
    Ok(ImageBlock {
        source,
        label,
        media_type,
        width,
        height,
        bytes: Some(Arc::from(encoded)),
    })
}

fn encode_png(image: &image::DynamicImage) -> Result<Vec<u8>> {
    let mut output = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut output, image::ImageFormat::Png)
        .context("encoding PNG")?;
    Ok(output.into_inner())
}

/// Re-resolve a weak image source to live bytes, producing a fresh block.
///
/// - Attachment: bounded store read.
/// - LocalFile: canonical regular-file and size checks, then a bounded read.
/// - Url: the same web fetch helper the `read` tool uses, so the agent-side
///   URL policy matches the tool exactly (Nole downloads; the provider never
///   fetches the URL itself).
async fn resolve_image_source(
    source: &ImageSource,
    label: &str,
    attachments: &AttachmentStore,
    client: &Client,
) -> Result<ImageBlock> {
    match source {
        ImageSource::Attachment { uri } => {
            let uri = AttachmentUri::parse(uri)
                .with_context(|| format!("image source URI is not canonical: {uri}"))?;
            let store = attachments.clone();
            let source = source.clone();
            let label = label.to_string();
            tokio::task::spawn_blocking(move || {
                let bytes = store
                    .read_limited(uri.id(), MAX_IMAGE_BYTES)
                    .with_context(|| format!("reading image attachment {uri}"))?;
                image_block_from_bytes(source, label, bytes)
            })
            .await
            .context("joining attachment image read and decode")?
            .with_context(|| format!("decoding image attachment {uri}"))
        }
        ImageSource::LocalFile { path } => {
            let canonical = async_fs::canonicalize(path)
                .await
                .with_context(|| format!("resolving image file {}", path.display()))?;
            let metadata = async_fs::metadata(&canonical)
                .await
                .with_context(|| format!("reading image file {}", canonical.display()))?;
            if !metadata.is_file() {
                bail!(
                    "image source is not a regular file: {}",
                    canonical.display()
                );
            }
            if metadata.len() > MAX_IMAGE_BYTES {
                bail!(
                    "image file {} is {} bytes, exceeding the {MAX_IMAGE_BYTES} byte limit",
                    canonical.display(),
                    metadata.len()
                );
            }
            let path = canonical.clone();
            let label = label.to_string();
            tokio::task::spawn_blocking(move || {
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("reading image file {}", path.display()))?;
                image_block_from_bytes(ImageSource::LocalFile { path }, label, bytes)
            })
            .await
            .context("joining local image read and decode")?
            .with_context(|| format!("decoding image file {}", canonical.display()))
        }
        ImageSource::Url { url } => {
            let request_url = url.clone();
            let (bytes, final_url) = fetch_web_image_bytes(client, url).await?;
            let label = label.to_string();
            tokio::task::spawn_blocking(move || {
                image_block_from_bytes(ImageSource::Url { url: final_url }, label, bytes)
            })
            .await
            .context("joining web image decode")?
            .with_context(|| format!("web fetch failed during image_decode for {request_url}"))
        }
    }
}

/// Fetch an image URL with the same `read` tool boundary: transport phase
/// errors, HTTP status previews, redirect handling, and an 8 MiB body cap.
/// Returns the redirect-resolved URL used as the refreshed weak source.
async fn fetch_web_image_bytes(client: &Client, url: &str) -> Result<(Vec<u8>, String)> {
    let (response, final_url, _content_type) = fetch_web_response(client, url).await?;
    let bytes =
        crate::agent::tools::web::read_http_body_with_limit(response, "image", MAX_IMAGE_BYTES)
            .await
            .context("web fetch failed during response_body")?;
    Ok((bytes, final_url))
}

/// Materialize every unresolved image block in a conversation, filling the
/// in-process `Arc` cache from the live source so token counting, provider
/// retries, and the first request each re-read at most once. Fails before any
/// base64 allocation or provider HTTP call when the accumulated normalized
/// bytes for one request exceed `MAX_AGENT_IMAGE_REQUEST_BYTES`.
pub(crate) async fn prepare_provider_messages(
    messages: &mut [Message],
    attachments: &AttachmentStore,
    client: &Client,
) -> Result<()> {
    let mut running_bytes: u64 = 0;
    for message in messages.iter_mut() {
        for part in message.parts.iter_mut() {
            let MessagePart::Image(block) = part else {
                continue;
            };
            if block.bytes.is_none() {
                let label = block.label.clone();
                *block = resolve_image_source(&block.source, &label, attachments, client).await?;
            }
            let length = block.bytes.as_ref().map_or(0, |bytes| bytes.len() as u64);
            running_bytes = running_bytes.saturating_add(length);
            if running_bytes > MAX_AGENT_IMAGE_REQUEST_BYTES {
                bail!("image input exceeds the {MAX_AGENT_IMAGE_REQUEST_BYTES} byte request limit");
            }
        }
    }
    Ok(())
}

/// Byte ranges (start, end) of every Markdown image whose target is a strict
/// attachment URI, together with the alt text and canonical URI, in source
/// order. Code blocks, escaped text, HTML comments, ordinary links, and
/// remote/local image targets never produce these events.
fn attachment_image_spans(source: &str) -> Vec<(usize, usize, String, String)> {
    let Ok(document) = mbdown::parse(source) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    collect_attachment_image_spans(document.nodes(), &mut spans);
    spans.sort_by_key(|span| span.0);
    spans
}

fn collect_attachment_image_spans(
    nodes: &[Node<'_>],
    spans: &mut Vec<(usize, usize, String, String)>,
) {
    for node in nodes {
        match node {
            Node::Markdown(markdown) => {
                let offset = markdown.source_span().start;
                let events = markdown.events();
                let mut index = 0;
                while index < events.len() {
                    if let Event::Start(Container::Image { target, .. }) = &events[index].event {
                        let relative_end = events[index..]
                            .iter()
                            .position(|item| matches!(item.event, Event::End(ContainerEnd::Image)))
                            .map(|end_offset| index + end_offset);
                        if let Some(end_index) = relative_end {
                            if let Ok(uri) = AttachmentUri::parse(target.as_ref()) {
                                let start = offset + events[index].span.start;
                                let end = offset + events[end_index].span.end;
                                let alt = alt_text(&events[index + 1..end_index]);
                                spans.push((start, end, alt, uri.to_string()));
                            }
                            index = end_index + 1;
                            continue;
                        }
                    }
                    index += 1;
                }
            }
            Node::Box { children, .. }
            | Node::Center { children }
            | Node::Right { children }
            | Node::Indent { children, .. }
            | Node::Columns { children, .. }
            | Node::Column { children, .. } => collect_attachment_image_spans(children, spans),
        }
    }
}

fn alt_text(events: &[mbdown::SpannedEvent<'_>]) -> String {
    events
        .iter()
        .filter_map(|item| match &item.event {
            Event::Text(text) => Some(text.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Parse a user prompt into an ordered `Message` of Text and Image parts.
///
/// Only `![alt](nole://attachment/<uuid>)` embeds (strict `AttachmentUri`)
/// become image parts; everything else stays text, and remote/local images in
/// Markdown remain text for the model to read on its own. Attachment reads
/// run in a blocking task, and the accumulated normalized bytes are capped at
/// `MAX_AGENT_IMAGE_REQUEST_BYTES` before any checkpoint.
pub(crate) async fn parse_user_message(
    text: String,
    store: &AttachmentStore,
    supports_images: bool,
) -> Result<Message> {
    let spans = attachment_image_spans(&text);
    if spans.is_empty() {
        return Ok(Message::user(text));
    }
    if !supports_images {
        bail!("{DISABLED_IMAGE_ERROR}");
    }
    let mut parts = Vec::new();
    let mut cursor = 0usize;
    let mut running_bytes: u64 = 0;
    for (start, end, alt, uri) in spans {
        if start > cursor {
            parts.push(MessagePart::Text {
                text: text[cursor..start].to_string(),
            });
        }
        let uri = AttachmentUri::parse(&uri)?;
        let store = store.clone();
        let source_uri = uri.to_string();
        let block = tokio::task::spawn_blocking(move || {
            let metadata = store.metadata(uri.id())?;
            let bytes = store.read_limited(uri.id(), MAX_IMAGE_BYTES)?;
            let label = if alt.trim().is_empty() {
                metadata.display_name
            } else {
                alt
            };
            image_block_from_bytes(ImageSource::Attachment { uri: source_uri }, label, bytes)
        })
        .await
        .context("joining embedded attachment image read and decode")?
        .with_context(|| format!("reading embedded attachment {uri}"))?;
        let normalized_bytes = block
            .bytes
            .as_ref()
            .expect("newly decoded image blocks carry bytes")
            .len() as u64;
        running_bytes = running_bytes.saturating_add(normalized_bytes);
        if running_bytes > MAX_AGENT_IMAGE_REQUEST_BYTES {
            bail!("image input exceeds the {MAX_AGENT_IMAGE_REQUEST_BYTES} byte request limit");
        }
        parts.push(MessagePart::Image(block));
        cursor = end;
    }
    if cursor < text.len() {
        parts.push(MessagePart::Text {
            text: text[cursor..].to_string(),
        });
    }
    Ok(Message::user_parts(parts))
}

/// Append already-parsed parts to the last user message (or start a new user
/// message), preserving exact ordering with any preceding parts.
pub(crate) fn append_user_parts(messages: &mut Vec<Message>, parts: Vec<MessagePart>) {
    if parts.is_empty() {
        return;
    }
    if let Some(message) = messages
        .last_mut()
        .filter(|message| message.role == MessageRole::User)
    {
        message.parts.extend(parts);
    } else {
        messages.push(Message::user_parts(parts));
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::attachment::AttachmentStore;
    use crate::image_data::detect_image_format;
    use crate::storage::ATTACHMENTS_DIR;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::new_rgb8(width, height);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn image_block_from_bytes_passes_through_png_with_dimensions() {
        let block = image_block_from_bytes(
            ImageSource::Url {
                url: "u".to_string(),
            },
            "x.png".into(),
            png_bytes(8, 4),
        )
        .unwrap();
        assert_eq!(block.width, 8);
        assert_eq!(block.height, 4);
        assert_eq!(block.media_type, ImageMediaType::Png);
        assert!(block.bytes.is_some());
    }

    fn gif_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            let frame =
                image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
            encoder.encode_frame(image::Frame::new(frame)).unwrap();
        }
        bytes
    }

    #[test]
    fn gif_is_normalized_to_png_with_first_frame_dimensions() {
        let block = image_block_from_bytes(
            ImageSource::Attachment {
                uri: "nole://attachment/00000000-0000-4000-8000-000000000000".to_string(),
            },
            "anim.gif".into(),
            gif_bytes(4, 2),
        )
        .unwrap();
        assert_eq!(block.media_type, ImageMediaType::Png);
        assert_eq!((block.width, block.height), (4, 2));
        assert!(
            detect_image_format(&block.bytes.as_ref().unwrap()[..16])
                == Some(image::ImageFormat::Png)
        );
    }

    #[test]
    fn empty_and_over_limit_bytes_fail() {
        assert!(image_block_from_bytes(
            ImageSource::Attachment {
                uri: "u".to_string()
            },
            "empty".into(),
            Vec::new()
        )
        .is_err());
        let oversized = vec![0u8; (MAX_IMAGE_BYTES as usize) + 1];
        assert!(image_block_from_bytes(
            ImageSource::Attachment {
                uri: "u".to_string()
            },
            "big".into(),
            oversized
        )
        .is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parse_user_message_splits_text_around_attachment_images() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let uri = store
            .import_bytes(&png_bytes(4, 4), Some("scan.png"))
            .unwrap()
            .uri()
            .to_string();
        let text = format!("before ![scan]({uri}) after");
        let message = parse_user_message(text, &store, true).await.unwrap();
        assert_eq!(message.parts.len(), 3);
        assert_eq!(
            message.parts[0],
            MessagePart::Text {
                text: "before ".to_string()
            }
        );
        match &message.parts[1] {
            MessagePart::Image(block) => {
                assert_eq!(block.label, "scan");
                assert_eq!(block.width, 4);
            }
            _ => panic!("expected image part"),
        }
        assert_eq!(
            message.parts[2],
            MessagePart::Text {
                text: " after".to_string()
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parse_user_message_keeps_remote_images_as_text() {
        let store = AttachmentStore::new(tempfile::tempdir().unwrap().path().join(ATTACHMENTS_DIR));
        let text = "see ![diagram](https://example.com/a.png) now".to_string();
        let message = parse_user_message(text, &store, true).await.unwrap();
        assert_eq!(message.parts.len(), 1);
        assert_eq!(
            message.parts[0],
            MessagePart::Text {
                text: "see ![diagram](https://example.com/a.png) now".to_string()
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parse_user_message_keeps_invalid_attachment_images_as_text() {
        let store = AttachmentStore::new(tempfile::tempdir().unwrap().path().join(ATTACHMENTS_DIR));
        let text = "see ![diagram](nole://attachment/not-a-canonical-uuid) now".to_string();
        let message = parse_user_message(text.clone(), &store, false)
            .await
            .unwrap();
        assert_eq!(message, Message::user(text));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn image_markdown_is_parsed_into_ordered_user_parts() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let uri = store
            .import_bytes(&png_bytes(4, 4), Some("scan.png"))
            .unwrap()
            .uri()
            .to_string();
        let message = parse_user_message(format!("before ![scan]({uri}) after"), &store, true)
            .await
            .unwrap();
        assert_eq!(message.parts.len(), 3);
        assert_eq!(
            message.parts[0],
            MessagePart::Text {
                text: "before ".to_string()
            }
        );
        match &message.parts[1] {
            MessagePart::Image(block) => {
                assert_eq!(block.label, "scan");
                assert_eq!(block.width, 4);
                assert!(block.bytes.is_some());
            }
            _ => panic!("expected image part"),
        }
        assert_eq!(
            message.parts[2],
            MessagePart::Text {
                text: " after".to_string()
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parse_user_message_bails_when_disabled_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let uri = store
            .import_bytes(&png_bytes(2, 2), Some("scan.png"))
            .unwrap()
            .uri()
            .to_string();
        let text = format!("![s]({uri})");
        let error = parse_user_message(text, &store, false).await.unwrap_err();
        assert_eq!(error.to_string(), DISABLED_IMAGE_ERROR);
    }
}
