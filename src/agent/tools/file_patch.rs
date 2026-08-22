//! The `edit` tool: applies an upstream-style hashline patch.
//!
//! One tool call takes a single `patch` string made of zero or more
//! `[PATH#TAG]` sections. Every section is preflighted (path safety, snapshot
//! anchor, planning, drift rebase, read coverage, staged candidate) before
//! publication, so a multi-section patch commits as one complete operation.
//! After publication each surviving file is re-scanned and re-anchored in the
//! [`crate::agent::SnapshotStore`] with the whole file marked seen; consecutive
//! edits therefore reuse the latest read state.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::file_edit::{inspect_text_file, prepare_edit, FileInspection, PreparedEdit};
use super::file_ops::{move_to_new_file, path_zone, resolve_mutation_path, PathZone};
use super::util::required_string;
use super::workspace_quota::{check_workspace_staged_write, workspace_edit_budget};
use super::write_policy::{post_write_result, validate_write_preconditions, WriteSource};
use crate::agent::hashline::{
    parse_patch, plan_section, rebase_edits, syntax_for_path, FileOp, PlannedFile, RegisterBank,
    Section,
};
use crate::agent::snapshots::{
    normalize_hash_line, SnapshotIdentityHasher, SnapshotStore, SnapshotTagHasher,
};
use crate::agent::{canonical_root, ApprovalGate, ApprovalKind, ApprovalRequest, Tool};

/// Largest file the tool will plan a hashline edit for.
const MAX_PLANNING_BYTES: u64 = 8 * 1024 * 1024;
/// Byte cap for the whole tool result string.
const MAX_RESULT_BYTES: usize = 8 * 1024;
/// Context lines shown before and after each changed region in the result.
const RESULT_CONTEXT_LINES: usize = 2;

pub struct Edit {
    root: PathBuf,
    gate: ApprovalGate,
    reads: Arc<SnapshotStore>,
    registers: Arc<RegisterBank>,
}

/// Structured end-of-file insertion for the common append case. It retains
/// the same read snapshot, approval, drift detection, and atomic publication
/// semantics as [`Edit`] while presenting structured append input to the model.
pub struct Append {
    edit: Edit,
}

impl Append {
    pub fn new(
        root: &Path,
        gate: ApprovalGate,
        reads: Arc<SnapshotStore>,
        registers: Arc<RegisterBank>,
    ) -> Result<Self> {
        Ok(Self {
            edit: Edit::new(root, gate, reads, registers)?,
        })
    }
}

impl Edit {
    pub fn new(
        root: &Path,
        gate: ApprovalGate,
        reads: Arc<SnapshotStore>,
        registers: Arc<RegisterBank>,
    ) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            gate,
            reads,
            registers,
        })
    }

    /// Resolve an existing edit target through its canonical parent and a real
    /// file path in an editable zone. The target may be a Nole-relative path or
    /// an external absolute path.
    fn resolve_path(&self, input: &str) -> Result<PathBuf> {
        let unresolved = resolve_mutation_path(&self.root, input)?;
        let metadata = fs::symlink_metadata(&unresolved)
            .with_context(|| format!("checking {}", unresolved.display()))?;
        if metadata.file_type().is_symlink() {
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
        Ok(path)
    }

    /// Resolve the destination of an `MV` op: canonical parent, fresh leaf, and
    /// an editable zone. The destination may be a Nole-relative path or an
    /// external absolute path.
    fn resolve_destination(&self, dest_text: &str) -> Result<PathBuf> {
        let destination = resolve_mutation_path(&self.root, dest_text)?;
        match path_zone(&self.root, &destination) {
            PathZone::Config => bail!("edit cannot move files inside config/"),
            PathZone::Attachments => {
                bail!("generic file tools cannot operate inside attachments/")
            }
            _ => {}
        }
        match fs::symlink_metadata(&destination) {
            Ok(_) => bail!("destination already exists: {dest_text}"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
            Err(error) => Err(error).with_context(|| format!("checking destination {dest_text}")),
        }
    }

    /// Preflight one section: anchor the snapshot, plan, recover drift, gate
    /// read coverage, and stage the prepared candidate; `REM` follows its
    /// removal path.
    fn preflight(&self, section: &Section, relative: &str, path: PathBuf) -> Result<SectionPlan> {
        let inspection = inspect_text_file(&path)?;
        if inspection.len > MAX_PLANNING_BYTES {
            bail!("file is too large for a hashline edit; use write");
        }
        if self.reads.reconcile_dirty(&path, inspection.identity)? {
            bail!("file changed since read; read it again before editing");
        }
        let snapshot = self
            .reads
            .head(&path)?
            .context("edit requires read on the same path in the current Agent session first")?;
        let anchored = if snapshot.tag.eq_ignore_ascii_case(&section.tag) {
            snapshot
        } else if let Some(older) = self.reads.by_tag(&path, &section.tag)? {
            // The model is anchored on an older retained version: recovery.
            older
        } else {
            bail!("snapshot tag mismatch for {relative}; read the file again before editing");
        };

        let raw_text = fs::read_to_string(&path)
            .with_context(|| format!("reading {} for planning", path.display()))?;
        let raw_lines: Vec<String> = raw_text.lines().map(str::to_owned).collect();

        let drifting = anchored.identity != inspection.identity;
        let planned = if drifting {
            // The edits must be planned against the revision the model read,
            // then re-based onto the current file.
            let Some(base) = anchored.text.clone() else {
                let _ = self.reads.invalidate(&path);
                bail!("file changed since read; read it again before editing");
            };
            let base_lines: Vec<String> = base.lines().map(str::to_owned).collect();
            let mut planned = plan_section(
                section,
                &base_lines,
                syntax_for_path(&path),
                &self.registers,
            )?;
            rebase_edits(&base, &normalize_text(&raw_text), &mut planned)?;
            planned
        } else {
            plan_section(section, &raw_lines, syntax_for_path(&path), &self.registers)?
        };

        for &(start, end) in &planned.touched {
            anchored.ensure_seen(start, end)?;
        }

        let mut dest = None;
        let mut dest_relative = String::new();
        if let Some(FileOp::Move { dest: text }) = &planned.file_op {
            dest_relative.clone_from(text);
            dest = Some(self.resolve_destination(text)?);
        }

        let prepared =
            if planned.edits.is_empty() || matches!(planned.file_op, Some(FileOp::Remove)) {
                None
            } else {
                let budget = workspace_edit_budget(&self.root, &path)?;
                match prepare_edit(&path, relative, &planned.edits, &inspection, budget) {
                    Ok(prepared) => Some(prepared),
                    Err(error) => {
                        let _ = self.reads.invalidate(&path);
                        return Err(error);
                    }
                }
            };

        Ok(SectionPlan {
            relative: relative.to_string(),
            path,
            inspection,
            planned,
            prepared,
            dest,
            dest_relative,
            unchanged: false,
            post_tag: None,
            post_total: None,
            post_lines: None,
            post_display: None,
        })
    }

    /// Request approval for the whole patch, covering every touched path and
    /// every move destination. The gate decides by path: APPROVE always asks,
    /// APPROVE asks for every path, AUTO asks for paths leaving the Nole root,
    /// and YOLO approves immediately.
    async fn approve(&self, plans: &[SectionPlan]) -> Result<()> {
        let mut paths = Vec::with_capacity(plans.len() * 2);
        for plan in plans {
            paths.push(plan.path.as_path());
            if let Some(dest) = &plan.dest {
                paths.push(dest.as_path());
            }
        }
        let mut message = String::new();
        for plan in plans {
            if let Some(prepared) = &plan.prepared {
                message.push_str(&prepared.diff);
                if !message.ends_with('\n') {
                    message.push('\n');
                }
            }
            match &plan.planned.file_op {
                Some(FileOp::Move { dest }) => {
                    message.push_str(&format!("move {} -> {dest}\n", plan.relative));
                }
                Some(FileOp::Remove) => message.push_str(&format!("delete {}\n", plan.relative)),
                None => {}
            }
        }
        let title = if plans.len() == 1 {
            format!("Edit {}", plans[0].relative)
        } else {
            format!("Edit {} files", plans.len())
        };
        self.gate
            .request_for_paths(
                ApprovalRequest {
                    title,
                    message,
                    kind: ApprovalKind::Diff,
                },
                &paths,
            )
            .await?;
        Ok(())
    }

    /// Publish the whole patch: verify every target is unchanged since
    /// preflight, validate preconditions and workspace staging, write the
    /// staged candidates, then apply file operations (`REM` then `MV`).
    fn publish(&self, plans: &mut [SectionPlan]) -> Result<()> {
        // Every pre-publication check completes before the first write, so a
        // multi-section patch publishes as one complete operation.
        // A patch whose body reproduces the original bytes is a no-op: drop the
        // staged candidate so publication neither churns the mtime nor wakes the
        // file watcher. Identity is normalized, so a whitespace-only rewrite is
        // still a real change and is caught by the length comparison.
        for plan in plans.iter_mut() {
            let unchanged = plan.prepared.as_ref().is_some_and(|prepared| {
                prepared.candidate_identity == prepared.original_identity
                    && prepared.candidate_len == plan.inspection.len
            });
            if unchanged {
                plan.prepared = None;
                plan.unchanged = true;
            }
        }
        for plan in plans.iter() {
            let current = inspect_text_file(&plan.path)?;
            if current.identity != plan.inspection.identity {
                let _ = self.reads.invalidate(&plan.path);
                bail!("file changed before editing; read it again and retry");
            }
            if let Some(prepared) = &plan.prepared {
                let (destination, staged) = match &plan.dest {
                    Some(dest) => (dest.as_path(), prepared.path()),
                    None => (plan.path.as_path(), prepared.path()),
                };
                validate_write_preconditions(
                    &self.root,
                    destination,
                    WriteSource::File(prepared.path()),
                )?;
                check_workspace_staged_write(
                    &self.root,
                    destination,
                    staged,
                    prepared.candidate_len,
                )?;
            }
        }
        for plan in plans.iter_mut() {
            if let Some(prepared) = plan.prepared.take() {
                prepared.publish(&plan.path)?;
            }
        }
        for plan in plans.iter() {
            match &plan.planned.file_op {
                Some(FileOp::Remove) => {
                    fs::remove_file(&plan.path)
                        .with_context(|| format!("deleting {}", plan.path.display()))?;
                    self.reads.consume(&plan.path)?;
                }
                Some(FileOp::Move { .. }) => {
                    let destination = plan.dest.as_ref().context("move destination is missing")?;
                    move_to_new_file(&plan.path, destination).with_context(|| {
                        format!(
                            "moving {} to {}",
                            plan.path.display(),
                            destination.display()
                        )
                    })?;
                    self.reads.relocate(&plan.path, destination)?;
                }
                None => {}
            }
        }
        Ok(())
    }

    /// Re-anchor every surviving file so consecutive edits use the current
    /// snapshot.
    /// The stored identity is the raw-byte identity (matching
    /// [`inspect_text_file`]), so matching content preserves the anchor.
    fn anchor(&self, plans: &mut [SectionPlan]) -> Result<()> {
        for plan in plans.iter_mut() {
            let Some((location, display)) = plan.surviving_location() else {
                continue;
            };
            let location = location.to_path_buf();
            let display = display.to_string();
            let scanned = scan_published_file(&location)?;
            let tag = self.reads.record(
                location.clone(),
                scanned.identity,
                scanned.tag.clone(),
                scanned.total_lines,
                Some(scanned.text),
                (0, scanned.total_lines),
            )?;
            let lines = fs::read_to_string(&location)
                .with_context(|| format!("reading {}", location.display()))?
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            plan.post_tag = Some(tag);
            plan.post_total = Some(scanned.total_lines);
            plan.post_lines = Some(lines);
            plan.post_display = Some(display);
        }
        Ok(())
    }

    fn render_result(&self, plans: &[SectionPlan]) -> String {
        let mut out = CappedResult::new(MAX_RESULT_BYTES);
        for plan in plans {
            if plan.unchanged || (plan.planned.edits.is_empty() && plan.planned.file_op.is_none()) {
                out.push(&format!("no changes needed for {}\n", plan.relative));
                continue;
            }
            let Some(tag) = &plan.post_tag else {
                // `REM` sections carry their removal operation directly.
                continue;
            };
            let total = plan.post_total.unwrap_or(0);
            let display = plan
                .post_display
                .as_deref()
                .unwrap_or(plan.relative.as_str());
            out.push(&format!("edited {}\n", plan.relative));
            out.push(&format!("[{display}#{tag}] {total} lines\n"));
            if let Some(lines) = &plan.post_lines {
                append_edit_windows(&mut out, &plan.planned, lines);
            }
        }
        for plan in plans {
            match &plan.planned.file_op {
                Some(FileOp::Remove) => out.push(&format!("removed {}\n", plan.relative)),
                Some(FileOp::Move { dest }) => {
                    out.push(&format!("moved {} -> {dest}\n", plan.relative))
                }
                None => {}
            }
        }
        for plan in plans {
            for warning in &plan.planned.warnings {
                out.push(&format!("{}: {warning}\n", plan.relative));
            }
            for resolution in &plan.planned.resolutions {
                out.push(&format!("{}: {resolution}\n", plan.relative));
            }
        }
        for plan in plans {
            if let Some((location, display)) = plan.surviving_location() {
                out.push(&post_write_result(String::new(), display, location));
            }
        }
        out.into_string()
    }
}

/// Everything resolved and staged for one patch section before publication.
struct SectionPlan {
    relative: String,
    path: PathBuf,
    inspection: FileInspection,
    planned: PlannedFile,
    prepared: Option<PreparedEdit>,
    dest: Option<PathBuf>,
    dest_relative: String,
    unchanged: bool,
    post_tag: Option<String>,
    post_total: Option<usize>,
    post_lines: Option<Vec<String>>,
    post_display: Option<String>,
}

impl SectionPlan {
    /// Where the published file lives, if anywhere: the destination after a
    /// move, the source otherwise, and nowhere after a removal.
    fn surviving_location(&self) -> Option<(&Path, &str)> {
        match &self.planned.file_op {
            Some(FileOp::Remove) => None,
            Some(FileOp::Move { .. }) => Some((self.dest.as_ref()?, self.dest_relative.as_str())),
            None => Some((&self.path, self.relative.as_str())),
        }
    }
}

/// The exact-byte scan used to re-anchor a published file.
struct ScannedFile {
    identity: [u8; 32],
    tag: String,
    total_lines: usize,
    text: String,
}

/// Scan a published file for re-anchoring. The tag, identity, and retained
/// text all use the store's per-line normalization (trailing spaces, tabs,
/// and CR stripped, then a single `\n` per line), exactly matching both
/// `inspect_text_file` and the read pipeline — matching content preserves the
/// anchor, and CRLF/LF or trailing-whitespace normalization shares that anchor
/// identity.
fn scan_published_file(path: &Path) -> Result<ScannedFile> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("reading {} after edit", path.display()))?;
    let normalized = normalize_text(&content);
    let mut identity = SnapshotIdentityHasher::default();
    identity.update(normalized.as_bytes());
    let mut tag = SnapshotTagHasher::default();
    tag.update(normalized.as_bytes());
    Ok(ScannedFile {
        identity: identity.finish(),
        tag: tag.finish(),
        total_lines: content.lines().count(),
        text: normalized,
    })
}

/// Normalize file text exactly as the snapshot store does: each line's body
/// (with any trailing `\r`) stripped of trailing spaces, tabs, and CR, then
/// newline-terminated. Final-newline normalization yields a trailing `\n`,
/// giving equivalent line-ending styles one snapshot identity.
fn normalize_text(content: &str) -> String {
    let mut normalized = String::with_capacity(content.len());
    for segment in content.split_inclusive('\n') {
        let body = segment.strip_suffix('\n').unwrap_or(segment);
        let body = body.strip_suffix('\r').unwrap_or(body);
        normalized.push_str(&normalize_hash_line(body));
        normalized.push('\n');
    }
    normalized
}

/// A result string capped at `cap` bytes; further pushes are elided with `…`.
struct CappedResult {
    out: String,
    remaining: usize,
    truncated: bool,
}

impl CappedResult {
    fn new(cap: usize) -> Self {
        Self {
            out: String::new(),
            remaining: cap,
            truncated: false,
        }
    }

    fn push(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        if text.len() <= self.remaining {
            self.out.push_str(text);
            self.remaining -= text.len();
            return;
        }
        let mut end = self.remaining.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.out.push_str(&text[..end]);
        self.out.push('…');
        self.remaining = 0;
        self.truncated = true;
    }

    fn into_string(self) -> String {
        self.out
    }
}

/// Appends a numbered window of the post-edit file around every changed
/// region, with `RESULT_CONTEXT_LINES` lines of context and a blank line
/// between non-adjacent regions.
fn append_edit_windows(out: &mut CappedResult, planned: &PlannedFile, lines: &[String]) {
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut delta = 0isize;
    for edit in &planned.edits {
        let post_start = (edit.start_line as isize + delta) as usize;
        let post_end = (edit.start_line as isize + delta + edit.lines.len() as isize) as usize;
        let window_start = post_start.saturating_sub(RESULT_CONTEXT_LINES);
        let window_end = (post_end + RESULT_CONTEXT_LINES).min(lines.len());
        if let Some(last) = regions.last_mut() {
            if window_start <= last.1 {
                last.1 = last.1.max(window_end);
            } else {
                regions.push((window_start, window_end));
            }
        } else {
            regions.push((window_start, window_end));
        }
        delta += edit.lines.len() as isize - (edit.end_line_exclusive - edit.start_line) as isize;
    }
    for (index, (start, end)) in regions.iter().enumerate() {
        if index > 0 {
            out.push("\n");
        }
        for (line_index, line) in lines.iter().enumerate().take(*end).skip(*start) {
            out.push(&format!("{}:{line}\n", line_index + 1));
        }
    }
}

#[async_trait::async_trait]
impl Tool for Append {
    fn name(&self) -> &'static str {
        "append"
    }

    fn description(&self) -> &'static str {
        "Append UTF-8 text to the end of one file already read in this Agent session. Pass the exact path and 4-hex tag from the latest [path#TAG] read/edit result. Use this instead of edit with PUT >$ for ordinary end appends."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "tag": {
                    "type": "string",
                    "pattern": "^[0-9A-Fa-f]{4}$",
                    "description": "Exact snapshot tag from the latest [path#TAG] result"
                },
                "content": { "type": "string", "minLength": 1 }
            },
            "required": ["path", "tag", "content"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let path = required_string(input, "path")?;
        let tag = required_string(input, "tag")?;
        let content = required_string(input, "content")?;
        if content.is_empty() {
            bail!("append content must not be empty");
        }
        let mut patch = format!("[{path}#{tag}]\nPUT >$:\n");
        for line in content.lines() {
            patch.push('+');
            patch.push_str(line);
            patch.push('\n');
        }
        self.edit.execute(&json!({"patch": patch})).await
    }
}

#[async_trait::async_trait]
impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Apply a hashline patch to files you have already read. Use append instead for ordinary end-of-file additions. A patch contains one or more [PATH#TAG] sections. \
PATH is relative to the Nole root or an absolute external path; TAG is the 4-hex snapshot tag \
from the file's latest read result (the [path#TAG] header); consecutive edits reuse the NEW tag \
each edit returns. Ops: \
PUT N.=M: replaces original inclusive lines N..M with the following + body rows; PUT N*: \
replaces the syntactic block starting at line N; PUT <N: inserts body before line N (<1 = file \
head); PUT >N: inserts body after line N (>$ = file tail); PUT >N*: inserts after the block at \
line N; CUT N.=M or CUT N* deletes a span and captures it, with an optional trailing @name; PUT \
<N @name, PUT >N @name, PUT N.=M @name or PUT N* @name pastes a captured register (@name is \
required for span replaces). REM deletes the file. MV DEST moves or renames it (DEST is relative \
to the Nole root or an absolute external path; double-quote DEST only when it contains spaces); \
line edits in the section apply to the source first. Body \
rows start on the line after a `PUT ...:` header. Every body row is +TEXT verbatim with leading spaces preserved; a bare + is a blank line. Keep body text on the following row rather than the PUT header line. Line numbers remain ORIGINAL file numbers throughout the section. \
Example replacement: [data/note.md#3F2A]\nPUT 2.=2:\n+replacement text\n. Example append: [data/note.md#3F2A]\nPUT >$:\n+new final line\n. The result shows each edited \
file's NEW [path#TAG], new line count, and numbered windows of the changed regions."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string" }
            },
            "required": ["patch"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let patch_text = required_string(input, "patch")?;
        let patch = parse_patch(patch_text)?;
        if patch.sections.is_empty() {
            bail!("patch must contain at least one section");
        }
        let mut plans = Vec::with_capacity(patch.sections.len());
        let mut seen_paths = HashSet::new();
        for section in &patch.sections {
            let relative = section.path.trim().to_string();
            if relative.is_empty() {
                bail!("patch section must name a path");
            }
            let path = self.resolve_path(&relative)?;
            if !seen_paths.insert(path.clone()) {
                bail!("patch contains multiple sections for the same path: {relative}");
            }
            plans.push(self.preflight(section, &relative, path)?);
        }
        self.approve(&plans).await?;
        self.publish(&mut plans)?;
        self.anchor(&mut plans)?;
        Ok(self.render_result(&plans))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::*;
    use crate::agent::snapshots::{snapshot_identity, snapshot_tag};
    use crate::agent::test_support::{bypass_gate, event_channel, gate, test_runtime};
    use crate::agent::{AgentEvent, ApprovalDecision, PermissionMode};

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let directory = tempdir().unwrap();
        let root = directory.path().join("root");
        fs::create_dir_all(root.join("workspace/main")).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        fs::create_dir(root.join("config")).unwrap();
        (directory, root)
    }

    /// Record a full-read snapshot (whole file seen) and return its tag.
    fn record_full(reads: &SnapshotStore, path: &Path, content: &str) -> String {
        reads
            .record(
                path.to_path_buf(),
                snapshot_identity(content),
                snapshot_tag(content),
                content.lines().count(),
                None,
                (0, content.lines().count()),
            )
            .unwrap()
    }

    fn edit_tool(root: &Path, reads: Arc<SnapshotStore>) -> Edit {
        Edit::new(
            root,
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap()
    }

    #[test]
    fn append_uses_structured_content_and_preserves_blank_lines() {
        let (_directory, root) = workspace();
        let raw = root.join("data/note.md");
        fs::write(&raw, "before").unwrap();
        let path = fs::canonicalize(&raw).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let tag = record_full(&reads, &path, "before");
        let append = Append::new(
            &root,
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();

        let result = test_runtime()
            .block_on(append.execute(&json!({
                "path": "data/note.md",
                "tag": tag,
                "content": "first appended line\n\nlast appended line"
            })))
            .unwrap();

        assert!(result.contains("edited data/note.md"));
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "before\nfirst appended line\n\nlast appended line\n"
        );
    }

    /// Extract the `[path#TAG]` tag from an edit result.
    fn extract_tag(result: &str, marker: &str) -> String {
        let prefix = format!("[{marker}#");
        let start = result.find(&prefix).expect("result contains a new tag") + prefix.len();
        result[start..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect()
    }

    #[test]
    fn own_watcher_event_preserves_new_tag_for_a_second_edit() {
        let (directory, root) = workspace();
        let raw = root.join("data/note.md");
        fs::write(&raw, "line one\nline two\nline three\n").unwrap();
        let path = fs::canonicalize(&raw).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let tag = record_full(&reads, &path, "line one\nline two\nline three\n");
        let edit = edit_tool(&root, reads.clone());

        let result = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!("[data/note.md#{tag}]\nPUT 2.=2:\n+REPLACED\n")
            })))
            .unwrap();
        assert!(result.contains("edited data/note.md"));
        assert!(result.contains("[data/note.md#"));
        assert!(result.contains("2:REPLACED"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "line one\nREPLACED\nline three\n"
        );

        // The published file is re-anchored with the whole file seen, so the
        // delayed watcher event for that write must not discard the NEW tag.
        let new_tag = extract_tag(&result, "data/note.md");
        assert_ne!(new_tag, tag);
        reads.mark_dirty(&path).unwrap();
        let second = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!("[data/note.md#{new_tag}]\nPUT 1.=1:\n+first!\n")
            })))
            .unwrap();
        assert!(second.contains("edited data/note.md"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "first!\nREPLACED\nline three\n"
        );
        assert!(directory.path().exists());
    }

    #[test]
    fn watcher_event_for_a_different_revision_requires_a_new_read() {
        let (_directory, root) = workspace();
        let raw = root.join("data/note.md");
        fs::write(&raw, "original\n").unwrap();
        let path = fs::canonicalize(&raw).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let tag = record_full(&reads, &path, "original\n");
        let edit = edit_tool(&root, reads.clone());

        fs::write(&path, "external revision\n").unwrap();
        reads.mark_dirty(&path).unwrap();
        let error = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!("[data/note.md#{tag}]\nPUT 1.=1:\n+agent revision\n")
            })))
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "file changed since read; read it again before editing"
        );
        assert!(reads.head(&path).unwrap().is_none());
    }

    #[test]
    fn two_section_patch_touches_two_files() {
        let (_directory, root) = workspace();
        let raw_a = root.join("data/a.md");
        let raw_b = root.join("data/b.md");
        fs::write(&raw_a, "A1\nA2\n").unwrap();
        fs::write(&raw_b, "B1\nB2\n").unwrap();
        let a = fs::canonicalize(&raw_a).unwrap();
        let b = fs::canonicalize(&raw_b).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let tag_a = record_full(&reads, &a, "A1\nA2\n");
        let tag_b = record_full(&reads, &b, "B1\nB2\n");
        let edit = edit_tool(&root, reads);

        let result = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!(
                    "[data/a.md#{tag_a}]\nPUT 1.=1:\n+A-new\n[data/b.md#{tag_b}]\nCUT 2.=2\n"
                )
            })))
            .unwrap();
        assert!(result.contains("edited data/a.md"));
        assert!(result.contains("edited data/b.md"));
        assert_eq!(fs::read_to_string(&a).unwrap(), "A-new\nA2\n");
        assert_eq!(fs::read_to_string(&b).unwrap(), "B1\n");
    }

    #[test]
    fn tag_mismatch_is_rejected() {
        let (_directory, root) = workspace();
        let raw = root.join("workspace/main/note.md");
        fs::write(&raw, "line\n").unwrap();
        let path = fs::canonicalize(&raw).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        record_full(&reads, &path, "line\n");
        let edit = edit_tool(&root, reads);

        let error = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": "[workspace/main/note.md#DEAD]\nPUT 1.=1:\n+x\n"
            })))
            .unwrap_err();
        assert!(error.to_string().contains("snapshot tag mismatch"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "line\n");
    }

    #[test]
    fn editing_an_unread_line_is_rejected() {
        let (_directory, root) = workspace();
        let raw = root.join("workspace/main/note.md");
        fs::write(&raw, "alpha\nbeta\ngamma\n").unwrap();
        let path = fs::canonicalize(&raw).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let tag = reads
            .record(
                path.clone(),
                snapshot_identity("alpha\nbeta\ngamma\n"),
                snapshot_tag("alpha\nbeta\ngamma\n"),
                3,
                None,
                (0, 1),
            )
            .unwrap();
        let edit = edit_tool(&root, reads);

        let error = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!("[workspace/main/note.md#{tag}]\nPUT 3.=3:\n+changed\n")
            })))
            .unwrap_err();
        assert!(error.to_string().contains("must read lines"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbeta\ngamma\n");
    }

    #[test]
    fn rem_deletes_the_file_and_its_snapshot_history() {
        let (_directory, root) = workspace();
        let raw = root.join("workspace/main/note.md");
        fs::write(&raw, "keep me?\n").unwrap();
        let path = fs::canonicalize(&raw).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let tag = record_full(&reads, &path, "keep me?\n");
        let edit = edit_tool(&root, reads.clone());

        let result = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!("[workspace/main/note.md#{tag}]\nREM\n")
            })))
            .unwrap();
        assert!(result.contains("removed workspace/main/note.md"));
        assert!(!path.exists());
        assert!(reads.head(&path).unwrap().is_none());
    }

    #[test]
    fn mv_publishes_edited_content_and_relocates_the_snapshot() {
        let (_directory, root) = workspace();
        let raw = root.join("workspace/main/from.md");
        fs::write(&raw, "first\nsecond\n").unwrap();
        let from = fs::canonicalize(&raw).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let tag = record_full(&reads, &from, "first\nsecond\n");
        let edit = edit_tool(&root, reads.clone());

        let result = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!(
                    "[workspace/main/from.md#{tag}]\nPUT 1.=1:\n+MOVED\nMV workspace/main/to.md\n"
                )
            })))
            .unwrap();
        assert!(result.contains("moved workspace/main/from.md -> workspace/main/to.md"));
        assert!(!from.exists());
        let to = fs::canonicalize(root.join("workspace/main/to.md")).unwrap();
        assert_eq!(fs::read_to_string(&to).unwrap(), "MOVED\nsecond\n");
        assert!(reads.head(&from).unwrap().is_none());
        let anchored = reads
            .head(&to)
            .unwrap()
            .expect("moved snapshot is anchored");
        assert_eq!(anchored.tag, extract_tag(&result, "workspace/main/to.md"));
    }

    #[test]
    fn drifted_file_edits_are_recovered_through_the_retained_text() {
        let (_directory, root) = workspace();
        let raw = root.join("workspace/main/note.md");
        fs::write(&raw, "alpha\nbeta\ngamma\n").unwrap();
        let path = fs::canonicalize(&raw).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        // The model read the ORIGINAL version with the full text retained.
        let tag = reads
            .record(
                path.clone(),
                snapshot_identity("alpha\nbeta\ngamma\n"),
                snapshot_tag("alpha\nbeta\ngamma\n"),
                3,
                Some("alpha\nbeta\ngamma\n".to_string()),
                (0, 3),
            )
            .unwrap();
        // The file changes elsewhere before the edit is applied.
        fs::write(&raw, "alpha\nbeta\nchanged-externally\ngamma\n").unwrap();
        let edit = edit_tool(&root, reads.clone());

        let result = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!("[workspace/main/note.md#{tag}]\nPUT 2.=2:\n+BETA\n")
            })))
            .unwrap();
        assert!(result.contains("edited workspace/main/note.md"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "alpha\nBETA\nchanged-externally\ngamma\n"
        );
    }
    #[test]
    fn auto_mode_gates_absolute_external_edits_and_denial_preserves_content() {
        let (_directory, root) = workspace();
        let outside = tempdir().unwrap();
        let path = fs::canonicalize(outside.path())
            .unwrap()
            .join("external.txt");
        fs::write(&path, "before\n").unwrap();
        let path = fs::canonicalize(path).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let tag = record_full(&reads, &path, "before\n");
        let patch = json!({
            "patch": format!("[{}#{tag}]\nPUT 1.=1:\n+after\n", path.display())
        });

        let (events, mut receiver) = event_channel();
        let (decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let edit = Edit::new(
            &root,
            gate(PermissionMode::Auto, &root, events, decision_receiver),
            reads.clone(),
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        decisions.send(ApprovalDecision::Deny).unwrap();
        let error = test_runtime().block_on(edit.execute(&patch)).unwrap_err();
        assert!(error.to_string().contains("change denied by user"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
        assert!(receiver
            .try_recv()
            .is_ok_and(|event| matches!(event, AgentEvent::Approval(_))));

        let (events, _receiver) = event_channel();
        let (decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let edit = Edit::new(
            &root,
            gate(PermissionMode::Auto, &root, events, decision_receiver),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        decisions.send(ApprovalDecision::Approve).unwrap();
        test_runtime().block_on(edit.execute(&patch)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
    }
}
