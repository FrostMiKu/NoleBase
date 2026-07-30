//! Read-only filesystem and note search tools.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde_json::{json, Value};

use super::util::{
    MAX_FILE_BYTES, DEFAULT_SEARCH_RESULTS, MAX_SEARCH_OFFSET, MAX_SEARCH_RESULTS,
    MAX_SEARCH_SNIPPET_CHARS, display_path, fuzzy_match, optional_usize, required_string,
    truncate_chars,
};
use crate::agent::{ReadTracker, Tool, canonical_root};
use crate::model::SearchHit;
use crate::storage::Storage;

const DEFAULT_READ_LINES: usize = 200;
const MAX_READ_LINES: usize = 2_000;
const MAX_NOTE_RESULTS: usize = 2_000;
const MAX_DIRECTORY_RESULTS: usize = 2_000;
const MAX_DIRECTORY_SCAN: usize = 10_000;
const MAX_DIRECTORY_DEPTH: usize = 16;

pub struct ReadFile {
    root: PathBuf,
    private_config: PathBuf,
    reads: Arc<ReadTracker>,
}

impl ReadFile {
    pub fn new(root: &Path, reads: Arc<ReadTracker>) -> Result<Self> {
        let root = canonical_root(root)?;
        let private_config = fs::canonicalize(root.join("config/ai.toml"))
            .unwrap_or_else(|_| root.join("config/ai.toml"));
        Ok(Self {
            private_config,
            reads,
            root,
        })
    }
}

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a paginated range from any UTF-8 text file by absolute path, or by a path relative to the Nole root (maximum 1 MB). offset is a zero-based line number. The response includes every returned line's absolute zero-based line number and text without its line ending."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_READ_LINES, "default": DEFAULT_READ_LINES
                }
            },
            "required": ["path"], "additionalProperties": false
        })
    }
    async fn execute(&self, input: &Value) -> Result<String> {
        let path = required_string(input, "path")?;
        let path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.root.join(path)
        };
        let path =
            fs::canonicalize(&path).with_context(|| format!("resolving {}", path.display()))?;
        if path == self.private_config {
            bail!("AI configuration is private");
        }
        let metadata =
            fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            bail!("file must be a regular UTF-8 file no larger than 1 MB");
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let offset = optional_usize(input, "offset", 0, usize::MAX)?;
        let limit = optional_usize(input, "limit", DEFAULT_READ_LINES, MAX_READ_LINES)?;
        let lines: Vec<&str> = source_lines(&content);
        let total_lines = lines.len();
        let start = offset.min(total_lines);
        let end = start.saturating_add(limit).min(total_lines);
        let returned_lines = lines[start..end]
            .iter()
            .enumerate()
            .map(|(index, text)| json!({ "line": start + index, "text": text }))
            .collect::<Vec<_>>();
        self.reads
            .mark_file(path.clone(), content, start, end, total_lines)?;
        serde_json::to_string_pretty(&json!({
            "path": display_path(&self.root, &path),
            "offset": start,
            "returned_lines": end - start,
            "total_lines": total_lines,
            "has_more": end < total_lines,
            "lines": returned_lines,
        }))
        .context("encoding file read")
    }
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

pub struct ListDirectory {
    root: PathBuf,
}

impl ListDirectory {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for ListDirectory {
    fn name(&self) -> &'static str {
        "list_directory"
    }

    fn description(&self) -> &'static str {
        "List files and subdirectories in any directory with type, nesting depth, extension, byte size, line count, creation time, and modification time. depth=1 lists direct children; larger values recurse without following symlinks. Supports metadata sorting and pagination."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "depth": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_DIRECTORY_DEPTH, "default": 1
                },
                "sort_by": {
                    "type": "string",
                    "enum": ["name", "type", "depth", "line_count", "created_at", "modified_at", "size"],
                    "default": "name"
                },
                "order": { "type": "string", "enum": ["asc", "desc"], "default": "asc" },
                "offset": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_DIRECTORY_RESULTS, "default": 200
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let requested = input.get("path").and_then(Value::as_str).unwrap_or(".");
        let requested_path = Path::new(requested);
        let directory = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            self.root.join(requested_path)
        };
        let directory = fs::canonicalize(&directory)
            .with_context(|| format!("resolving directory {}", directory.display()))?;
        if !fs::metadata(&directory)
            .with_context(|| format!("reading metadata for {}", directory.display()))?
            .is_dir()
        {
            bail!("path is not a directory: {}", directory.display());
        }

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
        let (mut entries, truncated) = directory_entries(&directory, depth)?;
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
                    "path": listed_path(&self.root, &entry.path),
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
        serde_json::to_string_pretty(&json!({
            "directory": listed_path(&self.root, &directory),
            "depth": depth,
            "sort_by": sort_by,
            "order": if descending { "desc" } else { "asc" },
            "offset": start,
            "returned": end - start,
            "total": total,
            "has_more": end < total,
            "scan_truncated": truncated,
            "entries": entries,
        }))
        .context("encoding directory listing")
    }
}

fn directory_entries(root: &Path, max_depth: usize) -> Result<(Vec<DirectoryEntryMetadata>, bool)> {
    let mut entries = Vec::new();
    let mut directories = vec![(root.to_path_buf(), 1usize)];
    let mut truncated = false;
    while let Some((directory, depth)) = directories.pop() {
        let children = fs::read_dir(&directory)
            .with_context(|| format!("listing directory {}", directory.display()))?;
        for child in children {
            let child =
                child.with_context(|| format!("listing directory {}", directory.display()))?;
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)
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
                count_file_lines(&path).ok()
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

fn count_file_lines(path: &Path) -> Result<u64> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    let mut buffer = Vec::new();
    let mut line_count = 0u64;
    while reader.read_until(b'\n', &mut buffer)? != 0 {
        line_count += 1;
        buffer.clear();
    }
    Ok(line_count)
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
    storage: Storage,
    root: PathBuf,
}

impl ListNotes {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for ListNotes {
    fn name(&self) -> &'static str {
        "list_notes"
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
        let mut notes = self
            .storage
            .list_note_files()?
            .into_iter()
            .map(|note| note_metadata(note.path))
            .collect::<Result<Vec<_>>>()?;
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

fn note_metadata(path: PathBuf) -> Result<NoteMetadata> {
    let metadata =
        fs::metadata(&path).with_context(|| format!("reading metadata for {}", path.display()))?;
    let line_count = count_file_lines(&path)?;
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
    storage: Storage,
    root: PathBuf,
}

impl SearchContent {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for SearchContent {
    fn name(&self) -> &'static str {
        "search_content"
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
        for hit in self.storage.search_file_lines(query) {
            if let SearchHit::FileLine {
                path,
                line_no,
                text,
            } = hit
            {
                matches.push(json!({
                    "path": display_path(&self.root, &path),
                    "line": line_no.saturating_sub(1),
                    "snippet": truncate_chars(&text, MAX_SEARCH_SNIPPET_CHARS),
                }));
            }
        }
        paginated_search_result(query, offset, limit, matches)
    }
}

pub struct SearchFiles {
    storage: Storage,
    root: PathBuf,
}

impl SearchFiles {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for SearchFiles {
    fn name(&self) -> &'static str {
        "search_files"
    }

    fn description(&self) -> &'static str {
        "Fuzzy, case-insensitive filename search across active and archived .md/.mb notes, using the same matching as the Files sidebar. Supports result pagination."
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
        let matches = self
            .storage
            .list_note_files()?
            .into_iter()
            .chain(self.storage.list_archived_note_files()?)
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

fn source_lines(content: &str) -> Vec<&str> {
    content
        .split_inclusive('\n')
        .map(|line| {
            line.strip_suffix('\n')
                .and_then(|line| line.strip_suffix('\r'))
                .unwrap_or(line)
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
