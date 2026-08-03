//! Asynchronous Markdown image loading and terminal-protocol rendering.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
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
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::attachment::{AttachmentStore, AttachmentUri};
use crate::storage::ATTACHMENTS_DIR;
use crate::theme::Theme;

const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4096;
const MAX_IMAGE_ALLOC: u64 = 64 * 1024 * 1024;
const MAX_CACHED_IMAGES: usize = 64;
const MAX_CACHED_REMOTE_SOURCES: usize = 16;
const MAX_IMAGE_REDIRECTS: usize = 5;
const MAX_IMAGE_DOWNLOAD_ATTEMPTS: usize = 3;
const IMAGE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);
const IMAGE_FAILURE_CACHE_TTL: Duration = Duration::from_secs(5);
const IMAGE_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(200), Duration::from_millis(600)];

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
    /// A mutable attachment; bytes are read through the store, so
    /// physical object paths never leave [`AttachmentStore`].
    Attachment(AttachmentUri),
}

enum ImageState {
    Loading,
    Ready(SlicedProtocol),
    Failed { error: String, retry_at: Instant },
}

enum RemoteDownloadState {
    Empty,
    Loading,
    Ready(Arc<[u8]>),
    Failed { error: String, retry_at: Instant },
}

struct RemoteDownload {
    state: Mutex<RemoteDownloadState>,
    ready: Condvar,
}

impl RemoteDownload {
    fn new() -> Self {
        Self {
            state: Mutex::new(RemoteDownloadState::Empty),
            ready: Condvar::new(),
        }
    }
}

#[derive(Default)]
struct RemoteSourceCache {
    entries: HashMap<String, Arc<RemoteDownload>>,
    order: VecDeque<String>,
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
    remote_sources: Arc<Mutex<RemoteSourceCache>>,
    attachments: AttachmentStore,
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
            remote_sources: Arc::new(Mutex::new(RemoteSourceCache::default())),
            attachments: AttachmentStore::new(root.join(ATTACHMENTS_DIR)),
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
        theme: Theme,
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
                    draw_image_placeholder(
                        frame,
                        viewport,
                        top,
                        placement,
                        &error.to_string(),
                        true,
                        theme,
                    );
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
                Some(ImageState::Failed { error, .. }) => {
                    draw_image_placeholder(frame, viewport, top, placement, error, true, theme);
                }
                Some(ImageState::Loading) | None => {
                    draw_image_placeholder(
                        frame,
                        viewport,
                        top,
                        placement,
                        "Loading...",
                        false,
                        theme,
                    );
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
        if AttachmentUri::is_attachment_uri(source) {
            let uri = AttachmentUri::parse(source)?;
            let metadata = self
                .attachments
                .metadata(uri.id())
                .with_context(|| format!("reading attachment {uri}"))?;
            if metadata.size > MAX_IMAGE_BYTES {
                bail!("attachment image exceeds 8 MB");
            }
            return Ok((
                ImageKey {
                    source: uri.to_string(),
                    width,
                    height,
                    picker_generation: self.picker_generation,
                },
                ResolvedSource::Attachment(uri),
            ));
        }
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
        match self.states.get(&key) {
            Some(ImageState::Failed { retry_at, .. }) if Instant::now() >= *retry_at => {}
            Some(_) => return,
            None => {}
        }
        self.states.insert(key.clone(), ImageState::Loading);
        self.order.retain(|cached| cached != &key);
        self.order.push_back(key.clone());
        self.evict_old_entries();
        let sender = self.sender.clone();
        let picker = self.picker.clone();
        let remote_sources = Arc::clone(&self.remote_sources);
        let attachments = self.attachments.clone();
        std::thread::spawn(move || {
            let result = load_protocol(
                &picker,
                &source,
                Size::new(key.width, key.height),
                &remote_sources,
                &attachments,
            )
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
                    Err(error) => ImageState::Failed {
                        error,
                        retry_at: Instant::now() + IMAGE_FAILURE_CACHE_TTL,
                    },
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

fn load_protocol(
    picker: &Picker,
    source: &ResolvedSource,
    size: Size,
    remote_sources: &Arc<Mutex<RemoteSourceCache>>,
    attachments: &AttachmentStore,
) -> Result<SlicedProtocol> {
    let bytes = match source {
        ResolvedSource::Local(path) => Arc::<[u8]>::from(
            fs::read(path).with_context(|| format!("reading image {}", path.display()))?,
        ),
        ResolvedSource::Remote(url) => remote_image_bytes(remote_sources, url)?,
        ResolvedSource::Attachment(uri) => Arc::<[u8]>::from(
            attachments
                .read_limited(uri.id(), MAX_IMAGE_BYTES)
                .with_context(|| format!("reading attachment {uri}"))?,
        ),
    };
    let image = decode_image(bytes.as_ref())?;
    SlicedProtocol::new_with_resize(picker, image, size, Resize::Fit(None))
        .context("encoding image for the terminal")
}

fn remote_image_bytes(cache: &Arc<Mutex<RemoteSourceCache>>, url: &str) -> Result<Arc<[u8]>> {
    let download = {
        let mut cache = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(download) = cache.entries.get(url) {
            Arc::clone(download)
        } else {
            let download = Arc::new(RemoteDownload::new());
            cache.entries.insert(url.to_string(), Arc::clone(&download));
            cache.order.push_back(url.to_string());
            while cache.entries.len() > MAX_CACHED_REMOTE_SOURCES {
                let Some(oldest) = cache.order.pop_front() else {
                    break;
                };
                cache.entries.remove(&oldest);
            }
            download
        }
    };

    loop {
        let mut state = download
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &*state {
            RemoteDownloadState::Ready(bytes) => return Ok(Arc::clone(bytes)),
            RemoteDownloadState::Failed { error, retry_at } if Instant::now() < *retry_at => {
                bail!("{error}");
            }
            RemoteDownloadState::Empty | RemoteDownloadState::Failed { .. } => {
                *state = RemoteDownloadState::Loading;
                drop(state);

                let result = download_image(url).map(Arc::<[u8]>::from);
                let mut state = download
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                match result {
                    Ok(bytes) => {
                        *state = RemoteDownloadState::Ready(Arc::clone(&bytes));
                        download.ready.notify_all();
                        return Ok(bytes);
                    }
                    Err(error) => {
                        let error = error.to_string();
                        *state = RemoteDownloadState::Failed {
                            error: error.clone(),
                            retry_at: Instant::now() + IMAGE_FAILURE_CACHE_TTL,
                        };
                        download.ready.notify_all();
                        bail!("{error}");
                    }
                }
            }
            RemoteDownloadState::Loading => {
                drop(
                    download
                        .ready
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
            }
        }
    }
}

enum DownloadError {
    Retryable(anyhow::Error),
    Permanent(anyhow::Error),
}

fn download_image(url: &str) -> Result<Vec<u8>> {
    let url = validate_remote_image_url(reqwest::Url::parse(url).context("parsing image URL")?)?;
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("nole/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let deadline = Instant::now() + IMAGE_DOWNLOAD_TIMEOUT;
    let mut last_error = None;

    for attempt in 0..MAX_IMAGE_DOWNLOAD_ATTEMPTS {
        match download_image_attempt(&client, &url, deadline) {
            Ok(bytes) => return Ok(bytes),
            Err(DownloadError::Permanent(error)) => return Err(error),
            Err(DownloadError::Retryable(error)) => last_error = Some(error),
        }
        let Some(delay) = IMAGE_RETRY_DELAYS.get(attempt).copied() else {
            break;
        };
        if Instant::now() + delay >= deadline {
            break;
        }
        std::thread::sleep(delay);
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("image download timed out")))
}

fn download_image_attempt(
    client: &reqwest::blocking::Client,
    url: &reqwest::Url,
    deadline: Instant,
) -> std::result::Result<Vec<u8>, DownloadError> {
    let mut current = url.clone();
    for redirects in 0..=MAX_IMAGE_REDIRECTS {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| DownloadError::Retryable(anyhow::anyhow!("image download timed out")))?;
        let response = send_remote_image_request(client, &current, remaining)
            .map_err(DownloadError::Retryable)?;
        if is_image_redirect(response.status()) {
            if redirects == MAX_IMAGE_REDIRECTS {
                return Err(DownloadError::Permanent(anyhow::anyhow!(
                    "remote image exceeded 5 redirects"
                )));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .context("image redirect has no Location header")
                .map_err(DownloadError::Permanent)?
                .to_str()
                .context("image redirect Location is not valid text")
                .map_err(DownloadError::Permanent)?;
            current = redirected_image_url(&current, location).map_err(DownloadError::Permanent)?;
            continue;
        }
        if !response.status().is_success() {
            let error = anyhow::anyhow!("image server returned HTTP {}", response.status());
            return Err(if is_retryable_image_status(response.status()) {
                DownloadError::Retryable(error)
            } else {
                DownloadError::Permanent(error)
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_IMAGE_BYTES)
        {
            return Err(DownloadError::Permanent(anyhow::anyhow!(
                "remote image exceeds 8 MB"
            )));
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_IMAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("reading image response")
            .map_err(DownloadError::Retryable)?;
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(DownloadError::Permanent(anyhow::anyhow!(
                "remote image exceeds 8 MB"
            )));
        }
        return Ok(bytes);
    }
    unreachable!("the redirect loop always returns or reports its limit")
}

fn send_remote_image_request(
    client: &reqwest::blocking::Client,
    url: &reqwest::Url,
    timeout: Duration,
) -> Result<reqwest::blocking::Response> {
    client
        .get(url.clone())
        .timeout(timeout)
        .send()
        .context("downloading image")
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

fn is_retryable_image_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429) || status.is_server_error()
}

fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage> {
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

fn draw_image_placeholder(
    frame: &mut Frame,
    viewport: Rect,
    top: i64,
    placement: &mbtui::ImagePlacement,
    detail: &str,
    failed: bool,
    theme: Theme,
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
    let hidden_rows = top.saturating_neg().max(0) as usize;
    let visible_height = placement
        .height
        .saturating_sub(hidden_rows)
        .min(usize::from(viewport.height) - visible_y as usize);
    if width == 0 || visible_height == 0 {
        return;
    }
    let alt = if placement.alt.is_empty() {
        placement.source.as_str()
    } else {
        placement.alt.as_str()
    };
    let detail = if failed {
        format!("Failed: {detail}")
    } else {
        detail.to_string()
    };
    let lines = image_placeholder_lines(
        usize::from(width),
        placement.height,
        hidden_rows,
        visible_height,
        alt,
        &detail,
        failed,
        theme,
    );
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(
            viewport.x + column,
            viewport.y + visible_y as u16,
            width,
            visible_height as u16,
        ),
    );
}

#[allow(clippy::too_many_arguments)]
fn image_placeholder_lines(
    width: usize,
    height: usize,
    first_row: usize,
    visible_height: usize,
    alt: &str,
    detail: &str,
    failed: bool,
    theme: Theme,
) -> Vec<Line<'static>> {
    let border_style = Style::default()
        .fg(theme.ui_border_subtle)
        .bg(theme.surface_panel);
    let alt_style = Style::default()
        .fg(theme.text_secondary)
        .bg(theme.surface_panel)
        .add_modifier(Modifier::BOLD);
    let detail_style = Style::default()
        .fg(if failed {
            theme.ui_error
        } else {
            theme.text_muted
        })
        .bg(theme.surface_panel)
        .add_modifier(Modifier::ITALIC);
    let alt_row = height.saturating_sub(1) / 2;
    let detail_row = (alt_row + 1).min(height.saturating_sub(2));

    (first_row..first_row.saturating_add(visible_height))
        .map(|row| {
            if width < 2 {
                return Line::from(Span::styled(" ".repeat(width), border_style));
            }
            if row == 0 {
                return Line::from(Span::styled(
                    format!("┌{}┐", "─".repeat(width.saturating_sub(2))),
                    border_style,
                ));
            }
            if row + 1 == height {
                return Line::from(Span::styled(
                    format!("└{}┘", "─".repeat(width.saturating_sub(2))),
                    border_style,
                ));
            }
            let (content, content_style) = if row == alt_row {
                (alt, alt_style)
            } else if row == detail_row {
                (detail, detail_style)
            } else {
                ("", border_style)
            };
            placeholder_content_line(width, content, border_style, content_style)
        })
        .collect()
}

fn placeholder_content_line(
    width: usize,
    content: &str,
    border_style: Style,
    content_style: Style,
) -> Line<'static> {
    let inner_width = width.saturating_sub(2);
    let content = truncate_to_width(content, inner_width);
    let content_width = UnicodeWidthStr::width(content.as_str());
    let left = inner_width.saturating_sub(content_width) / 2;
    let right = inner_width.saturating_sub(content_width + left);
    Line::from(vec![
        Span::styled(format!("│{}", " ".repeat(left)), border_style),
        Span::styled(content, content_style),
        Span::styled(format!("{}│", " ".repeat(right)), border_style),
    ])
}

fn truncate_to_width(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.to_string();
    }
    let suffix = "...";
    let target = width.saturating_sub(suffix.len());
    let mut output = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > target {
            break;
        }
        output.push(character);
        used += character_width;
    }
    if width >= suffix.len() {
        output.push_str(suffix);
    }
    output
}

fn clamp_i16(value: i64) -> i16 {
    value.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn png_bytes() -> Vec<u8> {
        let image = image::DynamicImage::new_rgb8(2, 2);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn read_request(stream: &TcpStream) {
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
    }

    fn write_response(mut stream: TcpStream, status: &str, body: &[u8]) {
        read_request(&stream);
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

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
    fn attachment_image_uris_resolve_through_the_store() {
        let root = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(root.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let image = image::DynamicImage::new_rgb8(2, 2);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let png = bytes.into_inner();
        let uri = store.import_bytes(&png, Some("photo.png")).unwrap().uri();
        let service = ImageService::new(root.path());

        let (key, source) = service
            .resolve(&uri.to_string(), root.path(), 20, 10)
            .unwrap();
        assert_eq!(key.source, uri.to_string());
        assert!(matches!(source, ResolvedSource::Attachment(resolved) if resolved == uri));
    }

    #[test]
    fn attachment_image_protocol_loads_bytes_from_the_store() {
        let root = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(root.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let image = image::DynamicImage::new_rgb8(4, 4);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let png = bytes.into_inner();
        let uri = store.import_bytes(&png, Some("photo.png")).unwrap().uri();
        let cache = Arc::new(Mutex::new(RemoteSourceCache::default()));

        let protocol = load_protocol(
            &Picker::halfblocks(),
            &ResolvedSource::Attachment(uri),
            Size::new(4, 2),
            &cache,
            &store,
        )
        .unwrap();
        assert!(protocol.size().width > 0 && protocol.size().height > 0);
    }

    #[test]
    fn malformed_or_absent_attachment_image_uris_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let service = ImageService::new(root.path());
        assert!(service
            .resolve("nole://attachment/not-a-uuid", root.path(), 20, 10)
            .is_err());
        let missing = format!(
            "nole://attachment/{}",
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert!(service.resolve(&missing, root.path(), 20, 10).is_err());
    }

    #[test]
    fn decoded_images_honor_dimension_limits() {
        let image = image::DynamicImage::new_rgb8(8, 4);
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let decoded = decode_image(&bytes.into_inner()).unwrap();
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
    fn remote_image_urls_allow_private_and_loopback_hosts() {
        for url in [
            "http://127.0.0.1/image.png",
            "http://10.0.0.1/image.png",
            "http://192.168.1.20/image.png",
            "http://[::1]/image.png",
            "http://[fd00::1]/image.png",
        ] {
            assert!(
                validate_remote_image_url(reqwest::Url::parse(url).unwrap()).is_ok(),
                "{url}"
            );
        }
    }

    #[test]
    fn remote_download_retries_transient_http_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = png_bytes();
        let server = std::thread::spawn(move || {
            write_response(
                listener.accept().unwrap().0,
                "503 Service Unavailable",
                b"busy",
            );
            write_response(listener.accept().unwrap().0, "200 OK", &body);
        });

        let downloaded = download_image(&format!("http://{address}/image.png")).unwrap();
        assert!(decode_image(&downloaded).is_ok());
        server.join().unwrap();
    }

    #[test]
    fn remote_download_does_not_retry_permanent_http_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = Arc::clone(&requests);
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server_requests.fetch_add(1, Ordering::SeqCst);
            write_response(stream, "404 Not Found", b"missing");
        });

        let error = download_image(&format!("http://{address}/missing.png")).unwrap_err();
        assert!(error.to_string().contains("HTTP 404"));
        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_remote_requests_share_one_download() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = Arc::clone(&requests);
        let expected = png_bytes();
        let response = expected.clone();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            server_requests.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            write_response(stream, "200 OK", &response);
        });

        let cache = Arc::new(Mutex::new(RemoteSourceCache::default()));
        let url = format!("http://{address}/shared.png");
        let first_cache = Arc::clone(&cache);
        let first_url = url.clone();
        let first =
            std::thread::spawn(move || remote_image_bytes(&first_cache, &first_url).unwrap());
        let second_cache = Arc::clone(&cache);
        let second = std::thread::spawn(move || remote_image_bytes(&second_cache, &url).unwrap());

        assert_eq!(first.join().unwrap().as_ref(), expected.as_slice());
        assert_eq!(second.join().unwrap().as_ref(), expected.as_slice());
        server.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn expired_remote_failure_is_downloaded_again() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected = png_bytes();
        let response = expected.clone();
        let server = std::thread::spawn(move || {
            write_response(listener.accept().unwrap().0, "200 OK", &response);
        });

        let url = format!("http://{address}/recovered.png");
        let download = Arc::new(RemoteDownload::new());
        *download.state.lock().unwrap() = RemoteDownloadState::Failed {
            error: "temporary failure".to_string(),
            retry_at: Instant::now() - Duration::from_millis(1),
        };
        let cache = Arc::new(Mutex::new(RemoteSourceCache {
            entries: HashMap::from([(url.clone(), download)]),
            order: VecDeque::from([url.clone()]),
        }));

        let bytes = remote_image_bytes(&cache, &url).unwrap();
        assert_eq!(bytes.as_ref(), expected.as_slice());
        server.join().unwrap();
    }

    #[test]
    fn failed_image_placeholder_frames_the_alt_and_error() {
        let placement = mbtui::ImagePlacement {
            source: "https://example.test/image.png".to_string(),
            title: String::new(),
            alt: "Architecture diagram".to_string(),
            row: 0,
            column: 0,
            width: 32,
            height: 6,
        };
        let mut terminal = Terminal::new(TestBackend::new(32, 6)).unwrap();
        terminal
            .draw(|frame| {
                draw_image_placeholder(
                    frame,
                    frame.area(),
                    0,
                    &placement,
                    "connection refused",
                    true,
                    Theme::default(),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..6)
            .map(|row| {
                (0..32)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.starts_with('┌'));
        assert!(rendered.lines().next().unwrap().ends_with('┐'));
        assert!(rendered.contains("Architecture diagram"));
        assert!(rendered.contains("Failed: connection refused"));
        assert!(rendered.lines().last().unwrap().starts_with('└'));
        assert!(rendered.lines().last().unwrap().ends_with('┘'));
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
