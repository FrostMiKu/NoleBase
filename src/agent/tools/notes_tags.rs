//! Tag and daily-note tools backed by the workspace index and storage.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::util::{
    display_path, fuzzy_match, limited_diff, range_schema, required_string, truncate_chars,
    RangeSelector, MAX_DIFF_BYTES, MAX_SEARCH_RESULTS, MAX_SEARCH_SNIPPET_CHARS,
};
use super::write_policy::{validate_write, WriteSource};
use crate::agent::{
    canonical_root, ApprovalGate, ApprovalKind, ApprovalRequest, Tool, ToolExecutionPolicy,
};
use crate::model::SearchHit;
use crate::storage::Storage;
use crate::workspace_index::{TagRenamePlan, TagScope, WorkspaceIndexHandle};

pub struct ListTags {
    index: WorkspaceIndexHandle,
}

impl ListTags {
    pub fn new(index: WorkspaceIndexHandle) -> Self {
        Self { index }
    }
}

#[async_trait::async_trait]
impl Tool for ListTags {
    fn name(&self) -> &'static str {
        "list_tags"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "List Hashtags with document and mention counts. Supports fuzzy filtering, workspace scope, sorting, and pagination."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Optional fuzzy tag-name filter" },
                "scope": {
                    "type": "string", "enum": ["all", "daily", "notes", "archives"],
                    "default": "all"
                },
                "sort_by": {
                    "type": "string", "enum": ["documents", "mentions", "name"],
                    "default": "documents"
                },
                "order": { "type": "string", "enum": ["asc", "desc"], "default": "desc" },
                "range": range_schema(MAX_SEARCH_RESULTS)
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let scope = tag_scope(input)?;
        let sort_by = input
            .get("sort_by")
            .and_then(Value::as_str)
            .unwrap_or("documents");
        if !matches!(sort_by, "documents" | "mentions" | "name") {
            bail!("unsupported sort_by: {sort_by}");
        }
        let descending = match input.get("order").and_then(Value::as_str).unwrap_or("desc") {
            "asc" => false,
            "desc" => true,
            other => bail!("unsupported order: {other}"),
        };
        let selector = RangeSelector::from_input(input, MAX_SEARCH_RESULTS)?;
        let mut tags = self
            .index
            .with_index(|index| index.tags_scoped(scope))
            .context("workspace tag index is still building")?;
        if !query.is_empty() {
            tags.retain(|tag| fuzzy_match(&tag.name, query));
        }
        tags.sort_by(|left, right| {
            let ordering = match sort_by {
                "documents" => left.documents.cmp(&right.documents),
                "mentions" => left.mentions.cmp(&right.mentions),
                "name" => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
                _ => unreachable!(),
            };
            let ordering = if descending {
                ordering.reverse()
            } else {
                ordering
            };
            ordering.then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        let page = selector.window(tags.len());
        let items = tags[page.start_index..page.end_index]
            .iter()
            .map(|tag| {
                json!({
                    "tag": tag.name,
                    "documents": tag.documents,
                    "mentions": tag.mentions,
                })
            })
            .collect::<Vec<_>>();
        let mut result = json!({
            "query": query,
            "scope": tag_scope_label(scope),
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
        serde_json::to_string_pretty(&result).context("encoding tag list")
    }
}

pub struct SearchTag {
    root: PathBuf,
    index: WorkspaceIndexHandle,
}

impl SearchTag {
    pub fn new(root: &Path, index: WorkspaceIndexHandle) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            index,
        })
    }
}

#[async_trait::async_trait]
impl Tool for SearchTag {
    fn name(&self) -> &'static str {
        "search_tag"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Search one exact Hashtag across Markdown files. Returns paths, one-based source line numbers, and source snippets with range pagination."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tag": { "type": "string", "description": "Exact tag name, with or without #" },
                "scope": {
                    "type": "string", "enum": ["all", "daily", "notes", "archives"],
                    "default": "all"
                },
                "range": range_schema(MAX_SEARCH_RESULTS)
            },
            "required": ["tag"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let tag = required_string(input, "tag")?.trim();
        if tag.is_empty() {
            bail!("tag must not be empty");
        }
        let scope = tag_scope(input)?;
        let selector = RangeSelector::from_input(input, MAX_SEARCH_RESULTS)?;
        let hits = self
            .index
            .with_index(|index| index.exact_tag_hits(tag, scope))
            .context("workspace tag index is still building")?;
        let items = hits
            .iter()
            .filter_map(|hit| match hit {
                SearchHit::FileLine {
                    path,
                    line_no,
                    text,
                } => Some(json!({
                    "path": display_path(&self.root, path),
                    "line": line_no,
                    "snippet": truncate_chars(text, MAX_SEARCH_SNIPPET_CHARS),
                })),
                SearchHit::DocumentLine { .. } => None,
            })
            .collect::<Vec<_>>();
        let page = selector.window(items.len());
        let mut result = json!({
            "tag": tag.trim_start_matches('#'),
            "scope": tag_scope_label(scope),
            "range": selector.as_string(),
            "returned": page.returned(),
            "total": page.total,
            "has_more": page.has_more(),
            "items": &items[page.start_index..page.end_index],
        });
        if let Some(next) = page.next() {
            result["next"] = json!(next);
        }
        serde_json::to_string_pretty(&result).context("encoding tag search")
    }
}

pub struct RenameTag {
    storage: Storage,
    root: PathBuf,
    index: WorkspaceIndexHandle,
    gate: ApprovalGate,
}

impl RenameTag {
    pub fn new(root: &Path, index: WorkspaceIndexHandle, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            root: canonical_root(root)?,
            index,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for RenameTag {
    fn name(&self) -> &'static str {
        "rename_tag"
    }

    fn description(&self) -> &'static str {
        "Rename one exact Hashtag across daily notes, active notes, and archives."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Existing exact tag, with or without #" },
                "to": { "type": "string", "description": "New valid tag, with or without #" }
            },
            "required": ["from", "to"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let from = required_string(input, "from")?.trim();
        let to = required_string(input, "to")?.trim();
        let paths = self
            .index
            .with_index(|index| index.tag_paths(from))
            .context("workspace tag index is still building")?;
        let plan = TagRenamePlan::prepare(&self.storage, paths, from, to)?;
        let mut diff = format!(
            "Rename #{} to #{} in {} documents ({} mentions)\n\n",
            plan.from,
            plan.to,
            plan.documents(),
            plan.mentions()
        );
        for (path, before, after, _) in plan.changes() {
            let label = display_path(&self.root, path);
            diff.push_str(&limited_diff(before, after, &label, &label));
            diff.push('\n');
            if diff.len() > MAX_DIFF_BYTES {
                let mut end = MAX_DIFF_BYTES;
                while !diff.is_char_boundary(end) {
                    end -= 1;
                }
                diff.truncate(end);
                diff.push_str("\n... diff truncated ...\n");
                break;
            }
        }
        self.gate
            .request(ApprovalRequest {
                title: format!("Rename #{} to #{}", plan.from, plan.to),
                message: diff,
                kind: ApprovalKind::Diff,
            })
            .await?;
        let outcome = plan.apply()?;
        self.index
            .refresh_paths(&self.storage, outcome.paths.clone());
        serde_json::to_string_pretty(&json!({
            "from": outcome.from,
            "to": outcome.to,
            "documents": outcome.documents,
            "mentions": outcome.mentions,
            "paths": outcome
                .paths
                .iter()
                .map(|path| display_path(&self.root, path))
                .collect::<Vec<_>>(),
        }))
        .context("encoding tag rename result")
    }
}

pub struct AddDailyEntry {
    storage: Storage,
}

impl AddDailyEntry {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for AddDailyEntry {
    fn name(&self) -> &'static str {
        "add_daily_entry"
    }

    fn description(&self) -> &'static str {
        "Add content to daily/YYYY-MM-DD.md, creating the file if absent and otherwise appending it after a blank line."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": {
                "date": {
                    "type": "string",
                    "description": "Local calendar date in YYYY-MM-DD format. Omit to append to today's daily note."
                },
                "content": { "type": "string" }
            },
            "required": ["content"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let content = required_string(input, "content")?;
        let requested_date = input
            .get("date")
            .map(|date| {
                date.as_str()
                    .context("field date must be a string")
                    .map(str::to_owned)
            })
            .transpose()?;
        let date = requested_date
            .clone()
            .unwrap_or_else(|| chrono::Local::now().date_naive().to_string());
        let path = self.storage.daily_file_path(&date)?;
        let mut candidate = match std::fs::read_to_string(&path) {
            Ok(existing) => existing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        if !candidate.is_empty() {
            candidate.push('\n');
        }
        candidate.push_str(content);
        candidate.push('\n');
        validate_write(&self.storage.root, &path, WriteSource::Text(&candidate))?;
        let note = match requested_date {
            Some(date) => self.storage.append_daily(&date, content)?,
            None => self.storage.append_to_today(content)?,
        };
        serde_json::to_string(&json!({ "date": note.date.to_string() }))
            .context("encoding daily result")
    }
}

fn tag_scope(input: &Value) -> Result<Option<TagScope>> {
    Ok(
        match input.get("scope").and_then(Value::as_str).unwrap_or("all") {
            "all" => None,
            "daily" => Some(TagScope::Daily),
            "notes" => Some(TagScope::Notes),
            "archives" => Some(TagScope::Archives),
            other => bail!("unsupported tag scope: {other}"),
        },
    )
}

fn tag_scope_label(scope: Option<TagScope>) -> &'static str {
    match scope {
        None => "all",
        Some(TagScope::Daily) => "daily",
        Some(TagScope::Notes) => "notes",
        Some(TagScope::Archives) => "archives",
    }
}
