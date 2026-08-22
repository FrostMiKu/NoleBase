//! Note search and listing tools: `list_notes` and `search_files`.
//!
//! Both operate on `data/` `.md`/`.mb` note files (`search_files` also covers
//! `archives/`) and share the `range` pagination shape.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde_json::{json, Value};
use tokio::fs as async_fs;

use super::super::util::{
    display_path, fuzzy_match, range_schema, required_string, RangeSelector, MAX_SEARCH_RESULTS,
};
use super::count_file_lines;
use crate::agent::{canonical_root, Tool, ToolExecutionPolicy};
use crate::storage::is_note_path;

const MAX_NOTE_RESULTS: usize = 2_000;

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
