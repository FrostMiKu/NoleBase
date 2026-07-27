//! Asynchronous Markdown image loading and terminal-protocol rendering.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Cursor, Read};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use image::ImageReader;
use ratatui::layout::{Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};
use ratatui_image::Resize;

use crate::theme::catppuccin as ctp;

const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4096;
const MAX_IMAGE_ALLOC: u64 = 64 * 1024 * 1024;
const MAX_CACHED_IMAGES: usize = 64;
const MAX_IMAGE_REDIRECTS: usize = 5;
const IMAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ImageKey {
    source: String,
    width: u16,
    height: u16,
    picker_generation: u64,
}

#[derive(Clone, Debug)]
enum ResolvedSource {
    Local(PathBuf),
    Remote(String),
}

enum ImageState {
    Loading,
    Ready(SlicedProtocol),
    Failed(String),
}

struct LoadResult {
    key: ImageKey,
    result: std::result::Result<SlicedProtocol, String>,
}

pub(crate) struct ImageService {
    root: PathBuf,
    picker: Picker,
    picker_generation: u64,
    states: HashMap<ImageKey, ImageState>,
    order: VecDeque<ImageKey>,
    sender: Sender<LoadResult>,
    receiver: Receiver<LoadResult>,
}

impl ImageService {
    pub(crate) fn new(root: &Path) -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            root: fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            picker: Picker::halfblocks(),
            picker_generation: 0,
            states: HashMap::new(),
            order: VecDeque::new(),
            sender,
            receiver,
        }
    }

    pub(crate) fn set_picker(&mut self, picker: Picker) {
        self.picker = picker;
        self.picker_generation = self.picker_generation.wrapping_add(1);
        self.states.clear();
        self.order.clear();
    }

    pub(crate) fn render(
        &mut self,
        frame: &mut Frame,
        placements: &[mbtui::ImagePlacement],
        viewport: Rect,
        scroll: usize,
        base_dir: &Path,
    ) {
        self.collect_results();
        if viewport.width == 0 || viewport.height == 0 {
            return;
        }
        for placement in placements {
            if placement.width == 0 || placement.height == 0 {
                continue;
            }
            let top = placement.row as i64 - scroll as i64;
            let bottom = top.saturating_add(placement.height as i64);
            if bottom <= 0 || top >= i64::from(viewport.height) {
                continue;
            }
            let width = u16::try_from(placement.width).unwrap_or(u16::MAX).min(
                viewport
                    .width
                    .saturating_sub(u16::try_from(placement.column).unwrap_or(u16::MAX)),
            );
            let height = u16::try_from(placement.height).unwrap_or(u16::MAX);
            if width == 0 || height == 0 {
                continue;
            }
            let (key, source) = match self.resolve(&placement.source, base_dir, width, height) {
                Ok(resolved) => resolved,
                Err(error) => {
                    draw_image_label(frame, viewport, top, placement, &error.to_string(), true);
                    continue;
                }
            };
            self.request_if_needed(key.clone(), source);
            match self.states.get(&key) {
                Some(ImageState::Ready(protocol)) => {
                    let size = protocol.size();
                    let x = placement
                        .column
                        .saturating_add(placement.width.saturating_sub(size.width as usize) / 2);
                    let y = top.saturating_add(
                        placement.height.saturating_sub(size.height as usize) as i64 / 2,
                    );
                    let position = SignedPosition {
                        x: clamp_i16(x as i64),
                        y: clamp_i16(y),
                    };
                    frame.render_widget(SlicedImage::new(protocol, position), viewport);
                }
                Some(ImageState::Failed(error)) => {
                    draw_image_label(frame, viewport, top, placement, error, true);
                }
                Some(ImageState::Loading) | None => {
                    draw_image_label(frame, viewport, top, placement, "loading", false);
                }
            }
        }
    }

    fn resolve(
        &self,
        source: &str,
        base_dir: &Path,
        width: u16,
        height: u16,
    ) -> Result<(ImageKey, ResolvedSource)> {
        if let Ok(url) = reqwest::Url::parse(source) {
            let url = validate_remote_image_url(url)?;
            return Ok((
                ImageKey {
                    source: url.to_string(),
                    width,
                    height,
                    picker_generation: self.picker_generation,
                },
                ResolvedSource::Remote(url.to_string()),
            ));
        }

        let requested = Path::new(source);
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            base_dir.join(requested)
        };
        let path = fs::canonicalize(&candidate)
            .with_context(|| format!("image not found: {}", candidate.display()))?;
        if !path.starts_with(&self.root) {
            bail!("local image is outside the Nole root");
        }
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES {
            bail!("image must be a regular file no larger than 8 MB");
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        Ok((
            ImageKey {
                source: format!("{}:{}:{modified}", path.display(), metadata.len()),
                width,
                height,
                picker_generation: self.picker_generation,
            },
            ResolvedSource::Local(path),
        ))
    }

    fn request_if_needed(&mut self, key: ImageKey, source: ResolvedSource) {
        if self.states.contains_key(&key) {
            return;
        }
        self.states.insert(key.clone(), ImageState::Loading);
        self.order.push_back(key.clone());
        self.evict_old_entries();
        let sender = self.sender.clone();
        let picker = self.picker.clone();
        std::thread::spawn(move || {
            let result = load_protocol(&picker, &source, Size::new(key.width, key.height))
                .map_err(|error| error.to_string());
            let _ = sender.send(LoadResult { key, result });
        });
    }

    fn collect_results(&mut self) {
        for loaded in self.receiver.try_iter() {
            if let std::collections::hash_map::Entry::Occupied(mut entry) =
                self.states.entry(loaded.key)
            {
                entry.insert(match loaded.result {
                    Ok(protocol) => ImageState::Ready(protocol),
                    Err(error) => ImageState::Failed(error),
                });
            }
        }
    }

    fn evict_old_entries(&mut self) {
        while self.states.len() > MAX_CACHED_IMAGES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.states.remove(&oldest);
        }
    }
}

fn load_protocol(picker: &Picker, source: &ResolvedSource, size: Size) -> Result<SlicedProtocol> {
    let bytes = match source {
        ResolvedSource::Local(path) => {
            fs::read(path).with_context(|| format!("reading image {}", path.display()))?
        }
        ResolvedSource::Remote(url) => download_image(url)?,
    };
    let image = decode_image(bytes)?;
    SlicedProtocol::new_with_resize(picker, image, size, Resize::Fit(None))
        .context("encoding image for the terminal")
}

fn download_image(url: &str) -> Result<Vec<u8>> {
    let mut current =
        validate_remote_image_url(reqwest::Url::parse(url).context("parsing image URL")?)?;
    let started = Instant::now();
    for redirects in 0..=MAX_IMAGE_REDIRECTS {
        let remaining = IMAGE_DOWNLOAD_TIMEOUT
            .checked_sub(started.elapsed())
            .context("image download timed out")?;
        let response = send_remote_image_request(&current, remaining)?;
        if is_image_redirect(response.status()) {
            if redirects == MAX_IMAGE_REDIRECTS {
                bail!("remote image exceeded 5 redirects");
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .context("image redirect has no Location header")?
                .to_str()
                .context("image redirect Location is not valid text")?;
            current = redirected_image_url(&current, location)?;
            continue;
        }
        if !response.status().is_success() {
            bail!("image server returned HTTP {}", response.status());
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_IMAGE_BYTES)
        {
            bail!("remote image exceeds 8 MB");
        }
        let mut bytes = Vec::new();
        response.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            bail!("remote image exceeds 8 MB");
        }
        return Ok(bytes);
    }
    unreachable!("the redirect loop always returns or reports its limit")
}

fn send_remote_image_request(
    url: &reqwest::Url,
    timeout: Duration,
) -> Result<reqwest::blocking::Response> {
    let host = url.host_str().context("image URL has no host")?;
    let port = url
        .port_or_known_default()
        .context("image URL has no known port")?;
    let addresses = (host, port)
        .to_socket_addrs()
        .context("resolving image host")?
        .collect::<Vec<_>>();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        bail!("remote image host resolves to a non-public address");
    }
    let pinned: SocketAddr = addresses[0];
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, pinned)
        .user_agent(concat!("nole/", env!("CARGO_PKG_VERSION")))
        .build()?;
    client.get(url.clone()).send().context("downloading image")
}

fn redirected_image_url(current: &reqwest::Url, location: &str) -> Result<reqwest::Url> {
    let next = current
        .join(location)
        .context("resolving image redirect Location")?;
    validate_remote_image_url(next)
}

fn validate_remote_image_url(url: reqwest::Url) -> Result<reqwest::Url> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported image URL scheme");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("image URLs cannot contain credentials");
    }
    url.host_str().context("image URL has no host")?;
    url.port_or_known_default()
        .context("image URL has no known port")?;
    Ok(url)
}

fn is_image_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            !address.is_private()
                && !address.is_loopback()
                && !address.is_link_local()
                && !address.is_broadcast()
                && !address.is_documentation()
                && !address.is_unspecified()
                && !address.is_multicast()
                && octets[0] != 0
                && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
                && !(octets[0] == 198 && matches!(octets[1], 18 | 19))
                && octets[0] < 240
        }
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            let segments = address.segments();
            !address.is_loopback()
                && !address.is_unspecified()
                && !address.is_multicast()
                && segments[0] & 0xfe00 != 0xfc00
                && segments[0] & 0xffc0 != 0xfe80
                && segments[..2] != [0x2001, 0x0db8]
        }
    }
}

fn decode_image(bytes: Vec<u8>) -> Result<image::DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("detecting image format")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC);
    reader.limits(limits);
    reader.decode().context("decoding image")
}

fn draw_image_label(
    frame: &mut Frame,
    viewport: Rect,
    top: i64,
    placement: &mbtui::ImagePlacement,
    detail: &str,
    failed: bool,
) {
    let visible_y = top.max(0);
    if visible_y >= i64::from(viewport.height) {
        return;
    }
    let column = u16::try_from(placement.column).unwrap_or(u16::MAX);
    if column >= viewport.width {
        return;
    }
    let width = u16::try_from(placement.width)
        .unwrap_or(u16::MAX)
        .min(viewport.width - column);
    let alt = if placement.alt.is_empty() {
        placement.source.as_str()
    } else {
        placement.alt.as_str()
    };
    let text = if failed {
        format!("[image] {alt} ({detail})")
    } else {
        format!("[image] {alt}")
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default()
                .fg(if failed { ctp::PINK } else { ctp::OVERLAY_0 })
                .add_modifier(Modifier::ITALIC),
        ))),
        Rect::new(viewport.x + column, viewport.y + visible_y as u16, width, 1),
    );
}

fn clamp_i16(value: i64) -> i16 {
    value.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn local_images_are_confined_to_the_nole_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(root.path().join("inside.png"), b"png").unwrap();
        fs::write(outside.path().join("outside.png"), b"png").unwrap();
        let service = ImageService::new(root.path());

        assert!(service.resolve("inside.png", root.path(), 20, 10).is_ok());
        assert!(service
            .resolve(
                outside.path().join("outside.png").to_str().unwrap(),
                root.path(),
                20,
                10,
            )
            .is_err());
    }

    #[test]
    fn decoded_images_honor_dimension_limits() {
        let image = image::DynamicImage::new_rgb8(8, 4);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let decoded = decode_image(bytes.into_inner()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 4));
    }

    #[test]
    fn remote_keys_include_the_requested_terminal_size() {
        let root = tempfile::tempdir().unwrap();
        let service = ImageService::new(root.path());
        let (small, _) = service
            .resolve("https://example.com/image.png", root.path(), 20, 10)
            .unwrap();
        let (wide, _) = service
            .resolve("https://example.com/image.png", root.path(), 40, 10)
            .unwrap();
        assert_ne!(small, wide);
    }

    #[test]
    fn remote_images_reject_non_public_network_ranges() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.1.1",
            "192.168.1.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public_ip(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn remote_image_redirects_resolve_relative_urls_and_reject_unsafe_targets() {
        let current = reqwest::Url::parse("https://images.example/a/source.png").unwrap();
        assert_eq!(
            redirected_image_url(&current, "../cdn/final.png")
                .unwrap()
                .as_str(),
            "https://images.example/cdn/final.png"
        );
        assert_eq!(
            redirected_image_url(&current, "https://cdn.example/final.png")
                .unwrap()
                .as_str(),
            "https://cdn.example/final.png"
        );
        assert!(redirected_image_url(&current, "file:///etc/passwd").is_err());
        assert!(redirected_image_url(&current, "https://user@cdn.example/image.png").is_err());
        for status in [301, 302, 303, 307, 308] {
            assert!(is_image_redirect(
                reqwest::StatusCode::from_u16(status).unwrap()
            ));
        }
        assert!(!is_image_redirect(reqwest::StatusCode::MULTIPLE_CHOICES));
    }

    #[test]
    fn halfblock_fallback_writes_image_cells_to_the_ratatui_buffer() {
        let mut source = image::RgbaImage::new(4, 4);
        for (x, y, pixel) in source.enumerate_pixels_mut() {
            *pixel = if (x + y) % 2 == 0 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 255, 0, 255])
            };
        }
        let protocol = SlicedProtocol::new_with_resize(
            &Picker::halfblocks(),
            image::DynamicImage::ImageRgba8(source),
            Size::new(4, 2),
            Resize::Fit(None),
        )
        .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(4, 2)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    SlicedImage::new(&protocol, SignedPosition::from((0, 0))),
                    frame.area(),
                );
            })
            .unwrap();

        assert!(terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() != " "));
    }
}
