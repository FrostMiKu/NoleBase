//! File mutation tools: write, edit, copy, move, rename, and delete.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::util::{display_path, limited_diff, required_string, MAX_FILE_BYTES};
use super::write_policy::{validate_write, WriteSource};
use crate::agent::{
    canonical_root, AgentEvent, AgentEventSender, ApprovalGate, ApprovalKind, ApprovalRequest,
    ReadTracker, Tool,
};

pub struct Edit {
    root: PathBuf,
    config_dir: PathBuf,
    gate: ApprovalGate,
    reads: Arc<ReadTracker>,
}

impl Edit {
    pub fn new(root: &Path, gate: ApprovalGate, reads: Arc<ReadTracker>) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            config_dir: root.join("config"),
            root,
            gate,
            reads,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Edit an existing UTF-8 file under the Nole root outside config/ using a `[path#TAG]` snapshot from read and one-based line anchors. The tag must match the latest read snapshot, and only displayed ranges or adjacent insertion anchors may change."
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
        if path.starts_with(&self.config_dir) {
            bail!("edit cannot operate inside config/");
        }
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            bail!("target must be a regular UTF-8 file no larger than 1 MB");
        }
        let old = fs::read_to_string(&path)
            .with_context(|| format!("reading current file {}", path.display()))?;
        let state = self
            .reads
            .file_state(&path)?
            .context("edit requires read on the same path first")?;
        if !state.tag.eq_ignore_ascii_case(tag) {
            bail!("snapshot tag mismatch for {relative}; read the file again before editing");
        }
        if state.snapshot != old {
            self.reads.consume_file(&path)?;
            bail!("file changed since read; read it again before editing");
        }
        let offsets = line_byte_offsets(&old);
        let total_lines = offsets.len().saturating_sub(1);
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
        let content = apply_line_edits(&old, &offsets, &edits);
        if content.len() as u64 > MAX_FILE_BYTES {
            bail!("edited content exceeds 1 MB");
        }
        if old == content {
            return Ok(format!("no changes needed for {relative}"));
        }
        validate_write(&self.root, &path, WriteSource::Text(&content))?;
        self.gate
            .request(ApprovalRequest {
                title: format!("Edit {relative}"),
                message: limited_diff(&old, &content, relative, relative),
                kind: ApprovalKind::Diff,
            })
            .await?;
        let current =
            fs::read_to_string(&path).with_context(|| format!("rechecking {}", path.display()))?;
        if current != old {
            self.reads.consume_file(&path)?;
            bail!("file changed before editing; read it again and retry");
        }
        fs::write(&path, &content).with_context(|| format!("editing {}", path.display()))?;
        self.reads.consume_file(&path)?;
        Ok(format!("edited {relative}"))
    }
}

#[derive(Debug)]
pub struct LineEdit {
    start_line: usize,
    end_line_exclusive: usize,
    lines: Vec<String>,
    insertion: bool,
    anchor_line: usize,
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

fn line_byte_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            offsets.push(index + 1);
        }
    }
    if offsets.last().copied() != Some(content.len()) {
        offsets.push(content.len());
    }
    offsets
}

fn apply_line_edits(old: &str, offsets: &[usize], edits: &[LineEdit]) -> String {
    let mut content = old.to_string();
    let line_ending = if old.contains("\r\n") { "\r\n" } else { "\n" };
    for edit in edits.iter().rev() {
        let mut replacement = if edit.lines.is_empty() {
            String::new()
        } else {
            format!("{}{}", edit.lines.join(line_ending), line_ending)
        };
        if edit.insertion
            && edit.start_line == offsets.len().saturating_sub(1)
            && !old.is_empty()
            && !old.ends_with('\n')
            && !replacement.is_empty()
        {
            replacement.insert_str(0, line_ending);
        }
        content.replace_range(
            offsets[edit.start_line]..offsets[edit.end_line_exclusive],
            &replacement,
        );
    }
    content
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

pub fn ensure_not_special(root: &Path, path: &Path) -> Result<()> {
    if path.starts_with(root.join("config")) || path.starts_with(root.join("daily")) {
        bail!("generic file tools cannot operate on this special file");
    }
    Ok(())
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

fn move_to_new_file(source: &Path, destination: &Path) -> Result<u64> {
    let bytes = copy_to_new_file(source, destination)?;
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
        "Copy a regular file from an absolute or Nole-relative source to a new path under the Nole root outside config/ and daily/."
    }

    fn input_schema(&self) -> Value {
        transfer_schema()
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let source = resolve_transfer_source(&self.root, required_string(input, "source")?)?;
        let destination_text = required_string(input, "destination")?;
        let destination = resolve_new_destination(&self.root, destination_text)?;
        validate_write(&self.root, &destination, WriteSource::File(&source))?;
        let bytes = copy_to_new_file(&source, &destination)?;
        Ok(format!("copied {bytes} bytes to {destination_text}"))
    }
}

pub struct Move {
    root: PathBuf,
    events: AgentEventSender,
}

impl Move {
    pub fn new(root: &Path, events: AgentEventSender) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Move {
    fn name(&self) -> &'static str {
        "move"
    }

    fn description(&self) -> &'static str {
        "Move a regular file from an absolute or Nole-relative source to a new path under the Nole root outside config/ and daily/."
    }

    fn input_schema(&self) -> Value {
        transfer_schema()
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let source = resolve_transfer_source(&self.root, required_string(input, "source")?)?;
        let destination_text = required_string(input, "destination")?;
        let destination = resolve_new_destination(&self.root, destination_text)?;
        validate_write(&self.root, &destination, WriteSource::File(&source))?;
        let bytes = move_to_new_file(&source, &destination)?;
        send_file_moved(&self.events, &self.root, &source, &destination);
        Ok(format!("moved {bytes} bytes to {destination_text}"))
    }
}

pub struct MoveMany {
    root: PathBuf,
    events: AgentEventSender,
}

impl MoveMany {
    pub fn new(root: &Path, events: AgentEventSender) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
        })
    }
}

#[async_trait::async_trait]
impl Tool for MoveMany {
    fn name(&self) -> &'static str {
        "move_many"
    }

    fn description(&self) -> &'static str {
        "Move multiple regular files from absolute or Nole-relative sources into one existing directory under the Nole root outside config/ and daily/, preserving each basename. Each destination must be new."
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
            validate_write(&self.root, &destination, WriteSource::File(&source))?;
            transfers.push((source, destination));
        }

        let mut completed = Vec::with_capacity(transfers.len());
        for (source, destination) in &transfers {
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
                    "source": source.to_string_lossy(),
                    "destination": display_path(&self.root, destination),
                    "bytes": bytes,
                })
            })
            .collect::<Vec<_>>();
        for (source, destination, _) in &completed {
            send_file_moved(&self.events, &self.root, source, destination);
        }
        serde_json::to_string_pretty(&json!({
            "destination_directory": directory_text,
            "count": moved.len(),
            "moved": moved,
        }))
        .context("encoding batch move result")
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
}

impl Rename {
    pub fn new(root: &Path, events: AgentEventSender) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Rename {
    fn name(&self) -> &'static str {
        "rename"
    }

    fn description(&self) -> &'static str {
        "Rename one regular file under the Nole root outside config/ and daily/ without changing its directory. The new name must be a basename and the destination must be new."
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
        validate_write(&self.root, &destination, WriteSource::File(&source))?;
        let bytes = move_to_new_file(&source, &destination)?;
        send_file_moved(&self.events, &self.root, &source, &destination);
        Ok(format!(
            "renamed {path_text} to {} ({bytes} bytes)",
            display_path(&self.root, &destination)
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
        "Delete a regular file under the Nole root outside config/."
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
        if path.starts_with(self.root.join("config")) {
            bail!("delete cannot operate inside config/");
        }
        let modified = metadata.modified().ok();
        self.gate
            .request(ApprovalRequest {
                title: "Delete file".to_string(),
                message: format!("Delete {relative}?"),
                kind: ApprovalKind::Confirm,
            })
            .await?;

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
    config_dir: PathBuf,
    daily_dir: PathBuf,
}

impl Write {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            config_dir: root.join("config"),
            daily_dir: root.join("daily"),
            root,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        "Write a complete UTF-8 text file under the Nole root outside config/ and daily/. Existing files are refused unless overwrite is true. The complete candidate is validated before any file is created or replaced."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" },
                "overwrite": {
                    "type": "boolean", "default": false,
                    "description": "Replace an existing regular file when true; otherwise existing paths are refused"
                }
            },
            "required": ["path", "content"], "additionalProperties": false
        })
    }
    async fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let content = required_string(input, "content")?;
        let overwrite = input
            .get("overwrite")
            .map(|value| value.as_bool().context("field overwrite must be a boolean"))
            .transpose()?
            .unwrap_or(false);
        if content.len() as u64 > MAX_FILE_BYTES {
            bail!("content exceeds 1 MB");
        }
        let path = safe_relative(&self.root, relative)?;
        if path.starts_with(&self.config_dir) || path.starts_with(&self.daily_dir) {
            bail!("generic file tools cannot operate on this special file");
        }
        validate_write(&self.root, &path, WriteSource::Text(content))?;
        let mut options = OpenOptions::new();
        options.write(true);
        if overwrite {
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                    bail!("overwrite target must be a regular file and cannot be a symlink");
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).with_context(|| format!("checking {relative}")),
            }
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        let mut file = options
            .open(&path)
            .with_context(|| format!("writing file {}", path.display()))?;
        file.write_all(content.as_bytes())?;
        Ok(format!("wrote {} bytes to {relative}", content.len()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use tempfile::tempdir;

    use super::*;
    use crate::agent::test_support::{drain_events, event_channel, test_runtime};
    use crate::agent::ApprovalDecision;

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
        assert_eq!(request.kind, ApprovalKind::Confirm);
        assert_eq!(request.title, "Delete file");
        assert_eq!(request.message, "Delete Old.md?");
    }
}
