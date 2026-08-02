//! Read-only filesystem, note search, and unified read tools.
//!
//! The `read` tool is a parser registry: a target (file path, directory path,
//! http(s) URL, or attachment URI) is resolved once, then each registered
//! [`ReadParser`] is asked in order whether it handles that target. Registering
//! a new format (for example a PDF parser) requires no change to the dispatch
//! logic — only a new parser registered before the generic text-file parser.
//! Attachment reads are read-only: they never register an edit snapshot.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde_json::{json, Value};
use tokio::fs as async_fs;

use super::util::{
    display_path, fuzzy_match, optional_usize, required_string, truncate_chars,
    DEFAULT_SEARCH_RESULTS, MAX_FILE_BYTES, MAX_SEARCH_OFFSET, MAX_SEARCH_RESULTS,
    MAX_SEARCH_SNIPPET_CHARS,
};
use super::web::{read_limited_http_body, web_fetch_content};
use crate::agent::{canonical_root, snapshot_tag, ReadTracker, Tool, ToolExecutionPolicy};
use crate::attachment::{AttachmentStore, AttachmentUri};
use crate::storage::ATTACHMENTS_DIR;

const DEFAULT_READ_LINES: usize = 200;
const MAX_READ_LINES: usize = 2_000;
const MAX_NOTE_RESULTS: usize = 2_000;
const MAX_DIRECTORY_RESULTS: usize = 2_000;
const MAX_DIRECTORY_SCAN: usize = 10_000;
const MAX_DIRECTORY_DEPTH: usize = 16;

/// A resolved target for the unified `read` tool.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    File { path: PathBuf },
    Directory { path: PathBuf },
    Web { url: String },
    Attachment { uri: AttachmentUri },
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
            Target::File { path } | Target::Directory { path } => listed_path(root, path),
            Target::Web { url } => url.clone(),
            Target::Attachment { uri } => uri.to_string(),
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
            Box::new(AttachmentParser),
            Box::new(TextFileParser),
        ];
        Ok(Self { ctx, parsers })
    }

    /// Registers a parser ahead of the generic text-file parser so specific
    /// formats (PDF, images, ...) get a chance to claim matching files first.
    /// Intentionally unused until such a parser lands; kept as the documented
    /// extension point for the registry.
    #[allow(dead_code)]
    pub fn register(&mut self, parser: impl ReadParser + 'static) {
        let text_index = self
            .parsers
            .iter()
            .position(|parser| parser.name() == "text_file")
            .unwrap_or(self.parsers.len());
        self.parsers.insert(text_index, Box::new(parser));
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
        "Read any target: a local UTF-8 file as hashline text (`[path#TAG]` plus absolute one-based `N:text` rows), a directory as a typed JSON listing, an http(s) URL as structured extracted content, or an attachment URI as structured read-only content or metadata. File reads are paginated, limited to 1 MB, and their snapshot tag and visible ranges gate edit; attachment content is read-only and never gates edit."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path, directory path, http(s) URL, or nole-attachment:// URI to read"
                },
                "offset": {
                    "type": "integer", "minimum": 0, "default": 0,
                    "description": "Number of preceding lines or entries to skip (file and directory targets)"
                },
                "limit": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_READ_LINES, "default": DEFAULT_READ_LINES,
                    "description": "Maximum lines or entries to return (file and directory targets)"
                },
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
    let requested = required_string(input, "path")?;
    if AttachmentUri::is_attachment_uri(&requested) {
        let uri = AttachmentUri::parse(&requested)?;
        return Ok(Target::Attachment { uri });
    }
    if requested.starts_with("https://") || requested.starts_with("http://") {
        return Ok(Target::Web {
            url: requested.to_string(),
        });
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
        Ok(Target::File { path })
    } else if metadata.is_dir() {
        Ok(Target::Directory { path })
    } else {
        bail!(
            "path is not a regular file or directory: {}",
            path.display()
        )
    }
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
        let Target::File { path } = target else {
            bail!("text_file parser received non-file target");
        };
        let metadata = async_fs::metadata(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            bail!("file must be a regular UTF-8 file no larger than 1 MB");
        }
        let content = async_fs::read_to_string(path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        let offset = optional_usize(input, "offset", 0, usize::MAX)?;
        let limit = optional_usize(input, "limit", DEFAULT_READ_LINES, MAX_READ_LINES)?;
        let lines: Vec<&str> = source_lines(&content);
        let total_lines = lines.len();
        let start = offset.min(total_lines);
        let end = start.saturating_add(limit).min(total_lines);
        let tag = snapshot_tag(&content);
        let target = listed_path(&ctx.root, path);
        let mut output = format!("[{target}#{tag}]");
        for (index, text) in lines[start..end].iter().enumerate() {
            write!(output, "\n{}:{text}", start + index + 1)?;
        }
        let first = if start < end { start + 1 } else { 0 };
        write!(output, "\n\n[Showing lines {first}-{end} of {total_lines}")?;
        if end < total_lines {
            write!(output, ". Use offset {end} to continue")?;
        }
        output.push(']');
        let tracked_tag = ctx
            .reads
            .mark_file(path.clone(), content, start, end, total_lines)?;
        debug_assert_eq!(tag, tracked_tag);
        Ok(ReadPayload::Text(output))
    }
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
        let Target::Attachment { uri } = target else {
            bail!("attachment parser received non-attachment target");
        };
        let metadata = ctx
            .attachments
            .metadata(uri.id())
            .with_context(|| format!("reading attachment {uri}"))?;
        let bytes = ctx
            .attachments
            .read_object(uri.id())
            .with_context(|| format!("reading attachment {uri}"))?;
        let mime = metadata.mime_type.as_deref().unwrap_or("");
        if mime.starts_with("image/") {
            // Images return structured metadata with dimensions when decodable;
            // undecodable image types (for example SVG) still return metadata.
            let mut payload = attachment_metadata_json(*uri, &metadata);
            if let Ok((width, height, format)) = image_dimensions(&bytes) {
                payload["width"] = json!(width);
                payload["height"] = json!(height);
                payload["format"] = json!(format);
            }
            return Ok(ReadPayload::Structured(payload));
        }
        // Read-only attachment content never registers an edit snapshot, so it
        // can never gate a later edit: pagination is a plain content window.
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(_) => {
                return Ok(ReadPayload::Structured(attachment_metadata_json(
                    *uri, &metadata,
                )));
            }
        };
        let offset = optional_usize(input, "offset", 0, usize::MAX)?;
        let limit = optional_usize(input, "limit", DEFAULT_READ_LINES, MAX_READ_LINES)?;
        let lines = source_lines(&content);
        let total_lines = lines.len();
        let start = offset.min(total_lines);
        let end = start.saturating_add(limit).min(total_lines);
        let mut payload = attachment_metadata_json(*uri, &metadata);
        payload["offset"] = json!(start);
        payload["returned"] = json!(end - start);
        payload["total_lines"] = json!(total_lines);
        payload["has_more"] = json!(end < total_lines);
        payload["lines"] = json!(lines[start..end]);
        Ok(ReadPayload::Structured(payload))
    }
}

/// Structured metadata shared by every attachment read result. Physical object
/// paths stay private; the URI is the only address exposed to the model.
fn attachment_metadata_json(
    uri: AttachmentUri,
    metadata: &crate::attachment::AttachmentMetadata,
) -> Value {
    json!({
        "name": attachment_name(&metadata.source),
        "uri": uri.to_string(),
        "mime_type": metadata.mime_type,
        "size": metadata.size,
        "imported_at": metadata.imported_at.to_rfc3339(),
    })
}

/// The import source's basename, falling back to the raw source string.
fn attachment_name(source: &str) -> String {
    Path::new(source)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| source.to_string())
}

/// Decode image dimensions without decoding pixel data.
fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32, String)> {
    use std::io::Cursor;
    let reader = image::ImageReader::new(Cursor::new(bytes))
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
        let offset = optional_usize(input, "offset", 0, usize::MAX)?;
        let limit = optional_usize(input, "limit", 200, MAX_DIRECTORY_RESULTS)?;
        let (mut entries, truncated) = directory_entries(path, depth).await?;
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
        let total = entries.len();
        let start = offset.min(total);
        let end = start.saturating_add(limit).min(total);
        let entries = entries[start..end]
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
        Ok(ReadPayload::Structured(json!({
            "depth": depth,
            "sort_by": sort_by,
            "order": if descending { "desc" } else { "asc" },
            "offset": start,
            "returned": end - start,
            "total": total,
            "has_more": end < total,
            "scan_truncated": truncated,
            "entries": entries,
        })))
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
        _input: &Value,
    ) -> Result<ReadPayload> {
        let Target::Web { url } = target else {
            bail!("web parser received non-web target");
        };
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
        let bytes = read_limited_http_body(response, "response").await?;
        let content = web_fetch_content(content_type.as_deref(), bytes)?;
        Ok(ReadPayload::Structured(json!({ "content": content })))
    }
}

async fn directory_entries(
    root: &Path,
    max_depth: usize,
) -> Result<(Vec<DirectoryEntryMetadata>, bool)> {
    let mut entries = Vec::new();
    let mut directories = vec![(root.to_path_buf(), 1usize)];
    let mut truncated = false;
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
            let line_count = if file_type.is_file() && metadata.len() <= MAX_FILE_BYTES {
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
            if entries.len() >= MAX_DIRECTORY_SCAN {
                truncated = true;
                break;
            }
            if file_type.is_dir() && depth < max_depth {
                directories.push((path, depth + 1));
            }
        }
        if truncated {
            break;
        }
    }
    Ok((entries, truncated))
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
                "offset": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_NOTE_RESULTS, "default": 200
                }
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
        let offset = optional_usize(input, "offset", 0, usize::MAX)?;
        let limit = optional_usize(input, "limit", 200, MAX_NOTE_RESULTS)?;
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
        let total = notes.len();
        let start = offset.min(total);
        let end = start.saturating_add(limit).min(total);
        let entries = notes[start..end]
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
        serde_json::to_string_pretty(&json!({
            "sort_by": sort_by,
            "order": if descending { "desc" } else { "asc" },
            "offset": start,
            "returned": end - start,
            "total": total,
            "has_more": end < total,
            "entries": entries,
        }))
        .context("encoding note listing")
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
        "Case-insensitive full-text search across managed Markdown files. Returns paths and matching zero-based source lines with result pagination."
    }

    fn input_schema(&self) -> Value {
        search_schema("Text to find in managed Markdown file contents")
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let query = required_string(input, "query")?.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        let offset = optional_usize(input, "offset", 0, MAX_SEARCH_OFFSET)?;
        let limit = optional_usize(input, "limit", DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS)?;
        let mut matches = Vec::new();
        let lowercase_query = query.to_lowercase();
        'directories: for directory in &self.directories {
            for file in list_note_files_in(directory).await? {
                let Ok(source) = async_fs::read_to_string(&file.path).await else {
                    continue;
                };
                for (line, text) in source.lines().enumerate() {
                    let snippet = text.trim();
                    if !snippet.is_empty() && text.to_lowercase().contains(&lowercase_query) {
                        matches.push(json!({
                            "path": display_path(&self.root, &file.path),
                            "line": line,
                            "snippet": truncate_chars(snippet, MAX_SEARCH_SNIPPET_CHARS),
                        }));
                        if matches.len() >= 200 {
                            break 'directories;
                        }
                    }
                }
            }
        }
        paginated_search_result(query, offset, limit, matches)
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
        let offset = optional_usize(input, "offset", 0, MAX_SEARCH_OFFSET)?;
        let limit = optional_usize(input, "limit", DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS)?;
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
        paginated_search_result(query, offset, limit, matches)
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

fn source_lines(content: &str) -> Vec<&str> {
    content
        .split_inclusive('\n')
        .map(|line| {
            let line = line.strip_suffix('\n').unwrap_or(line);
            line.strip_suffix('\r').unwrap_or(line)
        })
        .collect()
}

fn search_schema(query_description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": query_description },
            "offset": {
                "type": "integer", "minimum": 0,
                "maximum": MAX_SEARCH_OFFSET, "default": 0
            },
            "limit": {
                "type": "integer", "minimum": 1,
                "maximum": MAX_SEARCH_RESULTS, "default": DEFAULT_SEARCH_RESULTS
            }
        },
        "required": ["query"], "additionalProperties": false
    })
}

fn paginated_search_result(
    query: &str,
    offset: usize,
    limit: usize,
    matches: Vec<Value>,
) -> Result<String> {
    let total = matches.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    serde_json::to_string_pretty(&json!({
        "query": query,
        "offset": start,
        "returned": end - start,
        "total_matches": total,
        "has_more": end < total,
        "matches": &matches[start..end],
    }))
    .context("encoding search results")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

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

    #[tokio::test(flavor = "current_thread")]
    async fn attachment_uris_dispatch_before_http_and_local_paths() {
        let directory = tempfile::tempdir().unwrap();
        let ctx = attachment_ctx(directory.path());
        let hex = "ab".repeat(32);
        let uri = format!("nole-attachment://sha256/{hex}");

        let target = resolve_target(&ctx, &json!({ "path": uri })).await.unwrap();
        assert!(
            matches!(&target, Target::Attachment { uri: resolved } if resolved.to_string() == uri)
        );
        assert_eq!(target.kind(), "attachment");
        assert_eq!(target.display(&ctx.root), uri);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_attachment_uris_are_rejected_at_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let ctx = attachment_ctx(directory.path());
        for malformed in [
            "nole-attachment://sha256/not-hex".to_string(),
            format!("nole-attachment://sha256/{}", "AB".repeat(32)),
            format!("nole-attachment://sha256/{}", "ab".repeat(31)),
            "nole-attachment://sha256/".to_string(),
            "nole-attachment://md5/ab".to_string(),
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
            .execute(&json!({ "path": uri, "offset": 2, "limit": 2 }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["kind"], "attachment");
        assert_eq!(parsed["target"], uri);
        assert_eq!(parsed["name"], "notes.txt");
        assert_eq!(parsed["mime_type"], "text/plain");
        assert_eq!(parsed["size"], content.len() as u64);
        assert_eq!(parsed["offset"], 2);
        assert_eq!(parsed["returned"], 2);
        assert_eq!(parsed["total_lines"], 5);
        assert_eq!(parsed["has_more"], true);
        assert_eq!(parsed["lines"], json!(["line 3", "line 4"]));
        // Structured read-only content: no hashline `[path#TAG]` snapshot header
        // and no tag field, because attachment reads never gate edit.
        assert!(parsed.get("tag").is_none());
        assert!(!output.contains(&format!("[notes.txt#")));
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
            .join("objects")
            .join(metadata.id.to_hex());
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
        let uri = format!("nole-attachment://sha256/{}", "ab".repeat(32));
        let error = read.execute(&json!({ "path": uri })).await.unwrap_err();
        // anyhow Display shows only the outer context; the "no such attachment"
        // cause is visible in the Debug chain.
        assert!(format!("{error:?}").contains("no such attachment"));
        assert!(!format!("{error:?}").contains("objects"));
    }
}
