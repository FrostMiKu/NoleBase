//! Directory listing parser for the unified `read` tool.
//!
//! Walks the tree up to a bounded depth and returns entries sorted by the
//! requested key with the shared `range` pagination.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde_json::{json, Value};
use tokio::fs as async_fs;

use super::super::util::{optional_usize, RangeSelector};
use super::{
    count_file_lines, listed_path, ParseContext, ReadParser, ReadPayload, Target,
    MAX_DIRECTORY_DEPTH, MAX_DIRECTORY_RESULTS,
};

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

pub(crate) struct DirectoryParser;

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
        let payload = json!({
            "depth": depth,
            "sort_by": sort_by,
            "order": if descending { "desc" } else { "asc" },
            "range": selector.as_string(),
            "returned": page.returned(),
            "total": page.total,
            "has_more": page.has_more(),
            "items": items,
        });
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
            let line_count = if file_type.is_file() {
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
