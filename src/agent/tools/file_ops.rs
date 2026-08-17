//! File mutation tools: write, edit, copy, move, rename, and delete.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::file_edit::{inspect_text_file, prepare_edit};
use super::util::{display_path, required_string};
use super::workspace_quota::{
    check_workspace_staged_write, check_workspace_write, check_workspace_writes,
    copy_with_workspace_limits, workspace_dir, workspace_edit_budget,
};
use super::write_policy::{
    mbdown_warning, post_write_result, validate_write_preconditions, WriteSource,
    REPAIR_REQUIRED_MARKER,
};
use crate::agent::{
    canonical_root, AgentEvent, AgentEventSender, ApprovalGate, ApprovalKind, ApprovalRequest,
    ReadTracker, Tool,
};
use crate::export::ExportFormat;
use crate::storage::{ExportDestinationPolicy, ExportOutcome, Storage, ATTACHMENTS_DIR};

pub struct Edit {
    root: PathBuf,
    gate: ApprovalGate,
    reads: Arc<ReadTracker>,
}

impl Edit {
    pub fn new(root: &Path, gate: ApprovalGate, reads: Arc<ReadTracker>) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self { root, gate, reads })
    }
}

#[async_trait::async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Apply one or more line-based replacements or insertions to an existing UTF-8 file using the path and snapshot tag returned by read."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "tag": {
                    "type": "string",
                    "pattern": "^[0-9A-Fa-f]{4}$",
                    "description": "Four-hex snapshot tag from the latest `[path#TAG]` read header"
                },
                "edits": {
                    "type": "array", "minItems": 1, "maxItems": 100,
                    "items": {
                        "oneOf": [
                            {
                                "type": "object",
                                "properties": {
                                    "operation": { "type": "string", "const": "replace" },
                                    "start_line": {
                                        "type": "integer", "minimum": 1,
                                        "description": "One-based first line to replace, inclusive"
                                    },
                                    "end_line": {
                                        "type": "integer", "minimum": 1,
                                        "description": "One-based last line to replace, inclusive"
                                    },
                                    "lines": {
                                        "type": "array",
                                        "description": "Complete replacement lines without line-ending characters. Use an empty array to delete the inclusive range",
                                        "items": { "type": "string", "pattern": "^[^\\r\\n]*$" }
                                    }
                                },
                                "required": ["operation", "start_line", "end_line", "lines"],
                                "additionalProperties": false
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "operation": { "type": "string", "const": "insert" },
                                    "line": {
                                        "type": "integer", "minimum": 1,
                                        "description": "One-based source line used as the insertion anchor"
                                    },
                                    "position": {
                                        "type": "string", "enum": ["before", "after"],
                                        "description": "Insert before or after the anchor line; use before line 1 for an empty file"
                                    },
                                    "lines": {
                                        "type": "array", "minItems": 1,
                                        "description": "Complete lines to insert without line-ending characters",
                                        "items": { "type": "string", "pattern": "^[^\\r\\n]*$" }
                                    }
                                },
                                "required": ["operation", "line", "position", "lines"],
                                "additionalProperties": false
                            }
                        ]
                    }
                }
            },
            "required": ["path", "tag", "edits"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let tag = required_string(input, "tag")?;
        let edits = parse_line_edits(input)?;
        let unresolved = safe_relative(&self.root, relative)?;
        if fs::symlink_metadata(&unresolved)?.file_type().is_symlink() {
            bail!("refusing to edit through a symlink");
        }
        let path = fs::canonicalize(&unresolved)
            .with_context(|| format!("resolving existing file {}", unresolved.display()))?;
        match path_zone(&self.root, &path) {
            PathZone::Config => bail!("edit cannot operate inside config/"),
            PathZone::Attachments => {
                bail!("generic file tools cannot operate inside attachments/")
            }
            _ => {}
        }
        let state = self
            .reads
            .file_state(&path)?
            .context("edit requires read on the same path first")?;
        if !state.tag.eq_ignore_ascii_case(tag) {
            bail!("snapshot tag mismatch for {relative}; read the file again before editing");
        }
        let inspection = inspect_text_file(&path)?;
        if state.identity != inspection.identity {
            self.reads.consume_file(&path)?;
            bail!("file changed since read; read it again before editing");
        }
        let total_lines = inspection.total_lines;
        for edit in &edits {
            if edit.end_line_exclusive > total_lines {
                if edit.insertion {
                    bail!(
                        "invalid insertion anchor for line {} in file with {total_lines} lines",
                        edit.anchor_line
                    );
                }
                bail!(
                    "invalid inclusive edit range {} through {} for file with {total_lines} lines",
                    edit.start_line + 1,
                    edit.end_line_exclusive
                );
            }
            state.ensure_edit_read(edit.start_line, edit.end_line_exclusive)?;
        }

        let budget = workspace_edit_budget(&self.root, &path)?;
        let prepared = match prepare_edit(&path, relative, &edits, &inspection, budget) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.reads.consume_file(&path)?;
                return Err(error);
            }
        };
        if prepared.candidate_identity == prepared.original_identity {
            return Ok(format!("no changes needed for {relative}"));
        }
        validate_write_preconditions(&self.root, &path, WriteSource::File(prepared.path()))?;
        if outside_agent_workspace(&self.root, &path) {
            self.gate
                .request(ApprovalRequest {
                    title: format!("Edit {relative}"),
                    message: prepared.diff.clone(),
                    kind: ApprovalKind::Diff,
                })
                .await?;
        }
        let current = inspect_text_file(&path)?;
        if current.identity != prepared.original_identity {
            self.reads.consume_file(&path)?;
            bail!("file changed before editing; read it again and retry");
        }
        check_workspace_staged_write(&self.root, &path, prepared.path(), prepared.candidate_len)?;
        prepared.publish(&path)?;
        self.reads.consume_file(&path)?;
        Ok(post_write_result(
            format!("edited {relative}"),
            relative,
            &path,
        ))
    }
}

#[derive(Debug)]
pub struct LineEdit {
    pub(super) start_line: usize,
    pub(super) end_line_exclusive: usize,
    pub(super) lines: Vec<String>,
    pub(super) insertion: bool,
    pub(super) anchor_line: usize,
}

pub fn parse_line_edits(input: &Value) -> Result<Vec<LineEdit>> {
    let values = input
        .get("edits")
        .and_then(Value::as_array)
        .context("field edits must be an array")?;
    if values.is_empty() || values.len() > 100 {
        bail!("edits must contain between 1 and 100 entries");
    }
    let mut edits = values
        .iter()
        .map(|value| {
            let lines = parse_edit_lines(value)?;
            match value.get("operation").and_then(Value::as_str) {
                Some("replace") => {
                    let start_line = edit_line_number(value, "start_line")?;
                    let end_line = edit_line_number(value, "end_line")?;
                    if start_line > end_line {
                        bail!("replace start_line must not exceed inclusive end_line");
                    }
                    Ok(LineEdit {
                        start_line: start_line - 1,
                        end_line_exclusive: end_line,
                        lines,
                        insertion: false,
                        anchor_line: start_line,
                    })
                }
                Some("insert") => {
                    if lines.is_empty() {
                        bail!("insert lines must not be empty");
                    }
                    let line = edit_line_number(value, "line")?;
                    let position = value
                        .get("position")
                        .and_then(Value::as_str)
                        .context("insert position must be 'before' or 'after'")?;
                    let start_line = match position {
                        "before" => line - 1,
                        "after" => line,
                        other => bail!("unsupported insert position: {other}"),
                    };
                    Ok(LineEdit {
                        start_line,
                        end_line_exclusive: start_line,
                        lines,
                        insertion: true,
                        anchor_line: line,
                    })
                }
                Some(operation) => bail!("unsupported edit operation: {operation}"),
                None => bail!("edit operation must be 'replace' or 'insert'"),
            }
        })
        .collect::<Result<Vec<_>>>()?;
    edits.sort_by_key(|edit| (edit.start_line, edit.end_line_exclusive));
    for pair in edits.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if current.start_line < previous.end_line_exclusive
            || current.start_line == previous.start_line
        {
            bail!("edits must not overlap or share a start_line");
        }
    }
    Ok(edits)
}

fn edit_line_number(value: &Value, field: &str) -> Result<usize> {
    let line = value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|line| *line > 0)
        .with_context(|| format!("edit {field} must be a positive one-based integer"))?;
    usize::try_from(line).with_context(|| format!("{field} is too large"))
}

fn parse_edit_lines(value: &Value) -> Result<Vec<String>> {
    value
        .get("lines")
        .and_then(Value::as_array)
        .context("edit lines must be an array")?
        .iter()
        .map(|line| {
            let line = line.as_str().context("each edit line must be a string")?;
            if line.contains('\r') || line.contains('\n') {
                bail!("edit lines must not contain line-ending characters");
            }
            Ok(line.to_string())
        })
        .collect()
}

fn safe_relative(root: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("path must stay within the Nole root");
    }
    let path = root.join(relative);
    let parent = path.parent().context("path has no parent")?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("resolving parent directory {}", parent.display()))?;
    if !canonical_parent.starts_with(root) {
        bail!("path escapes the Nole root");
    }
    let name = path.file_name().context("path must name a file")?;
    Ok(canonical_parent.join(name))
}

fn resolve_transfer_source(root: &Path, input: &str) -> Result<PathBuf> {
    let unresolved = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    };
    let file_type = fs::symlink_metadata(&unresolved)
        .with_context(|| format!("checking source {}", unresolved.display()))?
        .file_type();
    if file_type.is_symlink() || !file_type.is_file() {
        bail!("source must be a regular file and cannot be a symlink");
    }
    let source = fs::canonicalize(&unresolved)
        .with_context(|| format!("resolving source {}", unresolved.display()))?;
    ensure_not_special(root, &source)?;
    Ok(source)
}

fn resolve_new_destination(root: &Path, input: &str) -> Result<PathBuf> {
    let destination = safe_relative(root, input)?;
    ensure_not_special(root, &destination)?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => bail!("destination already exists: {input}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(error).with_context(|| format!("checking destination {input}")),
    }
}

/// A named path zone under the Nole root. Zone classification drives both the
/// approval gate (mutations inside the Agent workspace run unapproved) and the
/// paths generic tools may touch at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathZone {
    /// `config/`: application-managed configuration, read-only for generic tools.
    Config,
    /// `daily/`: calendar notes with their own tools; several generic tools are excluded.
    Daily,
    /// `attachments/`: application-managed attachment internals; generic tools
    /// must never read or mutate these paths. Use dedicated attachment tools instead.
    Attachments,
    /// `workspace/main`: the current persisted main Agent session's sandbox.
    /// Mutations here proceed without approval.
    Workspace,
    /// Everything else (`data/`, `archives/`, `themes/`, `skills/`, root files).
    Normal,
}

/// Classify a canonical path under `root` by zone.
pub(crate) fn path_zone(root: &Path, path: &Path) -> PathZone {
    if path.starts_with(root.join("config")) {
        PathZone::Config
    } else if path.starts_with(root.join("daily")) {
        PathZone::Daily
    } else if path.starts_with(root.join(ATTACHMENTS_DIR)) {
        PathZone::Attachments
    } else if path.starts_with(workspace_dir(root)) {
        PathZone::Workspace
    } else {
        PathZone::Normal
    }
}

/// Whether mutating `path` requires user approval. Everything under
/// `workspace/main` is the current session's sandbox and bypasses the gate.
fn outside_agent_workspace(root: &Path, path: &Path) -> bool {
    !matches!(path_zone(root, path), PathZone::Workspace)
}

pub fn ensure_not_special(root: &Path, path: &Path) -> Result<()> {
    match path_zone(root, path) {
        PathZone::Config | PathZone::Daily | PathZone::Attachments => {
            bail!("generic file tools cannot operate on this special file");
        }
        PathZone::Workspace | PathZone::Normal => Ok(()),
    }
}

fn copy_to_new_file(source: &Path, destination: &Path) -> Result<u64> {
    let mut input =
        fs::File::open(source).with_context(|| format!("opening source {}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("creating destination {}", destination.display()))?;
    match std::io::copy(&mut input, &mut output) {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            drop(output);
            let _ = fs::remove_file(destination);
            Err(error).with_context(|| format!("copying to {}", destination.display()))
        }
    }
}

/// Identity of a move source captured before approval and re-verified after,
/// so a file that changes while the user is deciding is never moved.
struct SourceIdentity {
    canonical: PathBuf,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

/// Snapshot the identity of a move source: canonical path, size, and
/// modification time, after confirming it is a regular file.
fn capture_source_identity(path: &Path) -> Result<SourceIdentity> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("rechecking source {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("source must be a regular file and cannot be a symlink");
    }
    let canonical =
        fs::canonicalize(path).with_context(|| format!("resolving source {}", path.display()))?;
    Ok(SourceIdentity {
        canonical,
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// Re-check a captured source identity before mutating: the canonical path
/// must be unchanged and still hold a regular file of the same size and
/// modification time.
fn revalidate_source(identity: &SourceIdentity) -> Result<()> {
    let path = &identity.canonical;
    let current = fs::symlink_metadata(path)
        .with_context(|| format!("rechecking source {}", path.display()))?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.len() != identity.len
        || current.modified().ok() != identity.modified
        || fs::canonicalize(path)
            .with_context(|| format!("rechecking source {}", path.display()))?
            != *path
    {
        bail!("source changed before move; inspect it again and retry");
    }
    Ok(())
}

/// Move a file to a new path. Same-filesystem moves use an atomic `rename`;
/// only a cross-device error (EXDEV) falls back to a durable copy, sync, and
/// remove of the source.
fn move_to_new_file(source: &Path, destination: &Path) -> Result<u64> {
    match fs::rename(source, destination) {
        Ok(()) => {
            let bytes = fs::metadata(destination)
                .with_context(|| format!("stat after rename {}", destination.display()))?
                .len();
            Ok(bytes)
        }
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            move_across_devices(source, destination)
        }
        Err(error) => Err(error)
            .with_context(|| format!("renaming {} to {}", source.display(), destination.display())),
    }
}

/// Cross-device fallback for a move: copy into a freshly created destination
/// (never clobbering an existing path), make the copy durable by syncing the
/// file and its parent directory, then remove the source. A failure before
/// the source is removed rolls back the destination copy.
fn move_across_devices(source: &Path, destination: &Path) -> Result<u64> {
    let bytes = copy_to_new_file(source, destination)?;
    if let Err(error) = OpenOptions::new()
        .write(true)
        .open(destination)
        .and_then(|file| file.sync_all())
    {
        let _ = fs::remove_file(destination);
        return Err(error)
            .with_context(|| format!("syncing copied file {}", destination.display()));
    }
    #[cfg(not(windows))]
    if let Some(parent) = destination.parent() {
        if let Err(error) = fs::File::open(parent).and_then(|directory| directory.sync_all()) {
            let _ = fs::remove_file(destination);
            return Err(error)
                .with_context(|| format!("syncing destination directory {}", parent.display()));
        }
    }
    if let Err(error) = fs::remove_file(source) {
        let rollback = fs::remove_file(destination);
        bail!(
            "could not remove move source {}: {error}; destination rollback {}",
            source.display(),
            if rollback.is_ok() {
                "succeeded"
            } else {
                "failed"
            }
        );
    }
    Ok(bytes)
}

pub struct Copy {
    root: PathBuf,
}

impl Copy {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Copy {
    fn name(&self) -> &'static str {
        "copy"
    }

    fn description(&self) -> &'static str {
        "Copy an existing regular file to a new destination under the Nole root without overwriting."
    }

    fn input_schema(&self) -> Value {
        transfer_schema()
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let source = resolve_transfer_source(&self.root, required_string(input, "source")?)?;
        let destination_text = required_string(input, "destination")?;
        let destination = resolve_new_destination(&self.root, destination_text)?;
        validate_write_preconditions(&self.root, &destination, WriteSource::File(&source))?;
        let bytes = copy_with_workspace_limits(&self.root, &source, &destination)?;
        Ok(post_write_result(
            format!("copied {bytes} bytes to {destination_text}"),
            destination_text,
            &destination,
        ))
    }
}

pub struct ExportFile {
    storage: Storage,
    gate: ApprovalGate,
}

impl ExportFile {
    pub fn new(root: &Path, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(canonical_root(root)?)?,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for ExportFile {
    fn name(&self) -> &'static str {
        "export_file"
    }

    fn description(&self) -> &'static str {
        "Publish one Nole file to a new destination outside Nole as exact original bytes or safe standalone HTML."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Nole-root-relative path, or an absolute path within Nole"
                },
                "destination": {
                    "type": "string",
                    "description": "Absolute, ~/..., or relative to the parent of Nole; must be outside Nole and not exist"
                },
                "format": {
                    "enum": ["original", "html"],
                    "description": "HTML requires a UTF-8 .md or .mb source and a matching destination suffix"
                }
            },
            "required": ["source", "destination", "format"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let source = required_string(input, "source")?.to_string();
        let destination = required_string(input, "destination")?.to_string();
        let format = required_string(input, "format")?.parse::<ExportFormat>()?;
        let storage = self.storage.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            storage.prepare_export(
                source,
                destination,
                format,
                ExportDestinationPolicy::CreateNew,
            )
        })
        .await
        .context("joining export preparation")??;
        self.gate
            .request(ApprovalRequest {
                title: "Export file".to_string(),
                message: format!(
                    "Export {} as {} to {}?",
                    prepared.source().display(),
                    format.label(),
                    prepared.destination().display()
                ),
                kind: ApprovalKind::Confirm,
            })
            .await?;
        // Rendering HTML is CPU-bound, so publish on the blocking pool.
        let storage = self.storage.clone();
        let outcome = tokio::task::spawn_blocking(move || storage.publish_export(&prepared))
            .await
            .context("joining export publication")??;
        Ok(describe_export_outcome(&outcome, format))
    }
}

/// Format the tool result for a finished export: target path, byte count, and
/// a summary of any renderer diagnostics (downgraded assets) instead of
/// dropping them silently.
fn describe_export_outcome(outcome: &ExportOutcome, format: ExportFormat) -> String {
    let mut summary = format!(
        "exported {} bytes as {} to {}",
        outcome.bytes,
        format.agent_value(),
        outcome.destination.display()
    );
    if !outcome.diagnostics.is_empty() {
        summary.push_str(&format!(
            "; {} warning(s): first: {}",
            outcome.diagnostics.len(),
            outcome
                .diagnostics
                .first()
                .map(ToString::to_string)
                .unwrap_or_default()
        ));
    }
    summary
}

pub struct Move {
    root: PathBuf,
    events: AgentEventSender,
    gate: ApprovalGate,
}

impl Move {
    pub fn new(root: &Path, events: AgentEventSender, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Move {
    fn name(&self) -> &'static str {
        "move"
    }

    fn description(&self) -> &'static str {
        "Move an existing regular file to a new destination under the Nole root without overwriting."
    }

    fn input_schema(&self) -> Value {
        transfer_schema()
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let source = resolve_transfer_source(&self.root, required_string(input, "source")?)?;
        let identity = capture_source_identity(&source)?;
        let destination_text = required_string(input, "destination")?;
        let destination = resolve_new_destination(&self.root, destination_text)?;
        validate_write_preconditions(&self.root, &destination, WriteSource::File(&source))?;
        check_workspace_write(&self.root, &destination, identity.len)?;
        if outside_agent_workspace(&self.root, &source) {
            self.gate
                .request(ApprovalRequest {
                    title: "Move file".to_string(),
                    message: format!(
                        "Move {} to {}?",
                        display_path(&self.root, &source),
                        display_path(&self.root, &destination)
                    ),
                    kind: ApprovalKind::Confirm,
                })
                .await?;
        }
        revalidate_source(&identity)?;
        let bytes = move_to_new_file(&source, &destination)?;
        send_file_moved(&self.events, &self.root, &source, &destination);
        Ok(post_write_result(
            format!("moved {bytes} bytes to {destination_text}"),
            destination_text,
            &destination,
        ))
    }
}

pub struct MoveMany {
    root: PathBuf,
    events: AgentEventSender,
    gate: ApprovalGate,
}

impl MoveMany {
    pub fn new(root: &Path, events: AgentEventSender, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for MoveMany {
    fn name(&self) -> &'static str {
        "move_many"
    }

    fn description(&self) -> &'static str {
        "Move multiple regular files into an existing directory under the Nole root, preserving their basenames and never overwriting."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sources": {
                    "type": "array", "minItems": 1, "maxItems": 200,
                    "items": { "type": "string" }
                },
                "destination_directory": {
                    "type": "string",
                    "description": "Existing directory relative to the Nole root"
                }
            },
            "required": ["sources", "destination_directory"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let source_values = input
            .get("sources")
            .and_then(Value::as_array)
            .context("field sources must be an array")?;
        if source_values.is_empty() || source_values.len() > 200 {
            bail!("sources must contain between 1 and 200 paths");
        }
        let directory_text = required_string(input, "destination_directory")?;
        let destination_directory = resolve_destination_directory(&self.root, directory_text)?;
        let mut transfers = Vec::with_capacity(source_values.len());
        let mut sources = std::collections::HashSet::new();
        let mut destinations = std::collections::HashSet::new();
        for value in source_values {
            let source_text = value.as_str().context("each source must be a string")?;
            let source = resolve_transfer_source(&self.root, source_text)?;
            let identity = capture_source_identity(&source)?;
            if !sources.insert(source.clone()) {
                bail!("duplicate source: {source_text}");
            }
            let name = source.file_name().context("source must have a file name")?;
            let destination = destination_directory.join(name);
            ensure_not_special(&self.root, &destination)?;
            if !destinations.insert(destination.clone()) {
                bail!(
                    "multiple sources have the same basename: {}",
                    name.to_string_lossy()
                );
            }
            match fs::symlink_metadata(&destination) {
                Ok(_) => bail!("destination already exists: {}", destination.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("checking batch destination"),
            }
            validate_write_preconditions(&self.root, &destination, WriteSource::File(&source))?;
            transfers.push((source, destination, identity));
        }
        check_workspace_writes(
            &self.root,
            transfers
                .iter()
                .map(|(_, destination, identity)| (destination.as_path(), identity.len)),
        )?;

        if transfers
            .iter()
            .any(|(source, _, _)| outside_agent_workspace(&self.root, source))
        {
            self.gate
                .request(ApprovalRequest {
                    title: "Move files".to_string(),
                    message: format!(
                        "Move {} file{} into {}?",
                        transfers.len(),
                        if transfers.len() == 1 { "" } else { "s" },
                        directory_text
                    ),
                    kind: ApprovalKind::Confirm,
                })
                .await?;
        }

        for (_, _, identity) in &transfers {
            revalidate_source(identity)?;
        }

        let mut completed = Vec::with_capacity(transfers.len());
        for (source, destination, _) in &transfers {
            match move_to_new_file(source, destination) {
                Ok(bytes) => completed.push((source.clone(), destination.clone(), bytes)),
                Err(error) => {
                    let rollback_errors = rollback_moves(&completed);
                    if rollback_errors.is_empty() {
                        bail!("batch move failed and was rolled back: {error}");
                    }
                    bail!(
                        "batch move failed: {error}; rollback failures: {}",
                        rollback_errors.join("; ")
                    );
                }
            }
        }
        let moved = completed
            .iter()
            .map(|(source, destination, bytes)| {
                json!({
                    "source": display_path(&self.root, source),
                    "destination": display_path(&self.root, destination),
                    "bytes": bytes,
                })
            })
            .collect::<Vec<_>>();
        for (source, destination, _) in &completed {
            send_file_moved(&self.events, &self.root, source, destination);
        }
        let warnings = completed
            .iter()
            .filter_map(|(_, destination, _)| {
                let display = display_path(&self.root, destination);
                mbdown_warning(&display, destination)
            })
            .collect::<Vec<_>>();
        let mut result = json!({
            "destination_directory": directory_text,
            "count": moved.len(),
            "moved": moved,
        });
        if !warnings.is_empty() {
            result["mbdown_warnings"] = json!(warnings);
            result["repair"] = json!(format!(
                "Files were moved. Read each affected file and {REPAIR_REQUIRED_MARKER}."
            ));
        }
        serde_json::to_string_pretty(&result).context("encoding batch move result")
    }
}

fn resolve_destination_directory(root: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    if relative.is_absolute() {
        bail!("destination_directory must be relative to the Nole root");
    }
    let unresolved = root.join(relative);
    if fs::symlink_metadata(&unresolved)?.file_type().is_symlink() {
        bail!("destination_directory cannot be a symlink");
    }
    let directory = fs::canonicalize(&unresolved)
        .with_context(|| format!("resolving destination directory {input}"))?;
    if !directory.starts_with(root) {
        bail!("destination_directory escapes the Nole root");
    }
    ensure_not_special(root, &directory)?;
    if !fs::metadata(&directory)?.is_dir() {
        bail!("destination_directory must be an existing directory");
    }
    Ok(directory)
}

fn rollback_moves(completed: &[(PathBuf, PathBuf, u64)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (source, destination, _) in completed.iter().rev() {
        match move_to_new_file(destination, source) {
            Ok(_) => {}
            Err(error) => errors.push(format!(
                "{} -> {}: {error}",
                destination.display(),
                source.display()
            )),
        }
    }
    errors
}

pub struct Rename {
    root: PathBuf,
    events: AgentEventSender,
    gate: ApprovalGate,
}

impl Rename {
    pub fn new(root: &Path, events: AgentEventSender, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Rename {
    fn name(&self) -> &'static str {
        "rename"
    }

    fn description(&self) -> &'static str {
        "Rename a regular file without changing its directory or overwriting another path."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the Nole root" },
                "new_name": { "type": "string", "description": "New basename only" }
            },
            "required": ["path", "new_name"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let path_text = required_string(input, "path")?;
        if Path::new(path_text).is_absolute() {
            bail!("rename path must be relative to the Nole root");
        }
        let source = resolve_transfer_source(&self.root, path_text)?;
        let identity = capture_source_identity(&source)?;
        if !source.starts_with(&self.root) {
            bail!("rename source must be under the Nole root");
        }
        let new_name = required_string(input, "new_name")?;
        let candidate = Path::new(new_name);
        if candidate.file_name().is_none()
            || candidate.components().count() != 1
            || candidate == Path::new(".")
            || candidate == Path::new("..")
        {
            bail!("new_name must be a file basename without directory components");
        }
        let destination = source
            .parent()
            .context("source must have a parent directory")?
            .join(candidate);
        ensure_not_special(&self.root, &destination)?;
        match fs::symlink_metadata(&destination) {
            Ok(_) => bail!("destination already exists: {}", destination.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("checking rename destination"),
        }
        validate_write_preconditions(&self.root, &destination, WriteSource::File(&source))?;
        check_workspace_write(&self.root, &destination, identity.len)?;
        if outside_agent_workspace(&self.root, &source) {
            self.gate
                .request(ApprovalRequest {
                    title: "Rename file".to_string(),
                    message: format!(
                        "Rename {} to {}?",
                        display_path(&self.root, &source),
                        display_path(&self.root, &destination)
                    ),
                    kind: ApprovalKind::Confirm,
                })
                .await?;
        }
        revalidate_source(&identity)?;
        let bytes = move_to_new_file(&source, &destination)?;
        send_file_moved(&self.events, &self.root, &source, &destination);
        let destination_text = display_path(&self.root, &destination);
        Ok(post_write_result(
            format!("renamed {path_text} to {destination_text} ({bytes} bytes)"),
            &destination_text,
            &destination,
        ))
    }
}

fn send_file_moved(events: &AgentEventSender, root: &Path, from: &Path, to: &Path) {
    let display = |path: &Path| {
        path.strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let _ = events.send(AgentEvent::FileMoved {
        from: display(from),
        to: display(to),
    });
}

fn transfer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "source": { "type": "string" },
            "destination": { "type": "string", "description": "Path relative to the Nole root" }
        },
        "required": ["source", "destination"], "additionalProperties": false
    })
}

pub struct Delete {
    root: PathBuf,
    gate: ApprovalGate,
}

impl Delete {
    pub fn new(root: &Path, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Delete {
    fn name(&self) -> &'static str {
        "delete"
    }

    fn description(&self) -> &'static str {
        "Delete an existing regular file under the Nole root."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": {
                "path": { "type": "string", "description": "Path relative to the Nole root" }
            },
            "required": ["path"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let unresolved = safe_relative(&self.root, relative)?;
        let metadata = fs::symlink_metadata(&unresolved)
            .with_context(|| format!("checking {}", unresolved.display()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            bail!("delete only accepts regular files, not symlinks or directories");
        }
        let path = fs::canonicalize(&unresolved)?;
        match path_zone(&self.root, &path) {
            PathZone::Config => bail!("delete cannot operate inside config/"),
            PathZone::Attachments => {
                bail!("generic file tools cannot operate inside attachments/")
            }
            _ => {}
        }
        let modified = metadata.modified().ok();
        if outside_agent_workspace(&self.root, &path) {
            self.gate
                .request(ApprovalRequest {
                    title: "Delete file".to_string(),
                    message: format!("Delete {relative}?"),
                    kind: ApprovalKind::DestructiveConfirm,
                })
                .await?;
        }

        let current = fs::symlink_metadata(&unresolved)
            .with_context(|| format!("rechecking {}", unresolved.display()))?;
        if current.file_type().is_symlink()
            || !current.file_type().is_file()
            || current.len() != metadata.len()
            || current.modified().ok() != modified
            || fs::canonicalize(&unresolved)? != path
        {
            bail!("file changed before deletion; inspect it again and retry");
        }
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
        Ok(format!("deleted {relative}"))
    }
}

pub struct Write {
    root: PathBuf,
}

impl Write {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        "Create a complete new UTF-8 text file without overwriting; use read and edit for existing files."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"], "additionalProperties": false
        })
    }
    async fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let content = required_string(input, "content")?;
        let path = safe_relative(&self.root, relative)?;
        match path_zone(&self.root, &path) {
            PathZone::Config | PathZone::Daily => {
                bail!("generic file tools cannot operate on this special file");
            }
            PathZone::Attachments => {
                bail!("generic file tools cannot operate inside attachments/")
            }
            _ => {}
        }
        match fs::symlink_metadata(&path) {
            Ok(_) => bail!("write only creates new files; use read and edit for existing files"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("checking {relative}")),
        }
        validate_write_preconditions(&self.root, &path, WriteSource::Text(content))?;
        check_workspace_write(&self.root, &path, content.len() as u64)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options
            .open(&path)
            .with_context(|| format!("writing file {}", path.display()))?;
        file.write_all(content.as_bytes())?;
        Ok(post_write_result(
            format!("wrote {} bytes to {relative}", content.len()),
            relative,
            &path,
        ))
    }
}

/// Resolve a directory path to create, canonicalizing the deepest existing
/// ancestor and appending the missing components. The result stays within the
/// Nole root and no symlink is followed.
fn resolve_new_directory(root: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("path must stay within the Nole root");
    }
    let mut existing = root.to_path_buf();
    let mut missing = Vec::new();
    for component in relative.components() {
        let candidate = existing.join(component.as_os_str());
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    bail!("refusing to create a directory through a symlink");
                }
                existing = candidate;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(component.as_os_str().to_os_string());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", candidate.display()));
            }
        }
    }
    if missing.is_empty() {
        bail!("destination already exists: {input}");
    }
    let canonical = fs::canonicalize(&existing)
        .with_context(|| format!("resolving parent directory {}", existing.display()))?;
    if !canonical.starts_with(root) {
        bail!("path escapes the Nole root");
    }
    let mut path = canonical;
    for part in missing {
        path.push(part);
    }
    Ok(path)
}

pub struct Mkdir {
    root: PathBuf,
}

impl Mkdir {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Mkdir {
    fn name(&self) -> &'static str {
        "mkdir"
    }

    fn description(&self) -> &'static str {
        "Create a new directory and any missing parents under the Nole root without following symlinks."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path relative to the Nole root" }
            },
            "required": ["path"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let path = resolve_new_directory(&self.root, relative)?;
        match path_zone(&self.root, &path) {
            PathZone::Config | PathZone::Daily | PathZone::Attachments => {
                bail!("generic file tools cannot operate on this special file");
            }
            _ => {}
        }
        fs::create_dir_all(&path)
            .with_context(|| format!("creating directory {}", path.display()))?;
        Ok(format!("created directory {relative}"))
    }
}

pub struct RemoveDir {
    root: PathBuf,
}

impl RemoveDir {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for RemoveDir {
    fn name(&self) -> &'static str {
        "remove_dir"
    }

    fn description(&self) -> &'static str {
        "Recursively remove a directory tree inside workspace/main without following symlinks."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path relative to the Nole root, inside workspace/main" }
            },
            "required": ["path"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let unresolved = safe_relative(&self.root, relative)?;
        let metadata = fs::symlink_metadata(&unresolved)
            .with_context(|| format!("checking {}", unresolved.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("refusing to remove a directory through a symlink");
        }
        if !metadata.is_dir() {
            bail!("remove_dir only removes directories; use delete for regular files");
        }
        let path = fs::canonicalize(&unresolved)
            .with_context(|| format!("resolving {}", unresolved.display()))?;
        if !matches!(path_zone(&self.root, &path), PathZone::Workspace) {
            bail!("recursive removal is only allowed inside workspace/main");
        }
        fs::remove_dir_all(&path).with_context(|| format!("removing {}", path.display()))?;
        Ok(format!("removed directory {relative}"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use tempfile::tempdir;

    use super::*;
    use crate::agent::test_support::{
        drain_events, event_channel, test_runtime, TestFutureResultExt,
    };
    use crate::agent::ApprovalDecision;
    use crate::agent::{snapshot_identity, snapshot_tag};
    use crate::export::{ExportDiagnostic, ExportDiagnosticSeverity};

    fn gate_without_decisions() -> (ApprovalGate, tokio::sync::broadcast::Receiver<AgentEvent>) {
        let (event_sender, event_receiver) = event_channel();
        let (_decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        (
            ApprovalGate {
                bypass: Arc::new(AtomicBool::new(false)),
                cancelled: Arc::new(AtomicBool::new(false)),
                events: event_sender,
                decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
            },
            event_receiver,
        )
    }

    /// Run a tool without supplying any approval decision. A tool that requests
    /// approval would block forever; the timeout turns that hang into a failure.
    fn execute_unapproved<F>(run: F) -> String
    where
        F: FnOnce() -> Result<String> + Send + 'static,
    {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(run());
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("tool must complete without waiting for approval")
            .expect("tool must succeed without approval")
    }

    fn assert_no_approval_requested(events: Vec<AgentEvent>) {
        assert!(
            events
                .into_iter()
                .all(|event| !matches!(event, AgentEvent::Approval(_))),
            "expected no approval request"
        );
    }

    #[test]
    fn delete_file_requests_a_confirm_approval_naming_the_target() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("Old.md"), "content").unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let tool = Delete::new(&root, gate).unwrap();
        let input = json!({"path": "Old.md"});
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        let output = test_runtime().block_on(tool.execute(&input)).unwrap();
        assert!(output.contains("deleted Old.md"));
        assert!(!root.join("Old.md").exists());
        let events = drain_events(&mut event_receiver);
        let request = events
            .into_iter()
            .find_map(|event| match event {
                AgentEvent::Approval(request) => Some(request),
                _ => None,
            })
            .expect("delete_file must request approval");
        assert_eq!(request.kind, ApprovalKind::DestructiveConfirm);
        assert_eq!(request.title, "Delete file");
        assert_eq!(request.message, "Delete Old.md?");
    }

    #[test]
    fn workspace_edit_and_delete_bypass_approval_but_keep_snapshot_checks() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        let workspace = root.join("workspace/main");
        fs::create_dir_all(&workspace).unwrap();

        let draft = workspace.join("draft.md");
        fs::write(&draft, "wip\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let content = fs::read_to_string(&draft).unwrap();
        let canonical_draft = fs::canonicalize(&draft).unwrap();
        reads
            .mark_file(
                canonical_draft,
                snapshot_identity(&content),
                snapshot_tag(&content),
                0,
                1,
                1,
            )
            .unwrap();
        let (gate, mut events) = gate_without_decisions();
        let edit = Edit::new(&root, gate, reads).unwrap();
        let input = json!({
            "path": "workspace/main/draft.md",
            "tag": snapshot_tag(&content),
            "edits": [{"operation": "replace", "start_line": 1, "end_line": 1, "lines": ["edited"]}]
        });
        let output = execute_unapproved(move || test_runtime().block_on(edit.execute(&input)));
        assert!(output.contains("edited workspace/main/draft.md"));
        assert_eq!(fs::read_to_string(&draft).unwrap(), "edited\n");
        assert_no_approval_requested(drain_events(&mut events));

        // Snapshot checks still gate workspace edits: a stale tag is refused.
        let reads = Arc::new(ReadTracker::default());
        let content = fs::read_to_string(&draft).unwrap();
        let canonical_draft = fs::canonicalize(&draft).unwrap();
        reads
            .mark_file(
                canonical_draft,
                snapshot_identity(&content),
                snapshot_tag(&content),
                0,
                1,
                1,
            )
            .unwrap();
        let (gate, _) = gate_without_decisions();
        let edit = Edit::new(&root, gate, reads).unwrap();
        let input = json!({
            "path": "workspace/main/draft.md",
            "tag": "DEAD",
            "edits": [{"operation": "replace", "start_line": 1, "end_line": 1, "lines": ["again"]}]
        });
        let error = test_runtime().block_on(edit.execute(&input)).unwrap_err();
        assert!(error.to_string().contains("snapshot tag mismatch"));
        assert_eq!(fs::read_to_string(&draft).unwrap(), "edited\n");

        let removed = workspace.join("trash.md");
        fs::write(&removed, "remove").unwrap();
        let (gate, mut events) = gate_without_decisions();
        let delete = Delete::new(&root, gate).unwrap();
        let input = json!({"path": "workspace/main/trash.md"});
        let output = execute_unapproved(move || test_runtime().block_on(delete.execute(&input)));
        assert!(output.contains("deleted workspace/main/trash.md"));
        assert!(!removed.exists());
        assert_no_approval_requested(drain_events(&mut events));
    }

    #[test]
    fn workspace_transfers_and_directory_tools_bypass_approval() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        let workspace = root.join("workspace/main");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();

        // move and rename inside workspace/main proceed without approval.
        let moved = workspace.join("a.txt");
        fs::write(&moved, "a").unwrap();
        let (event_sender, mut events) = event_channel();
        let (gate, mut gate_events) = gate_without_decisions();
        let mover = Move::new(&root, event_sender, gate).unwrap();
        let input =
            json!({"source": "workspace/main/a.txt", "destination": "workspace/main/b.txt"});
        let output = execute_unapproved(move || test_runtime().block_on(mover.execute(&input)));
        assert!(output.contains("moved"));
        assert!(!moved.exists());
        assert_eq!(fs::read_to_string(workspace.join("b.txt")).unwrap(), "a");
        assert_no_approval_requested(drain_events(&mut gate_events));
        let _ = drain_events(&mut events);

        let (event_sender, _) = event_channel();
        let (gate, mut gate_events) = gate_without_decisions();
        let renamer = Rename::new(&root, event_sender, gate).unwrap();
        let input = json!({"path": "workspace/main/b.txt", "new_name": "c.txt"});
        let output = execute_unapproved(move || test_runtime().block_on(renamer.execute(&input)));
        assert!(output.contains("renamed"));
        assert!(!workspace.join("b.txt").exists());
        assert_eq!(fs::read_to_string(workspace.join("c.txt")).unwrap(), "a");
        assert_no_approval_requested(drain_events(&mut gate_events));

        // mkdir and remove_dir manage directories inside the workspace freely.
        // Neither tool consults an approval gate; they run unapproved by design.
        let mkdir = Mkdir::new(&root).unwrap();
        let output = execute_unapproved(move || {
            test_runtime().block_on(mkdir.execute(&json!({
                "path": "workspace/main/newdir/sub"
            })))
        });
        assert!(output.contains("created directory workspace/main/newdir/sub"));
        assert!(workspace.join("newdir/sub").is_dir());

        let remover = RemoveDir::new(&root).unwrap();
        let output = execute_unapproved(move || {
            test_runtime().block_on(remover.execute(&json!({"path": "workspace/main/newdir"})))
        });
        assert!(output.contains("removed directory workspace/main/newdir"));
        assert!(!workspace.join("newdir").exists());

        // remove_dir refuses trees outside workspace/main.
        fs::create_dir_all(root.join("data/notes")).unwrap();
        let remover = RemoveDir::new(&root).unwrap();
        let error = test_runtime()
            .block_on(remover.execute(&json!({"path": "data/notes"})))
            .unwrap_err();
        assert!(error.to_string().contains("workspace/main"));
        assert!(root.join("data/notes").is_dir());
    }

    #[test]
    fn external_source_moves_require_approval_and_denial_preserves_the_source() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir_all(root.join("data")).unwrap();
        let outside = tempdir().unwrap();
        let move_source = outside.path().join("move.txt");
        fs::write(&move_source, "move me").unwrap();

        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender.clone(),
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let mover = Move::new(&root, event_sender, gate).unwrap();
        let input = json!({
            "source": move_source,
            "destination": "data/moved.md"
        });
        let worker = std::thread::spawn(move || test_runtime().block_on(mover.execute(&input)));
        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert_eq!(request.kind, ApprovalKind::Confirm);
        assert_eq!(request.title, "Move file");
        assert!(request.message.contains("Move"));
        decision_sender.send(ApprovalDecision::Deny).unwrap();
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("change denied by user"));
        assert!(move_source.exists(), "denied move must preserve the source");
        assert!(!root.join("data/moved.md").exists());

        // Approval before mutation: re-run with approval and the source moves.
        fs::write(&move_source, "move me").unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender.clone(),
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let mover = Move::new(&root, event_sender, gate).unwrap();
        let input = json!({
            "source": move_source,
            "destination": "data/moved.md"
        });
        let worker = std::thread::spawn(move || test_runtime().block_on(mover.execute(&input)));
        let AgentEvent::Approval(_) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        let output = worker.join().unwrap().unwrap();
        assert!(output.contains("moved"));
        assert!(!move_source.exists());
        assert_eq!(
            fs::read_to_string(root.join("data/moved.md")).unwrap(),
            "move me"
        );

        // Batch move with any external source is denied atomically.
        let alpha = outside.path().join("alpha.txt");
        let beta = outside.path().join("beta.txt");
        fs::write(&alpha, "alpha").unwrap();
        fs::write(&beta, "beta").unwrap();
        fs::create_dir_all(root.join("data/collected")).unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender.clone(),
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let mover = MoveMany::new(&root, event_sender, gate).unwrap();
        let input = json!({
            "sources": [alpha, beta],
            "destination_directory": "data/collected"
        });
        let worker = std::thread::spawn(move || test_runtime().block_on(mover.execute(&input)));
        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert_eq!(request.title, "Move files");
        decision_sender.send(ApprovalDecision::Deny).unwrap();
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("change denied by user"));
        assert!(alpha.exists());
        assert!(beta.exists());
        assert!(!root.join("data/collected/alpha.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn same_filesystem_move_is_an_atomic_rename() {
        use std::os::unix::fs::MetadataExt;

        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("a.txt");
        fs::write(&source, "rename me").unwrap();
        let before = fs::metadata(&source).unwrap();

        let bytes = move_to_new_file(&source, &root.join("b.txt")).unwrap();
        assert_eq!(bytes, 9);
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "rename me");
        // rename preserves the inode; a copy/remove fallback would not.
        let after = fs::metadata(root.join("b.txt")).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    }

    #[test]
    fn cross_device_fallback_copies_syncs_and_removes_source() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("a.txt");
        fs::write(&source, "durable copy").unwrap();

        let bytes = move_across_devices(&source, &root.join("b.txt")).unwrap();
        assert_eq!(bytes, "durable copy".len() as u64);
        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(root.join("b.txt")).unwrap(),
            "durable copy"
        );
    }

    #[test]
    fn move_revalidates_the_source_after_approval() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir_all(root.join("data")).unwrap();
        let outside = tempdir().unwrap();
        let move_source = outside.path().join("move.txt");
        fs::write(&move_source, "move me").unwrap();

        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender.clone(),
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let mover = Move::new(&root, event_sender, gate).unwrap();
        let input = json!({
            "source": move_source,
            "destination": "data/moved.md"
        });
        let worker = std::thread::spawn(move || test_runtime().block_on(mover.execute(&input)));
        let AgentEvent::Approval(_) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        // The file changes while the user is deciding; approving must not move it.
        fs::write(&move_source, "changed while deciding").unwrap();
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("source changed before move"));
        assert!(move_source.exists());
        assert!(!root.join("data/moved.md").exists());
    }

    #[test]
    fn generic_file_tools_reject_attachment_internals() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        let attachments = root.join("attachments/objects");
        fs::create_dir_all(&attachments).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        fs::write(attachments.join("ab.txt"), "content").unwrap();
        fs::write(root.join("data/foo.txt"), "plain").unwrap();
        let attachment_path = "attachments/objects/ab.txt";

        let (event_sender, _) = event_channel();
        let (gate, _) = gate_without_decisions();
        assert!(Delete::new(&root, gate)
            .unwrap()
            .execute(&json!({"path": attachment_path}))
            .returns_err());
        assert!(Write::new(&root)
            .unwrap()
            .execute(&json!({"path": "attachments/objects/new.txt", "content": "x"}))
            .returns_err());
        assert!(Copy::new(&root)
            .unwrap()
            .execute(&json!({"source": attachment_path, "destination": "data/copied.txt"}))
            .returns_err());
        assert!(Copy::new(&root)
            .unwrap()
            .execute(&json!({"source": "data/foo.txt", "destination": "attachments/objects/x.txt"}))
            .returns_err());
        assert!(
            Move::new(&root, event_sender.clone(), gate_without_decisions().0)
                .unwrap()
                .execute(&json!({"source": attachment_path, "destination": "data/moved.txt"}))
                .returns_err()
        );
        assert!(
            Rename::new(&root, event_sender.clone(), gate_without_decisions().0)
                .unwrap()
                .execute(&json!({"path": attachment_path, "new_name": "x.txt"}))
                .returns_err()
        );
        assert!(Mkdir::new(&root)
            .unwrap()
            .execute(&json!({"path": "attachments/newdir"}))
            .returns_err());
        assert!(RemoveDir::new(&root)
            .unwrap()
            .execute(&json!({"path": "attachments/objects"}))
            .returns_err());
        assert!(attachments.join("ab.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_tools_resist_path_traversal_and_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        let workspace = root.join("workspace/main");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "secret").unwrap();

        assert!(Mkdir::new(&root)
            .unwrap()
            .execute(&json!({"path": "workspace/../data/evil"}))
            .returns_err());
        fs::write(workspace.join("victim.md"), "victim").unwrap();
        let escape = workspace.join("escape.md");
        symlink(&outside_file, &escape).unwrap();

        let reads = Arc::new(ReadTracker::default());
        let (gate, _) = gate_without_decisions();
        let edit = Edit::new(&root, gate, reads).unwrap();
        let error = test_runtime()
            .block_on(edit.execute(&json!({
                "path": "workspace/main/escape.md",
                "tag": "0000",
                "edits": [{"operation": "replace", "start_line": 1, "end_line": 1, "lines": ["x"]}]
            })))
            .unwrap_err();
        assert!(error.to_string().contains("symlink"));

        let (gate, _) = gate_without_decisions();
        let delete = Delete::new(&root, gate).unwrap();
        assert!(delete
            .execute(&json!({"path": "workspace/main/escape.md"}))
            .returns_err());
        assert!(escape.exists());
        assert!(outside_file.exists());

        let outside_dir = outside.path().join("dir");
        fs::create_dir(&outside_dir).unwrap();
        let linked_dir = workspace.join("linked");
        symlink(&outside_dir, &linked_dir).unwrap();
        let remover = RemoveDir::new(&root).unwrap();
        assert!(remover
            .execute(&json!({"path": "workspace/main/linked"}))
            .returns_err());
        assert!(linked_dir.exists());
        assert!(outside_dir.is_dir());

        let (event_sender, _) = event_channel();
        let (gate, _) = gate_without_decisions();
        assert!(Move::new(&root, event_sender, gate)
            .unwrap()
            .execute(&json!({"source": "workspace/main/escape.md", "destination": "workspace/main/copy.md"}))
            .returns_err());
    }

    #[test]
    fn export_file_is_strict_approval_gated_and_never_overwrites() {
        let directory = tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("root");
        fs::create_dir_all(root.join("data")).unwrap();
        let source = root.join("data/note.md");
        fs::write(&source, "# Agent export\n").unwrap();
        let output = tempdir().unwrap();
        let output_path = output.path().canonicalize().unwrap();
        let denied_destination = output_path.join("denied.md");

        let (event_sender, mut events) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let tool = ExportFile::new(&root, gate).unwrap();
        assert_eq!(
            tool.description(),
            "Publish one Nole file to a new destination outside Nole as exact original bytes or safe standalone HTML."
        );
        assert!(!tool.description().contains("PDF"));
        let schema = tool.input_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["required"],
            json!(["source", "destination", "format"])
        );

        decision_sender.send(ApprovalDecision::Deny).unwrap();
        let denied = test_runtime()
            .block_on(tool.execute(&json!({
                "source": "data/note.md",
                "destination": denied_destination,
                "format": "original"
            })))
            .unwrap_err();
        assert_eq!(denied.to_string(), "change denied by user");
        assert!(!denied_destination.exists());
        let approval = drain_events(&mut events)
            .into_iter()
            .find_map(|event| match event {
                AgentEvent::Approval(request) => Some(request),
                _ => None,
            })
            .unwrap();
        assert_eq!(approval.title, "Export file");
        assert_eq!(approval.kind, ApprovalKind::Confirm);
        assert!(approval.message.contains(" as Original to "));

        let destination = output_path.join("approved.html");
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        let result = test_runtime()
            .block_on(tool.execute(&json!({
                "source": "data/note.md",
                "destination": destination,
                "format": "html"
            })))
            .unwrap();
        assert!(result.starts_with("exported "));
        assert!(result.contains(" bytes as html to "));
        assert!(fs::read_to_string(&destination)
            .unwrap()
            .starts_with("<!doctype html>"));
        assert!(tool
            .execute(&json!({
                "source": "data/note.md",
                "destination": destination,
                "format": "html"
            }))
            .returns_err());
    }

    #[test]
    fn export_result_summarizes_renderer_diagnostics() {
        let outcome = ExportOutcome {
            destination: PathBuf::from("/tmp/out/note.html"),
            bytes: 4096,
            diagnostics: vec![
                ExportDiagnostic {
                    severity: ExportDiagnosticSeverity::Warning,
                    message: "missing image assets/pic.png".to_string(),
                },
                ExportDiagnostic {
                    severity: ExportDiagnosticSeverity::Warning,
                    message: "unsupported style block ignored".to_string(),
                },
            ],
        };
        let result = describe_export_outcome(&outcome, ExportFormat::Html);
        assert!(result.starts_with("exported 4096 bytes as html to /tmp/out/note.html"));
        assert!(result.contains("2 warning(s): first: warning: missing image assets/pic.png"));

        let quiet = ExportOutcome {
            destination: PathBuf::from("/tmp/out/note.html"),
            bytes: 8192,
            diagnostics: Vec::new(),
        };
        assert_eq!(
            describe_export_outcome(&quiet, ExportFormat::Html),
            "exported 8192 bytes as html to /tmp/out/note.html"
        );
    }
}
