//! Tag and daily-note tools backed by the workspace index and storage.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::util::{
    display_path, fuzzy_match, limited_diff, range_schema, required_string, truncate_chars,
    truncate_with_marker, RangeSelector, MAX_DIFF_BYTES, MAX_SEARCH_RESULTS,
    MAX_SEARCH_SNIPPET_CHARS,
};
use crate::core::agent::{
    canonical_root, ApprovalGate, ApprovalKind, ApprovalRequest, Tool, ToolExecutionPolicy,
};
use crate::core::model::SearchHit;
use crate::core::storage::Storage;
use crate::core::workspace_index::{TagRenamePlan, TagScope, WorkspaceIndexHandle};

pub struct Tags {
    index: WorkspaceIndexHandle,
}

impl Tags {
    pub fn new(index: WorkspaceIndexHandle) -> Self {
        Self { index }
    }
}

#[async_trait::async_trait]
impl Tool for Tags {
    fn name(&self) -> &'static str {
        "tags"
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

    async fn execute(&self, input: &Value) -> Result<crate::core::agent::ToolOutput> {
        let content: Result<String> = async {
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
            let mut result = format!(
                "Query: {query}\nScope: {}\nSort: {sort_by} {}\nTags: {} of {}\nRange: {}",
                tag_scope_label(scope),
                if descending { "desc" } else { "asc" },
                page.returned(),
                page.total,
                selector.as_string()
            );
            if let Some(next) = page.next() {
                result.push_str(&format!("\nNext range: {next}"));
            }
            for tag in &tags[page.start_index..page.end_index] {
                result.push_str(&format!(
                    "\n\n#{}\nDocuments: {}\nMentions: {}",
                    tag.name, tag.documents, tag.mentions
                ));
            }
            Ok(result)
        }
        .await;
        let content = content?;
        Ok(crate::core::agent::ToolOutput::text(content))
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

    async fn execute(&self, input: &Value) -> Result<crate::core::agent::ToolOutput> {
        let content: Result<String> = async {
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
                    } => Some(format!(
                        "{}:{line_no}: {}",
                        display_path(&self.root, path),
                        truncate_chars(text, MAX_SEARCH_SNIPPET_CHARS)
                    )),
                    SearchHit::DocumentLine { .. } => None,
                })
                .collect::<Vec<_>>();
            let page = selector.window(items.len());
            let mut result = format!(
                "Tag: #{}\nScope: {}\nMatches: {} of {}\nRange: {}",
                tag.trim_start_matches('#'),
                tag_scope_label(scope),
                page.returned(),
                page.total,
                selector.as_string()
            );
            if let Some(next) = page.next() {
                result.push_str(&format!("\nNext range: {next}"));
            }
            if page.returned() > 0 {
                result.push_str("\n\n");
                result.push_str(&items[page.start_index..page.end_index].join("\n"));
            }
            Ok(result)
        }
        .await;
        let content = content?;
        Ok(crate::core::agent::ToolOutput::text(content))
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

    async fn execute(&self, input: &Value) -> Result<crate::core::agent::ToolOutput> {
        let content: Result<String> = async {
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
                    diff = truncate_with_marker(&diff, MAX_DIFF_BYTES);
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
            let mut result = format!(
                "Renamed: #{} -> #{}\nDocuments: {}\nMentions: {}",
                outcome.from, outcome.to, outcome.documents, outcome.mentions
            );
            for path in &outcome.paths {
                result.push_str(&format!("\nPath: {}", display_path(&self.root, path)));
            }
            Ok::<String, anyhow::Error>(result)
        }
        .await;
        let content = content?;
        Ok(crate::core::agent::ToolOutput::text(content))
    }
}

pub struct Daily {
    storage: Storage,
    gate: ApprovalGate,
}

impl Daily {
    pub fn new(root: &Path, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Daily {
    fn name(&self) -> &'static str {
        "daily"
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

    async fn execute(&self, input: &Value) -> Result<crate::core::agent::ToolOutput> {
        let content: Result<String> = async {
            let content = required_string(input, "content")?;
            let requested_date = input
                .get("date")
                .map(|date| {
                    date.as_str()
                        .context("field date must be a string")
                        .map(str::to_owned)
                })
                .transpose()?;
            self.gate
                .request(ApprovalRequest {
                    title: "Add daily entry".into(),
                    message: format!(
                        "Add to {}: {content}",
                        requested_date.as_deref().unwrap_or("today")
                    ),
                    kind: ApprovalKind::Confirm,
                })
                .await?;
            let note = match requested_date {
                Some(date) => self.storage.append_daily(&date, content)?,
                None => self.storage.append_to_today(content)?,
            };
            Ok(format!("Added daily entry\nDate: {}", note.date))
        }
        .await;
        let content = content?;
        Ok(crate::core::agent::ToolOutput::text(content))
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

#[cfg(test)]
mod daily_approval_tests {
    use super::*;
    use crate::core::agent::test_support::{event_channel, gate, test_runtime};
    use crate::core::agent::{ApprovalDecision, PermissionMode};

    #[test]
    fn denying_a_daily_entry_preserves_the_existing_note() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage.append_daily("2026-11-14", "Original").unwrap();
        let path = directory.path().join("daily/2026-11-14.md");
        let before = std::fs::read(&path).unwrap();
        let (events, _receiver) = event_channel();
        let (decisions, receiver) = tokio::sync::mpsc::unbounded_channel();
        decisions.send(ApprovalDecision::Deny).unwrap();
        let tool = Daily::new(
            directory.path(),
            gate(PermissionMode::Approve, directory.path(), events, receiver),
        )
        .unwrap();
        assert!(test_runtime()
            .block_on(tool.execute(&json!({"date":"2026-11-14","content":"Change"})))
            .is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}
