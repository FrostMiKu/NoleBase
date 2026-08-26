//! Wiki-link tools backed by the shared wiki-link index and storage.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::util::{
    display_path, limited_diff, required_string, truncate_with_marker, MAX_DIFF_BYTES,
};
use crate::agent::{ApprovalGate, ApprovalKind, ApprovalRequest, Tool, ToolExecutionPolicy};
use crate::storage::Storage;
use crate::wiki_link_index::{
    matching_wiki_link_spans, replace_wiki_link_spans, WikiLinkIndexHandle,
};

/// Reject `target` unless it is a legal `[[wikilink]]` target for MBDown:
/// non-empty, free of surrounding whitespace, and without brackets or
/// newlines. A target containing `[`, `]`, `\n`, or `\r` would terminate or
/// inject markup into the `[[...]]` span rather than rename one wiki link, so
/// such targets are rejected before any diff is shown or file is written.
fn validate_wiki_target(target: &str) -> Result<()> {
    if target.is_empty() {
        bail!("wiki targets must not be empty");
    }
    if target.trim() != target {
        bail!("wiki target must not have leading or trailing whitespace");
    }
    if target.contains(['[', ']', '\n', '\r']) {
        bail!("wiki target must not contain brackets or newlines");
    }
    Ok(())
}

/// Create a unique hidden temp file next to `target` (same directory, same
/// filesystem) and write `content` into it, so the commit phase can atomically
/// rename it over the target. Mirrors the export staging convention;
/// `create_new` guarantees a coincidental file is never clobbered.
fn stage_replacement(target: &Path, content: &str) -> Result<PathBuf> {
    use std::io::Write as _;

    let parent = target
        .parent()
        .context("wiki rename target has no parent directory")?;
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("wiki-note");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..16u32 {
        let temp_name = format!(
            ".{name}.nole-rename-{}-{nonce}-{attempt}.tmp",
            std::process::id()
        );
        let temp = parent.join(temp_name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        match options.open(&temp) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(content.as_bytes()) {
                    let _ = std::fs::remove_file(&temp);
                    return Err(error)
                        .with_context(|| format!("writing staged file {}", temp.display()));
                }
                return Ok(temp);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating staged file next to {}", target.display()))
            }
        }
    }
    bail!(
        "could not allocate a staged wiki rename file next to {}",
        target.display()
    )
}

/// Resolve `target` to the managed notes it names, returning the distinct
/// paths sorted. Empty when the wiki index has not published yet.
fn resolve_paths(index: &WikiLinkIndexHandle, target: &str) -> Result<Vec<PathBuf>> {
    index
        .with_index(|index| index.resolve(target))
        .context("wiki-link index is still building")
}

pub struct Wikilink {
    root: PathBuf,
    index: WikiLinkIndexHandle,
}

impl Wikilink {
    pub fn new(root: &Path, index: WikiLinkIndexHandle) -> Result<Self> {
        Ok(Self {
            root: crate::agent::canonical_root(root)?,
            index,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Wikilink {
    fn name(&self) -> &'static str {
        "wikilink"
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Resolve a `[[wikilink]]` target to the managed notes it names, by file name or stem (case-insensitive). Reports a unique match, multiple candidates across daily/, data/, and archives/, or a missing target."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "Wiki target, e.g. Project or Project.md" }
            },
            "required": ["target"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let target = required_string(input, "target")?.trim();
        let candidates = resolve_paths(&self.index, target)?;
        let resolved = candidates
            .iter()
            .map(|path| json!({ "path": display_path(&self.root, path) }))
            .collect::<Vec<_>>();
        serde_json::to_string_pretty(&json!({
            "target": target,
            "status": if resolved.is_empty() { "missing" } else if resolved.len() == 1 { "unique" } else { "ambiguous" },
            "matches": resolved.len(),
            "candidates": resolved,
        }))
        .context("encoding wiki-link resolution")
    }
}

pub struct Backlinks {
    root: PathBuf,
    index: WikiLinkIndexHandle,
}

impl Backlinks {
    pub fn new(root: &Path, index: WikiLinkIndexHandle) -> Result<Self> {
        Ok(Self {
            root: crate::agent::canonical_root(root)?,
            index,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Backlinks {
    fn name(&self) -> &'static str {
        "backlinks"
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "List every managed note that links to the given note with `[[...]]`. The target is resolved by file name or stem (case-insensitive); returns the distinct referencing notes."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "Note name or stem, e.g. Project or Project.md" }
            },
            "required": ["target"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let target = required_string(input, "target")?.trim();
        let resolved = resolve_paths(&self.index, target)?;
        if resolved.len() != 1 {
            return serde_json::to_string_pretty(&json!({
                "target": target,
                "status": if resolved.is_empty() { "missing" } else { "ambiguous" },
                "matches": resolved.len(),
                "candidates": resolved
                    .iter()
                    .map(|path| display_path(&self.root, path))
                    .collect::<Vec<_>>(),
            }))
            .context("encoding backlink resolution");
        }
        let backlinks = self
            .index
            .with_index(|index| index.backlinks(&resolved[0]))
            .context("wiki-link index is still building")?;
        serde_json::to_string_pretty(&json!({
            "target": target,
            "note": display_path(&self.root, &resolved[0]),
            "status": "ok",
            "count": backlinks.len(),
            "backlinks": backlinks
                .iter()
                .map(|path| display_path(&self.root, path))
                .collect::<Vec<_>>(),
        }))
        .context("encoding backlink list")
    }
}

pub struct RenameWikilink {
    storage: Storage,
    root: PathBuf,
    index: WikiLinkIndexHandle,
    gate: ApprovalGate,
}

impl RenameWikilink {
    pub fn new(root: &Path, index: WikiLinkIndexHandle, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            root: crate::agent::canonical_root(root)?,
            index,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for RenameWikilink {
    fn name(&self) -> &'static str {
        "rename_wikilink"
    }

    fn description(&self) -> &'static str {
        "Rename one wiki-link target (`[[from]]` -> `[[to]]`) across daily notes, active notes, and archives. Only real wiki links are rewritten; code, comments, and embeds are untouched."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Existing wiki target, without [[ ]]" },
                "to": { "type": "string", "description": "New wiki target, without [[ ]]" }
            },
            "required": ["from", "to"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let from = required_string(input, "from")?;
        let to = required_string(input, "to")?;
        validate_wiki_target(from)?;
        validate_wiki_target(to)?;
        if from.eq_ignore_ascii_case(to) {
            bail!("source and destination wiki targets are the same");
        }
        let plan = WikiRenamePlan::prepare(&self.storage, &self.index, from, to)?;
        let mentions = plan.mentions;
        let mut diff = format!(
            "Rename [[{}]] to [[{}]] in {} documents ({} mentions)\n\n",
            plan.from,
            plan.to,
            plan.changes.len(),
            mentions
        );
        for change in &plan.changes {
            let label = display_path(&self.root, &change.path);
            diff.push_str(&limited_diff(&change.before, &change.after, &label, &label));
            diff.push('\n');
            if diff.len() > MAX_DIFF_BYTES {
                diff = truncate_with_marker(&diff, MAX_DIFF_BYTES);
                break;
            }
        }
        self.gate
            .request(ApprovalRequest {
                title: format!("Rename [[{}]] to [[{}]]", plan.from, plan.to),
                message: diff,
                kind: ApprovalKind::Diff,
            })
            .await?;
        let paths = plan.apply(&self.storage)?;
        serde_json::to_string_pretty(&json!({
            "from": from,
            "to": to,
            "documents": paths.len(),
            "mentions": mentions,
            "paths": paths
                .iter()
                .map(|path| display_path(&self.root, path))
                .collect::<Vec<_>>(),
        }))
        .context("encoding wiki rename result")
    }
}

struct WikiRenamePlan {
    from: String,
    to: String,
    mentions: usize,
    changes: Vec<WikiFileChange>,
}

struct WikiFileChange {
    path: PathBuf,
    before: String,
    after: String,
    mentions: usize,
}

impl WikiRenamePlan {
    fn prepare(
        storage: &Storage,
        index: &WikiLinkIndexHandle,
        from: &str,
        to: &str,
    ) -> Result<Self> {
        let paths = index
            .with_index(|index| index.locations_ignoring_case(from))
            .context("wiki-link index is still building")?;
        let mut changes = Vec::new();
        for path in paths {
            let metadata = std::fs::symlink_metadata(&path)
                .with_context(|| format!("checking {}", path.display()))?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            // Reject candidates that resolve outside the managed directories:
            // a managed directory swapped for a symlink must never redirect
            // the rename to files outside daily/, data/, or archives/.
            storage.validate_wiki_note(&path)?;
            let before = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let spans = matching_wiki_link_spans(&before, from);
            if spans.is_empty() {
                continue;
            }
            let after = replace_wiki_link_spans(&before, &spans, to);
            changes.push(WikiFileChange {
                path,
                before,
                after,
                mentions: spans.len(),
            });
        }
        if changes.is_empty() {
            bail!("wiki target [[{from}]] was not found");
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        let mentions = changes.iter().map(|change| change.mentions).sum();
        Ok(Self {
            from: from.to_string(),
            to: to.to_string(),
            mentions,
            changes,
        })
    }

    fn apply(self, storage: &Storage) -> Result<Vec<PathBuf>> {
        // Re-validate every target and confirm it still holds the exact
        // content the approval diff was built from. The check-then-act race
        // between this re-read and the commit below is the accepted TOCTOU
        // and is intentionally not closed here.
        for change in &self.changes {
            storage.validate_wiki_note(&change.path)?;
            let current = std::fs::read_to_string(&change.path)
                .with_context(|| format!("rechecking {}", change.path.display()))?;
            if current != change.before {
                bail!(
                    "{} changed while the wiki rename was being reviewed",
                    change.path.display()
                );
            }
        }
        // Stage every replacement to a sibling temp file first, so an
        // ordinary write failure (full disk, permissions) leaves every
        // original untouched instead of partially renaming the set.
        let mut staged: Vec<Option<PathBuf>> = vec![None; self.changes.len()];
        for (index, change) in self.changes.iter().enumerate() {
            match stage_replacement(&change.path, &change.after) {
                Ok(temp) => staged[index] = Some(temp),
                Err(error) => {
                    for pending in staged.iter().flatten() {
                        let _ = std::fs::remove_file(pending);
                    }
                    return Err(error);
                }
            }
        }
        // Commit by renaming each staged file over its target. A
        // same-directory rename cannot fail once staging succeeded under
        // ordinary conditions; if one does anyway, restore the already
        // committed files from the in-memory originals (best effort).
        for (index, change) in self.changes.iter().enumerate() {
            let temp = staged[index].take().expect("staged replacement exists");
            if let Err(error) = std::fs::rename(&temp, &change.path) {
                for done in self.changes.iter().take(index) {
                    let _ = std::fs::write(&done.path, &done.before);
                }
                for pending in staged.iter().flatten() {
                    let _ = std::fs::remove_file(pending);
                }
                return Err(error).with_context(|| format!("updating {}", change.path.display()));
            }
        }
        let paths = self.changes.into_iter().map(|change| change.path).collect();
        Ok(paths)
    }
}
