//! Read-only filesystem, note search, and unified read tools.
//!
//! The `read` tool is a parser registry: a target (file path, directory path,
//! http(s) URL, or attachment URI) is resolved once, then each registered
//! [`ReadParser`] is asked in order whether it handles that target. Registering
//! a new format (for example a PDF parser) requires no change to the dispatch
//! logic — only a new parser registered before the generic text-file parser.
//! Attachment reads are read-only: they never register an edit snapshot.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde_json::{json, Value};
use tokio::fs as async_fs;

use super::util::{
    display_path, fuzzy_match, optional_usize, range_schema, required_string, truncate_chars,
    RangeSelector, MAX_EDIT_FILE_BYTES, MAX_SEARCH_RESULTS, MAX_SEARCH_SNIPPET_CHARS,
};
use super::web::{read_http_body_with_limit, web_fetch_content};
use crate::agent::{canonical_root, ReadTracker, SnapshotTagHasher, Tool, ToolExecutionPolicy};
use crate::attachment::{AttachmentId, AttachmentStore, AttachmentUri};
use crate::storage::ATTACHMENTS_DIR;

const DEFAULT_READ_LINES: usize = 200;
const MAX_READ_LINES: usize = 2_000;
const MAX_READ_RESPONSE_BYTES: usize = 1_000_000;
const MAX_READ_LINE_BYTES: usize = 256 * 1024;
const READ_RESPONSE_OVERHEAD: usize = 8 * 1024;
const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WEB_READ_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXTRACTED_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_NOTE_RESULTS: usize = 2_000;
const MAX_DIRECTORY_RESULTS: usize = 2_000;
const MAX_DIRECTORY_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineRange {
    offset: usize,
    limit: usize,
}

/// A resolved target for the unified `read` tool.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    File {
        path: PathBuf,
        range: Option<LineRange>,
    },
    Directory {
        path: PathBuf,
    },
    Web {
        url: String,
        range: Option<LineRange>,
    },
    Attachment {
        uri: AttachmentUri,
        range: Option<LineRange>,
    },
}

impl Target {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Target::File { .. } => "file",
            Target::Directory { .. } => "directory",
            Target::Web { .. } => "web",
            Target::Attachment { .. } => "attachment",
        }
    }

    /// Root-relative form for local paths, the URL for web targets, and the
    /// canonical URI for attachments.
    pub(crate) fn display(&self, root: &Path) -> String {
        match self {
            Target::File { path, .. } | Target::Directory { path } => listed_path(root, path),
            Target::Web { url, .. } => url.clone(),
            Target::Attachment { uri, .. } => uri.to_string(),
        }
    }
}

/// Shared dependencies handed to every [`ReadParser`].
pub(crate) struct ParseContext {
    pub(crate) root: PathBuf,
    pub(crate) reads: Arc<ReadTracker>,
    pub(crate) client: reqwest::Client,
    pub(crate) attachments: AttachmentStore,
}

pub(crate) enum ReadPayload {
    Text(String),
    Structured(Value),
}

/// A reader for one kind of target. Implementations are tried in registration
/// order; specific parsers must be registered before the generic text-file one.
#[async_trait::async_trait]
pub(crate) trait ReadParser: Send + Sync {
    fn name(&self) -> &'static str;

    fn matches(&self, target: &Target) -> bool;

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        input: &Value,
    ) -> Result<ReadPayload>;
}

pub struct Read {
    ctx: ParseContext,
    parsers: Vec<Box<dyn ReadParser>>,
}

impl Read {
    pub fn new(root: &Path, reads: Arc<ReadTracker>, client: reqwest::Client) -> Result<Self> {
        let root = canonical_root(root)?;
        let attachments = AttachmentStore::new(root.join(ATTACHMENTS_DIR));
        let ctx = ParseContext {
            root,
            reads,
            client,
            attachments,
        };
        // Order matters: the generic text-file parser must be tried last so a
        // more specific file parser (for example PDF) can claim a target first.
        let parsers: Vec<Box<dyn ReadParser>> = vec![
            Box::new(WebParser),
            Box::new(DirectoryParser),
            Box::new(PdfFileParser),
            Box::new(AttachmentParser),
            Box::new(TextFileParser),
        ];
        Ok(Self { ctx, parsers })
    }

    /// Registers a parser ahead of built-in file parsers so callers can
    /// override format handling before the generic fallbacks.
    #[allow(dead_code)]
    pub fn register(&mut self, parser: impl ReadParser + 'static) {
        let file_index = self
            .parsers
            .iter()
            .position(|parser| parser.name() == "pdf_file")
            .unwrap_or(self.parsers.len());
        self.parsers.insert(file_index, Box::new(parser));
    }
}

#[async_trait::async_trait]
impl Tool for Read {
    fn name(&self) -> &'static str {
        "read"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Read local files, directories, URLs, PDFs, and attachment URIs. Text and extracted documents accept an inclusive `:start-end` line selector; editable text returns tagged source lines, while directories use range, depth, and sort options."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Local file/PDF path, http(s) URL/PDF URL, or attachment URI, optionally suffixed with inclusive lines `:start-end`; or a directory path"
                },
                "range": range_schema(MAX_DIRECTORY_RESULTS),
                "depth": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_DIRECTORY_DEPTH, "default": 1,
                    "description": "Directory recursion depth (directory targets only)"
                },
                "sort_by": {
                    "type": "string",
                    "enum": ["name", "type", "depth", "line_count", "created_at", "modified_at", "size"],
                    "default": "name",
                    "description": "Directory entry sort key (directory targets only)"
                },
                "order": {
                    "type": "string", "enum": ["asc", "desc"], "default": "asc",
                    "description": "Directory entry sort order (directory targets only)"
                }
            },
            "required": ["path"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let target = resolve_target(&self.ctx, input).await?;
        for parser in &self.parsers {
            if parser.matches(&target) {
                return match parser.parse(&self.ctx, &target, input).await? {
                    ReadPayload::Text(text) => Ok(text),
                    ReadPayload::Structured(Value::Object(mut payload)) => {
                        payload.insert("kind".into(), json!(target.kind()));
                        payload.insert("target".into(), json!(target.display(&self.ctx.root)));
                        serde_json::to_string_pretty(&Value::Object(payload))
                            .context("encoding read result")
                    }
                    ReadPayload::Structured(_) => {
                        bail!("parser {} returned non-object payload", parser.name())
                    }
                };
            }
        }
        bail!(
            "no parser registered for {} target {}",
            target.kind(),
            target.display(&self.ctx.root)
        )
    }
}

/// Resolves the `path` argument into a concrete target, rejecting the private
/// AI configuration and attachment internals before any parser sees them.
async fn resolve_target(ctx: &ParseContext, input: &Value) -> Result<Target> {
    let requested_input = required_string(input, "path")?;
    let (requested, range) = split_line_range(&requested_input)?;
    if requested.starts_with("https://") || requested.starts_with("http://") {
        return Ok(Target::Web {
            url: requested.to_string(),
            range,
        });
    }
    if AttachmentUri::is_attachment_uri(requested) {
        let uri = AttachmentUri::parse(requested)?;
        return Ok(Target::Attachment { uri, range });
    }
    let path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        ctx.root.join(requested)
    };
    let path = async_fs::canonicalize(&path)
        .await
        .with_context(|| format!("resolving {}", path.display()))?;
    if path.starts_with(ctx.root.join(ATTACHMENTS_DIR)) {
        bail!("generic tools cannot read attachment internals");
    }
    let private_config = ctx.root.join("config/ai.toml");
    if path == private_config {
        bail!("AI configuration is private");
    }
    let metadata = async_fs::metadata(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    if metadata.is_file() {
        Ok(Target::File { path, range })
    } else if metadata.is_dir() {
        if range.is_some() {
            bail!("line selectors can only be used with files and text attachments");
        }
        Ok(Target::Directory { path })
    } else {
        bail!(
            "path is not a regular file or directory: {}",
            path.display()
        )
    }
}

fn split_line_range(requested: &str) -> Result<(&str, Option<LineRange>)> {
    let Some((path, suffix)) = requested.rsplit_once(':') else {
        return Ok((requested, None));
    };
    let Some((start, end)) = suffix.split_once('-') else {
        return Ok((requested, None));
    };
    let (Ok(start), Ok(end)) = (start.parse::<usize>(), end.parse::<usize>()) else {
        return Ok((requested, None));
    };
    if start == 0 || end < start {
        bail!("line selector must be an inclusive range with 1 <= start <= end");
    }
    let limit = end - start + 1;
    if limit > MAX_READ_LINES {
        bail!("line selector may request at most {MAX_READ_LINES} lines");
    }
    Ok((
        path,
        Some(LineRange {
            offset: start - 1,
            limit,
        }),
    ))
}

fn line_window(range: Option<LineRange>, input: &Value) -> Result<(usize, usize)> {
    if input.get("range").is_some() {
        bail!("file and attachment lines must use a `path:start-end` selector");
    }
    Ok(range
        .map(|range| (range.offset, range.limit))
        .unwrap_or((0, DEFAULT_READ_LINES)))
}

fn continuation_selector(target: &str, end: usize, limit: usize) -> String {
    format!(
        "{target}:{}-{}",
        end.saturating_add(1),
        end.saturating_add(limit)
    )
}

#[derive(Debug)]
struct TextPage {
    lines: Vec<String>,
    start: usize,
    end: usize,
    total_lines: Option<usize>,
    has_more: bool,
    tag: Option<String>,
    full_content: Option<String>,
}

/// Read one UTF-8 line window with bounded memory. Editable files are scanned
/// completely to retain their exact snapshot and tag. Read-only files stop
/// after one look-ahead line once the requested window is full.
fn read_utf8_page(
    path: &Path,
    offset: usize,
    limit: usize,
    retain_snapshot: bool,
    encoded_len: fn(&str) -> usize,
) -> Result<Option<TextPage>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = retain_snapshot.then(SnapshotTagHasher::default);
    let mut full_content = retain_snapshot.then(String::new);
    let mut selected = Vec::with_capacity(limit.min(DEFAULT_READ_LINES));
    let mut line = Vec::new();
    let mut lines_seen = 0usize;
    let mut response_bytes = 0usize;
    let response_budget = MAX_READ_RESPONSE_BYTES.saturating_sub(READ_RESPONSE_OVERHEAD);
    let mut response_full = false;
    let mut has_more = false;
    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_READ_LINE_BYTES.saturating_add(3) as u64)
            .read_until(b'\n', &mut line)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        let raw = match std::str::from_utf8(&line) {
            Ok(raw) => raw,
            Err(_) => return Ok(None),
        };
        let without_lf = raw.strip_suffix('\n').unwrap_or(raw);
        let text = without_lf.strip_suffix('\r').unwrap_or(without_lf);
        if text.len() > MAX_READ_LINE_BYTES {
            bail!("line {} exceeds the 256 KiB read limit", lines_seen + 1);
        }
        if let Some(hasher) = &mut hasher {
            hasher.update(&line);
        }
        if let Some(content) = &mut full_content {
            if content.len().saturating_add(raw.len()) <= MAX_EDIT_FILE_BYTES as usize {
                content.push_str(raw);
            } else {
                full_content = None;
            }
        }
        if !retain_snapshot && lines_seen >= offset && (selected.len() >= limit || response_full) {
            has_more = true;
            break;
        }
        if lines_seen >= offset && selected.len() < limit && !response_full {
            let cost = encoded_len(text).saturating_add(32);
            if response_bytes.saturating_add(cost) <= response_budget {
                selected.push(text.to_string());
                response_bytes = response_bytes.saturating_add(cost);
            } else if selected.is_empty() {
                bail!(
                    "line {} cannot fit within the 1 MB read response limit",
                    lines_seen + 1
                );
            } else if retain_snapshot {
                response_full = true;
            } else {
                has_more = true;
                break;
            }
        }
        lines_seen += 1;
    }
    let start = offset.min(lines_seen);
    let end = start.saturating_add(selected.len());
    if retain_snapshot {
        has_more = end < lines_seen;
    }
    Ok(Some(TextPage {
        lines: selected,
        start,
        end,
        total_lines: (retain_snapshot || !has_more).then_some(lines_seen),
        has_more,
        tag: hasher.map(SnapshotTagHasher::finish),
        full_content,
    }))
}

fn plain_response_len(text: &str) -> usize {
    text.len()
}

fn json_response_len(text: &str) -> usize {
    text.chars()
        .map(|character| match character {
            '"' | '\\' | '\u{0008}' | '\t' | '\n' | '\u{000c}' | '\r' => 2,
            character if character <= '\u{001f}' => 6,
            character => character.len_utf8(),
        })
        .sum()
}

fn page_extracted_text(
    content: &str,
    offset: usize,
    limit: usize,
    encoded_len: fn(&str) -> usize,
) -> Result<TextPage> {
    if content.len() > MAX_EXTRACTED_TEXT_BYTES {
        bail!("extracted text exceeds the {MAX_EXTRACTED_TEXT_BYTES} byte limit");
    }
    let mut selected = Vec::with_capacity(limit.min(DEFAULT_READ_LINES));
    let mut lines_seen = 0usize;
    let mut response_bytes = 0usize;
    let response_budget = MAX_READ_RESPONSE_BYTES.saturating_sub(READ_RESPONSE_OVERHEAD);
    let mut has_more = false;
    for raw in content.split_inclusive('\n') {
        let without_lf = raw.strip_suffix('\n').unwrap_or(raw);
        let text = without_lf.strip_suffix('\r').unwrap_or(without_lf);
        if text.len() > MAX_READ_LINE_BYTES {
            bail!("line {} exceeds the 256 KiB read limit", lines_seen + 1);
        }
        if lines_seen >= offset && selected.len() >= limit {
            has_more = true;
            break;
        }
        if lines_seen >= offset {
            let cost = encoded_len(text).saturating_add(32);
            if response_bytes.saturating_add(cost) <= response_budget {
                selected.push(text.to_string());
                response_bytes = response_bytes.saturating_add(cost);
            } else if selected.is_empty() {
                bail!(
                    "line {} cannot fit within the 1 MB read response limit",
                    lines_seen + 1
                );
            } else {
                has_more = true;
                break;
            }
        }
        lines_seen += 1;
    }
    let start = offset.min(lines_seen);
    let end = start.saturating_add(selected.len());
    Ok(TextPage {
        lines: selected,
        start,
        end,
        total_lines: (!has_more).then_some(lines_seen),
        has_more,
        tag: None,
        full_content: None,
    })
}

fn add_structured_page(
    payload: &mut Value,
    page: TextPage,
    target: &str,
    offset: usize,
    limit: usize,
) {
    payload["range"] = json!(format!("{}-{}", offset + 1, offset.saturating_add(limit)));
    payload["returned"] = json!(page.end - page.start);
    payload["total"] = json!(page.total_lines);
    payload["has_more"] = json!(page.has_more);
    if page.has_more {
        payload["next"] = json!(continuation_selector(target, page.end, limit));
    }
    payload["items"] = json!(page.lines);
}

struct TextFileParser;

#[async_trait::async_trait]
impl ReadParser for TextFileParser {
    fn name(&self) -> &'static str {
        "text_file"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::File { .. })
    }

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        input: &Value,
    ) -> Result<ReadPayload> {
        let Target::File { path, range } = target else {
            bail!("text_file parser received non-file target");
        };
        let metadata = async_fs::metadata(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if !metadata.is_file() {
            bail!("path is not a regular file: {}", path.display());
        }
        let (offset, limit) = line_window(*range, input)?;
        let retain_snapshot = metadata.len() <= MAX_EDIT_FILE_BYTES;
        let page_path = path.clone();
        let page = tokio::task::spawn_blocking(move || {
            read_utf8_page(
                &page_path,
                offset,
                limit,
                retain_snapshot,
                plain_response_len,
            )
        })
        .await
        .context("joining paginated file read")??
        .with_context(|| format!("file is not valid UTF-8: {}", path.display()))?;
        let editable_snapshot = page.full_content.is_some();
        let target = listed_path(&ctx.root, path);
        let mut output = match &page.tag {
            Some(tag) => format!("[{target}#{tag}]"),
            None => format!("[{target}]"),
        };
        for (index, text) in page.lines.iter().enumerate() {
            write!(output, "\n{}:{text}", page.start + index + 1)?;
        }
        let first = if page.start < page.end {
            page.start + 1
        } else {
            0
        };
        match page.total_lines {
            Some(total_lines) => write!(
                output,
                "\n\n[Showing lines {first}-{} of {total_lines}",
                page.end
            )?,
            None => write!(output, "\n\n[Showing lines {first}-{}", page.end)?,
        }
        if page.has_more {
            write!(
                output,
                ". Continue with {}",
                continuation_selector(&target, page.end, limit)
            )?;
        }
        if !editable_snapshot {
            output.push_str(". Read-only: file exceeds the 1 MB edit limit");
        }
        output.push(']');
        if let Some(content) = page.full_content {
            let total_lines = page
                .total_lines
                .expect("editable snapshots always scan the complete file");
            let tracked_tag =
                ctx.reads
                    .mark_file(path.clone(), content, page.start, page.end, total_lines)?;
            debug_assert_eq!(page.tag.as_deref(), Some(tracked_tag.as_str()));
        }
        Ok(ReadPayload::Text(output))
    }
}

struct PdfFileParser;

#[async_trait::async_trait]
impl ReadParser for PdfFileParser {
    fn name(&self) -> &'static str {
        "pdf_file"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::File { path, .. } if is_pdf_path(path))
    }

    async fn parse(
        &self,
        _ctx: &ParseContext,
        target: &Target,
        input: &Value,
    ) -> Result<ReadPayload> {
        let Target::File { path, range } = target else {
            bail!("pdf_file parser received non-file target");
        };
        let metadata = async_fs::metadata(path)
            .await
            .with_context(|| format!("reading PDF {}", path.display()))?;
        if metadata.len() > MAX_PDF_BYTES {
            bail!("PDF exceeds the {MAX_PDF_BYTES} byte extraction limit");
        }
        let (offset, limit) = line_window(*range, input)?;
        let pdf_path = path.clone();
        let text = tokio::task::spawn_blocking(move || {
            pdf_extract::extract_text(&pdf_path)
                .with_context(|| format!("extracting PDF {}", pdf_path.display()))
        })
        .await
        .context("joining PDF extraction")??;
        let page = page_extracted_text(&text, offset, limit, json_response_len)?;
        let mut payload = json!({ "format": "pdf" });
        add_structured_page(
            &mut payload,
            page,
            &target.display(_ctx.root.as_path()),
            offset,
            limit,
        );
        Ok(ReadPayload::Structured(payload))
    }
}

fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

fn is_pdf_content(content_type: Option<&str>, url: &str, bytes: &[u8]) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/pdf"))
        || url
            .split(['?', '#'])
            .next()
            .is_some_and(|path| path.to_ascii_lowercase().ends_with(".pdf"))
        || bytes.starts_with(b"%PDF-")
}

fn extract_pdf_bytes(bytes: Vec<u8>) -> Result<String> {
    pdf_extract::extract_text_from_mem(&bytes).context("extracting PDF text")
}

struct AttachmentParser;

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
        if mime.eq_ignore_ascii_case("application/pdf") {
            if metadata.size > MAX_PDF_BYTES {
                bail!("PDF exceeds the {MAX_PDF_BYTES} byte extraction limit");
            }
            let (offset, limit) = line_window(*range, input)?;
            let pdf_path = ctx
                .attachments
                .open(uri.id())
                .with_context(|| format!("opening attachment {uri}"))?;
            let text = tokio::task::spawn_blocking(move || {
                pdf_extract::extract_text(&pdf_path)
                    .with_context(|| format!("extracting PDF {}", pdf_path.display()))
            })
            .await
            .context("joining PDF extraction")??;
            let page = page_extracted_text(&text, offset, limit, json_response_len)?;
            let mut payload = attachment_metadata_json(*uri, &metadata);
            payload["format"] = json!("pdf");
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
            read_utf8_page(&page_path, offset, limit, false, json_response_len)
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
fn attachment_metadata_json(
    uri: AttachmentUri,
    metadata: &crate::attachment::AttachmentMetadata,
) -> Value {
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

struct DirectoryEntryMetadata {
    path: PathBuf,
    name: String,
    kind: &'static str,
    depth: usize,
    extension: Option<String>,
    line_count: Option<u64>,
    created: Option<std::time::SystemTime>,
    modified: Option<std::time::SystemTime>,
    size: Option<u64>,
}

struct DirectoryParser;

#[async_trait::async_trait]
impl ReadParser for DirectoryParser {
    fn name(&self) -> &'static str {
        "directory"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::Directory { .. })
    }

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        input: &Value,
    ) -> Result<ReadPayload> {
        let Target::Directory { path } = target else {
            bail!("directory parser received non-directory target");
        };
        let depth = optional_usize(input, "depth", 1, MAX_DIRECTORY_DEPTH)?;
        let sort_by = input
            .get("sort_by")
            .and_then(Value::as_str)
            .unwrap_or("name");
        if !matches!(
            sort_by,
            "name" | "type" | "depth" | "line_count" | "created_at" | "modified_at" | "size"
        ) {
            bail!("unsupported sort_by: {sort_by}");
        }
        let descending = match input.get("order").and_then(Value::as_str).unwrap_or("asc") {
            "asc" => false,
            "desc" => true,
            other => bail!("unsupported order: {other}"),
        };
        let selector = RangeSelector::from_input(input, MAX_DIRECTORY_RESULTS)?;
        let mut entries = directory_entries(path, depth).await?;
        entries.sort_by(|a, b| {
            let ordering = match sort_by {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "type" => a.kind.cmp(b.kind),
                "depth" => a.depth.cmp(&b.depth),
                "line_count" => a.line_count.cmp(&b.line_count),
                "created_at" => a.created.cmp(&b.created),
                "modified_at" => a.modified.cmp(&b.modified),
                "size" => a.size.cmp(&b.size),
                _ => unreachable!(),
            };
            let ordering = if descending {
                ordering.reverse()
            } else {
                ordering
            };
            ordering.then_with(|| a.path.cmp(&b.path))
        });
        let page = selector.window(entries.len());
        let items = entries[page.start_index..page.end_index]
            .iter()
            .map(|entry| {
                json!({
                    "path": listed_path(&ctx.root, &entry.path),
                    "name": entry.name,
                    "type": entry.kind,
                    "depth": entry.depth,
                    "extension": entry.extension,
                    "line_count": entry.line_count,
                    "created_at": entry.created.map(|time| DateTime::<Local>::from(time).to_rfc3339()),
                    "modified_at": entry.modified.map(|time| DateTime::<Local>::from(time).to_rfc3339()),
                    "size": entry.size,
                })
            })
            .collect::<Vec<_>>();
        let mut payload = json!({
            "depth": depth,
            "sort_by": sort_by,
            "order": if descending { "desc" } else { "asc" },
            "range": selector.as_string(),
            "returned": page.returned(),
            "total": page.total,
            "has_more": page.has_more(),
            "items": items,
        });
        if let Some(next) = page.next() {
            payload["next"] = json!(next);
        }
        Ok(ReadPayload::Structured(payload))
    }
}

struct WebParser;

#[async_trait::async_trait]
impl ReadParser for WebParser {
    fn name(&self) -> &'static str {
        "web"
    }

    fn matches(&self, target: &Target) -> bool {
        matches!(target, Target::Web { .. })
    }

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        input: &Value,
    ) -> Result<ReadPayload> {
        let Target::Web { url, range } = target else {
            bail!("web parser received non-web target");
        };
        let (offset, limit) = line_window(*range, input)?;
        let response = ctx
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("fetching {url}"))?;
        if !response.status().is_success() {
            bail!("fetch returned HTTP {}", response.status());
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = read_http_body_with_limit(response, "response", MAX_WEB_READ_BYTES).await?;
        let (content, format) = if is_pdf_content(content_type.as_deref(), url, &bytes) {
            let content = tokio::task::spawn_blocking(move || extract_pdf_bytes(bytes))
                .await
                .context("joining PDF extraction")??;
            (content, "pdf")
        } else {
            (web_fetch_content(content_type.as_deref(), bytes)?, "text")
        };
        let page = page_extracted_text(&content, offset, limit, json_response_len)?;
        let mut payload = json!({
            "format": format,
            "content_type": content_type,
        });
        add_structured_page(&mut payload, page, url, offset, limit);
        Ok(ReadPayload::Structured(payload))
    }
}

async fn directory_entries(root: &Path, max_depth: usize) -> Result<Vec<DirectoryEntryMetadata>> {
    let mut entries = Vec::new();
    let mut directories = vec![(root.to_path_buf(), 1usize)];
    while let Some((directory, depth)) = directories.pop() {
        let mut children = async_fs::read_dir(&directory)
            .await
            .with_context(|| format!("listing directory {}", directory.display()))?;
        while let Some(child) = children
            .next_entry()
            .await
            .with_context(|| format!("listing directory {}", directory.display()))?
        {
            let path = child.path();
            let metadata = async_fs::symlink_metadata(&path)
                .await
                .with_context(|| format!("reading metadata for {}", path.display()))?;
            let file_type = metadata.file_type();
            let kind = if file_type.is_dir() {
                "directory"
            } else if file_type.is_file() {
                "file"
            } else if file_type.is_symlink() {
                "symlink"
            } else {
                "other"
            };
            let line_count = if file_type.is_file() && metadata.len() <= MAX_EDIT_FILE_BYTES {
                count_file_lines(&path).await.ok()
            } else {
                None
            };
            entries.push(DirectoryEntryMetadata {
                name: child.file_name().to_string_lossy().into_owned(),
                extension: path
                    .extension()
                    .map(|extension| extension.to_string_lossy().into_owned()),
                line_count,
                created: metadata.created().ok(),
                modified: metadata.modified().ok(),
                size: file_type.is_file().then_some(metadata.len()),
                path: path.clone(),
                kind,
                depth,
            });
            if file_type.is_dir() && depth < max_depth {
                directories.push((path, depth + 1));
            }
        }
    }
    Ok(entries)
}

async fn count_file_lines(path: &Path) -> Result<u64> {
    let content = async_fs::read(path).await?;
    let newlines = content.iter().filter(|byte| **byte == b'\n').count() as u64;
    Ok(newlines + u64::from(!content.is_empty() && !content.ends_with(b"\n")))
}

fn listed_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

struct NoteMetadata {
    path: PathBuf,
    name: String,
    line_count: u64,
    created: Option<std::time::SystemTime>,
    modified: std::time::SystemTime,
    size: u64,
}

pub struct ListNotes {
    data_dir: PathBuf,
    root: PathBuf,
}

impl ListNotes {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            data_dir: root.join("data"),
            root,
        })
    }
}

#[async_trait::async_trait]
impl Tool for ListNotes {
    fn name(&self) -> &'static str {
        "list_notes"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "List active data/ .md and .mb notes with line count, creation time, modification time, and byte size. Supports metadata sorting and pagination."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sort_by": {
                    "type": "string",
                    "enum": ["name", "line_count", "created_at", "modified_at", "size"],
                    "default": "modified_at"
                },
                "order": { "type": "string", "enum": ["asc", "desc"], "default": "desc" },
                "range": range_schema(MAX_NOTE_RESULTS)
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let sort_by = input
            .get("sort_by")
            .and_then(Value::as_str)
            .unwrap_or("modified_at");
        let descending = match input.get("order").and_then(Value::as_str).unwrap_or("desc") {
            "asc" => false,
            "desc" => true,
            other => bail!("unsupported order: {other}"),
        };
        if !matches!(
            sort_by,
            "name" | "line_count" | "created_at" | "modified_at" | "size"
        ) {
            bail!("unsupported sort_by: {sort_by}");
        }
        let selector = RangeSelector::from_input(input, MAX_NOTE_RESULTS)?;
        let listed = list_note_files_in(&self.data_dir).await?;
        let mut notes = Vec::with_capacity(listed.len());
        for note in listed {
            notes.push(note_metadata(note.path).await?);
        }
        notes.sort_by(|a, b| {
            let ordering = match sort_by {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "line_count" => a.line_count.cmp(&b.line_count),
                "created_at" => a.created.cmp(&b.created),
                "modified_at" => a.modified.cmp(&b.modified),
                "size" => a.size.cmp(&b.size),
                _ => unreachable!(),
            };
            let ordering = if descending {
                ordering.reverse()
            } else {
                ordering
            };
            ordering.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let page = selector.window(notes.len());
        let items = notes[page.start_index..page.end_index]
            .iter()
            .map(|note| json!({
                "path": display_path(&self.root, &note.path),
                "name": note.name,
                "line_count": note.line_count,
                "created_at": note.created.map(|time| DateTime::<Local>::from(time).to_rfc3339()),
                "modified_at": DateTime::<Local>::from(note.modified).to_rfc3339(),
                "size": note.size,
            }))
            .collect::<Vec<_>>();
        let mut result = json!({
            "sort_by": sort_by,
            "order": if descending { "desc" } else { "asc" },
            "range": selector.as_string(),
            "returned": page.returned(),
            "total": page.total,
            "has_more": page.has_more(),
            "items": items,
        });
        if let Some(next) = page.next() {
            result["next"] = json!(next);
        }
        serde_json::to_string_pretty(&result).context("encoding note listing")
    }
}

async fn note_metadata(path: PathBuf) -> Result<NoteMetadata> {
    let metadata = async_fs::metadata(&path)
        .await
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    let line_count = count_file_lines(&path).await?;
    Ok(NoteMetadata {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path,
        line_count,
        created: metadata.created().ok(),
        modified: metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        size: metadata.len(),
    })
}

pub struct SearchContent {
    directories: [PathBuf; 3],
    root: PathBuf,
}

impl SearchContent {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            directories: [root.join("daily"), root.join("data"), root.join("archives")],
            root,
        })
    }
}

#[async_trait::async_trait]
impl Tool for SearchContent {
    fn name(&self) -> &'static str {
        "search_content"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Case-insensitive full-text search across managed Markdown files. Returns paths and matching one-based source lines with range pagination."
    }

    fn input_schema(&self) -> Value {
        search_schema("Text to find in managed Markdown file contents")
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let query = required_string(input, "query")?.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        let selector = RangeSelector::from_input(input, MAX_SEARCH_RESULTS)?;
        let mut matches = Vec::new();
        let lowercase_query = query.to_lowercase();
        for directory in &self.directories {
            for file in list_note_files_in(directory).await? {
                let Ok(source) = async_fs::read_to_string(&file.path).await else {
                    continue;
                };
                for (line, text) in source.lines().enumerate() {
                    if text.to_lowercase().contains(&lowercase_query) {
                        matches.push(json!({
                            "path": display_path(&self.root, &file.path),
                            "line": line + 1,
                            "snippet": truncate_chars(text, MAX_SEARCH_SNIPPET_CHARS),
                        }));
                    }
                }
            }
        }
        paginated_search_result(query, selector, matches)
    }
}

pub struct SearchFiles {
    directories: [PathBuf; 2],
    root: PathBuf,
}

impl SearchFiles {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            directories: [root.join("data"), root.join("archives")],
            root,
        })
    }
}

#[async_trait::async_trait]
impl Tool for SearchFiles {
    fn name(&self) -> &'static str {
        "search_files"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Fuzzy, case-insensitive filename search across active and archived .md/.mb notes with result pagination."
    }

    fn input_schema(&self) -> Value {
        search_schema("Fuzzy filename query; the extension is not required")
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let query = required_string(input, "query")?.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        let selector = RangeSelector::from_input(input, MAX_SEARCH_RESULTS)?;
        let mut listed = Vec::new();
        for directory in &self.directories {
            listed.extend(list_note_files_in(directory).await?);
        }
        let matches = listed
            .into_iter()
            .filter(|file| {
                file.path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| fuzzy_match(name, query))
            })
            .map(|file| {
                json!({
                    "path": display_path(&self.root, &file.path),
                    "name": file.path.file_name().unwrap_or_default().to_string_lossy(),
                })
            })
            .collect();
        paginated_search_result(query, selector, matches)
    }
}

struct ListedNote {
    path: PathBuf,
    modified: std::time::SystemTime,
}

async fn list_note_files_in(directory: &Path) -> Result<Vec<ListedNote>> {
    async_fs::create_dir_all(directory)
        .await
        .with_context(|| format!("creating {}", directory.display()))?;
    let mut entries = async_fs::read_dir(directory)
        .await
        .with_context(|| format!("listing {}", directory.display()))?;
    let mut files = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .with_context(|| format!("listing {}", directory.display()))?
    {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if !is_note_path(&path) {
            continue;
        }
        let modified = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        files.push(ListedNote { path, modified });
    }
    files.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.path.file_name().cmp(&right.path.file_name()))
    });
    Ok(files)
}

fn is_note_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("mb")
        })
}

fn search_schema(query_description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": query_description },
            "range": range_schema(MAX_SEARCH_RESULTS)
        },
        "required": ["query"], "additionalProperties": false
    })
}

fn paginated_search_result(
    query: &str,
    selector: RangeSelector,
    matches: Vec<Value>,
) -> Result<String> {
    let page = selector.window(matches.len());
    let mut result = json!({
        "query": query,
        "range": selector.as_string(),
        "returned": page.returned(),
        "total": page.total,
        "has_more": page.has_more(),
        "items": &matches[page.start_index..page.end_index],
    });
    if let Some(next) = page.next() {
        result["next"] = json!(next);
    }
    serde_json::to_string_pretty(&result).context("encoding search results")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{BufRead as _, Cursor, Write as _};
    use std::net::TcpListener;

    use super::*;

    struct FakeParser;

    #[async_trait::async_trait]
    impl ReadParser for FakeParser {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn matches(&self, target: &Target) -> bool {
            matches!(target, Target::File { .. })
        }

        async fn parse(
            &self,
            _ctx: &ParseContext,
            _target: &Target,
            _input: &Value,
        ) -> Result<ReadPayload> {
            Ok(ReadPayload::Structured(json!({ "parsed_by": "fake" })))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_dispatches_to_registered_parsers_before_text_file() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("sample.pdf"), b"%PDF-1.4").unwrap();
        let mut read = Read::new(
            directory.path(),
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        read.register(FakeParser);

        let output = read
            .execute(&json!({ "path": "sample.pdf" }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["kind"], "file");
        assert_eq!(parsed["parsed_by"], "fake");
    }

    fn attachment_ctx(directory: &std::path::Path) -> ParseContext {
        ParseContext {
            root: directory.to_path_buf(),
            reads: Arc::new(ReadTracker::default()),
            client: reqwest::Client::new(),
            attachments: AttachmentStore::new(directory.join(ATTACHMENTS_DIR)),
        }
    }

    fn large_text(line_count: usize) -> String {
        (0..line_count)
            .map(|line| format!("line {line:05} {}", "x".repeat(20)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn simple_pdf(text: &str) -> Vec<u8> {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        let stream = format!("BT /F1 12 Tf 72 720 Td ({escaped}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        pdf
    }

    fn serve_once(
        content_type: &'static str,
        body: Vec<u8>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        (format!("http://{address}"), server)
    }

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
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": "report.pdf:1-20"}))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["kind"], "file");
        assert_eq!(parsed["format"], "pdf");
        assert!(parsed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("PDF marker")));
    }

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
    async fn url_selectors_page_reader_mode_text() {
        let (url, server) = serve_once("text/plain", b"first\nsecond\nthird\n".to_vec());
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": format!("{url}:2-2")}))
            .await
            .unwrap();
        server.join().unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["items"], json!(["second"]));
        assert_eq!(parsed["next"], format!("{url}:3-3"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_pdf_urls_extract_text_without_a_pdf_scheme() {
        let (url, server) = serve_once("application/pdf", simple_pdf("Remote PDF marker"));
        let read = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": format!("{url}/report.pdf:1-20")}))
            .await
            .unwrap();
        server.join().unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["format"], "pdf");
        assert!(parsed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("Remote PDF marker")));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_files_over_one_megabyte_are_read_in_pages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.txt");
        let content = large_text(50_000);
        assert!(content.len() as u64 > MAX_EDIT_FILE_BYTES);
        fs::write(&path, &content).unwrap();
        let tracker = Arc::new(ReadTracker::default());
        let read = Read::new(directory.path(), tracker.clone(), reqwest::Client::new()).unwrap();

        let output = read
            .execute(&json!({"path": "large.txt:49991-49995"}))
            .await
            .unwrap();
        assert!(output.contains("49991:line 49990"));
        assert!(output.contains("[Showing lines 49991-49995"));
        assert!(output.contains("Continue with large.txt:49996-50000"));
        assert!(output.contains("Read-only: file exceeds the 1 MB edit limit"));
        assert!(tracker
            .file_state(&fs::canonicalize(path).unwrap())
            .unwrap()
            .is_none());
        let final_page = read
            .execute(&json!({"path": "large.txt:49999-50003"}))
            .await
            .unwrap();
        assert!(final_page.contains("[Showing lines 49999-50000 of 50000"));
        assert!(!final_page.contains("Continue with"));
    }

    #[test]
    fn response_byte_cap_returns_a_continuable_partial_page() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wide.txt");
        let line = "x".repeat(250 * 1024);
        fs::write(&path, vec![line; 5].join("\n")).unwrap();
        let page = read_utf8_page(&path, 0, 5, false, plain_response_len)
            .unwrap()
            .unwrap();
        assert_eq!(page.total_lines, None);
        assert!(page.end > 0 && page.end < 5);
        assert_eq!(page.start, 0);
        assert_eq!(page.lines.len(), page.end);
    }

    #[test]
    fn oversized_single_line_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("one-line.txt");
        fs::write(&path, vec![b'x'; MAX_READ_LINE_BYTES + 1]).unwrap();
        let error = read_utf8_page(&path, 0, 1, false, plain_response_len).unwrap_err();
        assert!(error.to_string().contains("line 1"));
        assert!(error.to_string().contains("256 KiB"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attachment_uris_dispatch_before_http_and_local_paths() {
        let directory = tempfile::tempdir().unwrap();
        let ctx = attachment_ctx(directory.path());
        let uri = "nole://attachment/00000000-0000-4000-8000-000000000000".to_string();

        let target = resolve_target(&ctx, &json!({ "path": uri })).await.unwrap();
        assert!(
            matches!(&target, Target::Attachment { uri: resolved, .. } if resolved.to_string() == uri)
        );
        assert_eq!(target.kind(), "attachment");
        assert_eq!(target.display(&ctx.root), uri);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_attachment_uris_are_rejected_at_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let ctx = attachment_ctx(directory.path());
        for malformed in [
            "nole://attachment/not-a-uuid".to_string(),
            "nole://attachment/AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA".to_string(),
            "nole://attachment/00000000000000000000000000000000".to_string(),
            "nole://attachment/".to_string(),
            "nole://attachment/00000000-0000-4000-8000-000000000000-extra".to_string(),
        ] {
            assert!(
                resolve_target(&ctx, &json!({ "path": malformed }))
                    .await
                    .is_err(),
                "expected rejection for {malformed}"
            );
        }
        // A non-attachment URL still dispatches to the web target.
        let target = resolve_target(&ctx, &json!({ "path": "https://example.test" }))
            .await
            .unwrap();
        assert!(matches!(target, Target::Web { .. }));
    }

    #[test]
    fn line_selectors_are_one_based_inclusive_and_bounded() {
        assert_eq!(
            split_line_range("data/note.md:50-200").unwrap(),
            (
                "data/note.md",
                Some(LineRange {
                    offset: 49,
                    limit: 151,
                }),
            )
        );
        assert_eq!(
            split_line_range("data/note.md").unwrap(),
            ("data/note.md", None)
        );
        assert!(split_line_range("data/note.md:0-2").is_err());
        assert!(split_line_range("data/note.md:4-3").is_err());
        assert!(split_line_range("data/note.md:1-2001").is_err());
        assert!(line_window(None, &json!({"range": "1-2"})).is_err());
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
        assert!(parsed["total"].is_null());
        assert_eq!(parsed["has_more"], true);
        assert_eq!(parsed["items"], json!(["line 3", "line 4"]));
        assert_eq!(parsed["next"], format!("{uri}:5-6"));
        // Structured read-only content: no hashline `[path#TAG]` snapshot header
        // and no tag field, because attachment reads never gate edit.
        assert!(parsed.get("tag").is_none());
        assert!(!output.contains(&format!("[notes.txt#")));
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
        assert!(parsed["total"].is_null());
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
    async fn direct_reads_of_attachment_internals_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let metadata = store.import_bytes(b"payload", Some("secret.txt")).unwrap();
        let object = directory
            .path()
            .join(ATTACHMENTS_DIR)
            .join(metadata.id.to_string())
            .join("content");
        assert!(object.exists());
        let read = Read::new(
            directory.path(),
            Arc::new(ReadTracker::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let error = read
            .execute(&json!({ "path": object.to_str().unwrap() }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("attachment internals"));
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
