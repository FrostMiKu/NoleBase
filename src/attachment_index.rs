//! Derived index of canonical attachment URIs referenced by managed notes.
//!
//! The index scans the managed Markdown/MBDown files (daily/, data/, archives/)
//! for canonical attachment URIs (`nole-attachment://sha256/<64 lowercase hex>`)
//! and tracks, per URI, how many times it is referenced and which files do so.
//! It is rebuilt once at startup and then refreshed incrementally from file
//! watcher events via [`AttachmentIndexer::paths_changed`].
//!
//! The index stores URIs as plain strings: the canonical form is defined by the
//! URI scheme itself, and the attachment store (src/attachment.rs) owns the
//! typed URI/metadata. Keeping this module string-keyed avoids a second
//! attachment abstraction while still giving the browser and delete guard exact
//! canonical URIs to compare against (see [`AttachmentUri`]'s `Display`).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::storage::Storage;

/// The canonical scheme prefix of every attachment URI.
pub const ATTACHMENT_URI_PREFIX: &str = "nole-attachment://sha256/";

/// The canonical digest length: 256 bits as 64 lowercase hex digits.
const DIGEST_LEN: usize = 64;

/// Aggregate reference data for one canonical attachment URI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReferenceEntry {
    /// Total occurrences across all managed files.
    count: usize,
    /// Distinct managed files that reference the attachment, sorted by path.
    locations: Vec<PathBuf>,
}

/// Derived reference index: canonical attachment URI -> referencing notes.
#[derive(Clone, Debug, Default)]
pub struct AttachmentReferenceIndex {
    /// Per managed file, the canonical URIs it references (with duplicates).
    files: HashMap<PathBuf, Vec<String>>,
    /// Canonical URI -> aggregate reference data.
    references: HashMap<String, ReferenceEntry>,
}

impl AttachmentReferenceIndex {
    /// Scan every managed Markdown/MBDown file and index its attachment URIs.
    pub fn build(storage: &Storage) -> Self {
        let mut index = Self::default();
        for directory in [&storage.daily_dir, &storage.data_dir, &storage.archives_dir] {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some((path, uris)) = index_file(storage, &path) {
                    index.files.insert(path, uris);
                }
            }
        }
        index.rebuild_references();
        index
    }

    /// Re-index the given paths (created, modified, or removed). Paths that are
    /// not managed Markdown files are ignored, so watcher events for the
    /// attachments and workspace directories never disturb the index.
    pub fn refresh_paths(&mut self, storage: &Storage, paths: &[PathBuf]) {
        let mut unique = paths.iter().cloned().collect::<HashSet<_>>();
        let mut changed = false;
        for path in unique.drain() {
            if self.files.remove(&path).is_some() {
                changed = true;
            }
            if let Some((path, uris)) = index_file(storage, &path) {
                self.files.insert(path, uris);
                changed = true;
            }
        }
        if changed {
            self.rebuild_references();
        }
    }

    /// Total occurrences of the canonical URI across all managed files.
    pub fn reference_count(&self, uri: &str) -> usize {
        self.references
            .get(uri)
            .map(|entry| entry.count)
            .unwrap_or(0)
    }

    /// Distinct managed files referencing the canonical URI, sorted by path.
    pub fn locations(&self, uri: &str) -> Vec<PathBuf> {
        self.references
            .get(uri)
            .map(|entry| entry.locations.clone())
            .unwrap_or_default()
    }

    /// Whether any managed file references the canonical URI.
    pub fn is_referenced(&self, uri: &str) -> bool {
        self.references.contains_key(uri)
    }

    fn rebuild_references(&mut self) {
        self.references.clear();
        for (path, uris) in &self.files {
            for uri in uris {
                let entry = self.references.entry(uri.clone()).or_default();
                entry.count += 1;
                if !entry.locations.contains(path) {
                    entry.locations.push(path.clone());
                }
            }
        }
        for entry in self.references.values_mut() {
            entry.locations.sort();
        }
    }
}

/// Background worker that keeps an [`AttachmentReferenceIndex`] fresh from
/// file watcher events, mirroring the workspace indexer's revision protocol.
pub struct AttachmentIndexer {
    commands: Sender<IndexCommand>,
    updates: Receiver<(u64, AttachmentReferenceIndex)>,
    requested_revision: Cell<u64>,
}

enum IndexCommand {
    PathsChanged { revision: u64, paths: Vec<PathBuf> },
    Stop,
}

impl AttachmentIndexer {
    /// Spawn the worker and return immediately. The initial snapshot is
    /// published on the update channel before any `paths_changed` call.
    pub fn spawn(storage: Storage) -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (update_sender, update_receiver) = mpsc::channel();
        thread::spawn(move || index_worker(storage, command_receiver, update_sender));
        Self {
            commands: command_sender,
            updates: update_receiver,
            requested_revision: Cell::new(0),
        }
    }

    /// Queue changed paths for incremental re-indexing.
    pub fn paths_changed(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            let revision = self.requested_revision.get().saturating_add(1);
            if self
                .commands
                .send(IndexCommand::PathsChanged { revision, paths })
                .is_ok()
            {
                self.requested_revision.set(revision);
            }
        }
    }

    /// Drain the update channel and return the newest snapshot at or after the
    /// last requested revision.
    pub fn try_latest_update(&self) -> Option<AttachmentReferenceIndex> {
        let requested = self.requested_revision.get();
        self.updates
            .try_iter()
            .filter(|(revision, _)| *revision >= requested)
            .map(|(_, index)| index)
            .last()
    }
}

impl Drop for AttachmentIndexer {
    fn drop(&mut self) {
        let _ = self.commands.send(IndexCommand::Stop);
    }
}

fn index_worker(
    storage: Storage,
    commands: Receiver<IndexCommand>,
    updates: Sender<(u64, AttachmentReferenceIndex)>,
) {
    let mut index = AttachmentReferenceIndex::build(&storage);
    let mut revision = 0;
    if !apply_pending_commands(&storage, &commands, &mut index, &mut revision) {
        return;
    }
    if updates.send((revision, index.clone())).is_err() {
        return;
    }

    while let Ok(command) = commands.recv() {
        match command {
            IndexCommand::PathsChanged {
                revision: changed_revision,
                paths,
            } => {
                revision = revision.max(changed_revision);
                index.refresh_paths(&storage, &paths);
                if !apply_pending_commands(&storage, &commands, &mut index, &mut revision) {
                    return;
                }
                if updates.send((revision, index.clone())).is_err() {
                    return;
                }
            }
            IndexCommand::Stop => return,
        }
    }
}

fn apply_pending_commands(
    storage: &Storage,
    commands: &Receiver<IndexCommand>,
    index: &mut AttachmentReferenceIndex,
    revision: &mut u64,
) -> bool {
    let mut paths = Vec::new();
    for command in commands.try_iter() {
        match command {
            IndexCommand::PathsChanged {
                revision: changed_revision,
                paths: changed,
            } => {
                *revision = (*revision).max(changed_revision);
                paths.extend(changed);
            }
            IndexCommand::Stop => return false,
        }
    }
    index.refresh_paths(storage, &paths);
    true
}

/// A managed Markdown/MBDown file inside daily/, data/, or archives/.
fn index_file(storage: &Storage, path: &Path) -> Option<(PathBuf, Vec<String>)> {
    if !is_managed_markdown(storage, path) {
        return None;
    }
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let source = fs::read_to_string(path).ok()?;
    Some((path.to_path_buf(), find_attachment_uris(&source)))
}

fn is_managed_markdown(storage: &Storage, path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    if !extension.eq_ignore_ascii_case("md") && !extension.eq_ignore_ascii_case("mb") {
        return false;
    }
    matches!(
        path.parent(),
        Some(parent)
            if parent == storage.daily_dir
                || parent == storage.data_dir
                || parent == storage.archives_dir
    )
}

/// Every canonical attachment URI found in `text`, in source order, including
/// duplicates. The URI scheme is self-delimiting, so this covers link targets,
/// image embeds, and bare URIs in prose alike.
pub fn find_attachment_uris(text: &str) -> Vec<String> {
    let mut uris = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find(ATTACHMENT_URI_PREFIX) {
        let after = &rest[index + ATTACHMENT_URI_PREFIX.len()..];
        match after.get(..DIGEST_LEN) {
            Some(digest) if is_lowercase_hex(digest) => {
                let next = after.as_bytes().get(DIGEST_LEN);
                if next.is_none_or(|byte| !is_lowercase_hex_byte(*byte)) {
                    uris.push(format!("{ATTACHMENT_URI_PREFIX}{digest}"));
                    rest = &after[DIGEST_LEN..];
                    continue;
                }
            }
            _ => {}
        }
        // Not a canonical digest here; advance past this prefix so the scan
        // cannot loop on the same match.
        rest = after;
    }
    uris
}

fn is_lowercase_hex(value: &str) -> bool {
    value.bytes().all(is_lowercase_hex_byte)
}

fn is_lowercase_hex_byte(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage_with(directory: &tempfile::TempDir) -> Storage {
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage
    }

    fn uri(digest: &str) -> String {
        format!("{ATTACHMENT_URI_PREFIX}{digest}")
    }

    fn digest(seed: u8) -> String {
        format!("{:064x}", seed)
    }

    #[test]
    fn scanner_finds_links_embeds_and_prose_uris_with_boundaries() {
        let a = digest(1);
        let b = digest(2);
        let text = format!(
            "see [report]({a}) and ![]({b}) or bare {a} \n",
            a = uri(&a),
            b = uri(&b)
        );
        let found = find_attachment_uris(&text);
        assert_eq!(found, vec![uri(&a), uri(&b), uri(&a)]);

        // An over-long digest must not match; the trailing hex digit keeps the
        // first 64 from being a canonical URI on its own.
        let long = format!("{ATTACHMENT_URI_PREFIX}{}abc", "a".repeat(DIGEST_LEN));
        assert!(find_attachment_uris(&long).is_empty());

        // Uppercase hex and wrong schemes are not canonical.
        let upper = format!("{ATTACHMENT_URI_PREFIX}{}", "A".repeat(DIGEST_LEN));
        assert!(find_attachment_uris(&upper).is_empty());
        assert!(find_attachment_uris("nole-attachment://md5/aaaa").is_empty());
    }

    #[test]
    fn index_counts_shared_references_across_managed_groups() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(&digest(1));
        let b = uri(&digest(2));
        fs::write(
            storage.daily_dir.join("2026-07-28.md"),
            format!("[a]({a})\n"),
        )
        .unwrap();
        fs::write(
            storage.data_dir.join("Note.mb"),
            format!("[a]({a}) and [b]({b})\n"),
        )
        .unwrap();
        fs::write(
            storage.archives_dir.join("Old.md"),
            format!("[b]({b}) again\n"),
        )
        .unwrap();

        let index = AttachmentReferenceIndex::build(&storage);
        assert_eq!(index.reference_count(&a), 2);
        assert_eq!(index.locations(&a).len(), 2);
        assert_eq!(index.reference_count(&b), 2);
        assert_eq!(index.locations(&b).len(), 2);
        assert!(index.is_referenced(&a));
        assert!(!index.is_referenced(&uri(&digest(3))));
    }

    #[test]
    fn refresh_removes_references_and_drops_deleted_files() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(&digest(1));
        let note = storage.data_dir.join("Note.md");
        fs::write(&note, format!("[a]({a}) twice [a]({a})\n")).unwrap();

        let mut index = AttachmentReferenceIndex::build(&storage);
        assert_eq!(index.reference_count(&a), 2);
        assert_eq!(index.locations(&a).len(), 1);

        // Editing the note to drop the reference refreshes the count to zero.
        fs::write(&note, "no attachments here\n").unwrap();
        index.refresh_paths(&storage, &[note.clone()]);
        assert_eq!(index.reference_count(&a), 0);
        assert!(index.locations(&a).is_empty());
        assert!(!index.is_referenced(&a));

        // Re-adding, then deleting the file entirely, refreshes to zero again.
        fs::write(&note, format!("[a]({a})\n")).unwrap();
        index.refresh_paths(&storage, &[note.clone()]);
        assert_eq!(index.reference_count(&a), 1);
        fs::remove_file(&note).unwrap();
        index.refresh_paths(&storage, &[note]);
        assert!(!index.is_referenced(&a));
    }

    #[test]
    fn refresh_ignores_attachments_and_workspace_paths() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(&digest(1));
        fs::write(
            storage.daily_dir.join("2026-07-28.md"),
            format!("[a]({a})\n"),
        )
        .unwrap();
        let mut index = AttachmentReferenceIndex::build(&storage);
        assert_eq!(index.reference_count(&a), 1);

        let attachments = directory.path().join("attachments");
        fs::create_dir_all(&attachments).unwrap();
        let blob = attachments.join("blob.md");
        fs::write(&blob, format!("[a]({a})\n")).unwrap();
        index.refresh_paths(&storage, &[blob.clone()]);
        fs::remove_file(&blob).unwrap();
        index.refresh_paths(&storage, &[blob]);
        assert_eq!(
            index.reference_count(&a),
            1,
            "attachments md must not count"
        );
    }

    #[test]
    fn worker_publishes_incremental_updates() {
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let indexer = AttachmentIndexer::spawn(storage.clone());
        let (_, initial) = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let a = uri(&digest(1));
        assert_eq!(initial.reference_count(&a), 0);

        let note = storage.data_dir.join("Live.md");
        fs::write(&note, format!("[a]({a})\n")).unwrap();
        indexer.paths_changed(vec![note.clone()]);
        let (_, created) = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(created.reference_count(&a), 1);

        fs::write(&note, "cleared\n").unwrap();
        indexer.paths_changed(vec![note.clone()]);
        let (_, modified) = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(modified.reference_count(&a), 0);

        fs::remove_file(&note).unwrap();
        indexer.paths_changed(vec![note]);
        let (_, deleted) = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(!deleted.is_referenced(&a));
    }
}
