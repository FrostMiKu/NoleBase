//! Derived index of canonical attachment URIs referenced by managed notes.
//!
//! The index parses each managed Markdown/MBDown file (daily/, data/,
//! archives/) with the same MBDown parser the renderer uses and records the
//! canonical attachment URIs (`nole://attachment/<id>`) that appear as
//! clickable/renderable reference targets: Markdown link destinations
//! (including autolinks and reference-style links), image destinations,
//! MBDown embeds (`![[...]]`), and `[link=...]` tag targets. Raw URI strings
//! in prose, fenced or inline code, HTML comments, escaped text, and wiki-link
//! bodies never produce those events and are therefore not references: the
//! renderer itself does not turn them into attachment links.
//!
//! Per URI the index preserves the total occurrence count and the distinct
//! set of referencing managed notes. The indexer rebuilds once at startup and
//! refreshes incrementally from file watcher events via
//! [`AttachmentIndexer::paths_changed`], publishing every snapshot together
//! with the revision of the last applied change batch so consumers can detect
//! stale snapshots (see [`AttachmentIndexer::try_latest_update`]).
//!
//! The index stores URIs as plain strings keyed on the canonical form produced
//! by [`AttachmentUri`]'s `Display`: the attachment store owns the typed
//! URI/identity, and keeping this module string-keyed avoids a second
//! attachment abstraction while still giving the browser and delete guard
//! exact canonical URIs to compare against.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use mbdown::{Container, Event, InlineTag, Node, SpannedEvent};

use crate::attachment::AttachmentUri;
use crate::storage::Storage;

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

    #[cfg(test)]
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

    #[cfg(test)]
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
    /// last requested revision, paired with the revision of the last applied
    /// change batch (0 for the initial build). Consumers use the revision to
    /// reject actions based on stale index state.
    pub fn try_latest_update(&self) -> Option<(u64, AttachmentReferenceIndex)> {
        let requested = self.requested_revision.get();
        self.updates
            .try_iter()
            .filter(|(revision, _)| *revision >= requested)
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

/// Every canonical attachment URI in `text` that the MBDown renderer would
/// render as an attachment reference, in source order, including duplicates:
/// Markdown link destinations (including autolinks and reference-style links),
/// image destinations, MBDown embeds, and `[link=...]` tag targets. URIs
/// inside code, HTML comments, escaped text, or plain prose never produce
/// those events and are therefore not references.
pub fn find_attachment_uris(text: &str) -> Vec<String> {
    let mut uris = Vec::new();
    let Ok(document) = mbdown::parse(text) else {
        return uris;
    };
    collect_node_attachment_uris(document.nodes(), &mut uris);
    uris
}

fn collect_node_attachment_uris(nodes: &[Node<'_>], uris: &mut Vec<String>) {
    for node in nodes {
        match node {
            Node::Markdown(markdown) => collect_event_attachment_uris(markdown.events(), uris),
            Node::Box { children, .. }
            | Node::Center { children }
            | Node::Right { children }
            | Node::Indent { children, .. }
            | Node::Columns { children, .. }
            | Node::Column { children, .. } => collect_node_attachment_uris(children, uris),
        }
    }
}

fn collect_event_attachment_uris(events: &[SpannedEvent<'_>], uris: &mut Vec<String>) {
    for item in events {
        match &item.event {
            Event::Start(Container::Link { target, .. })
            | Event::Start(Container::Image { target, .. }) => {
                push_canonical_attachment_uri(target, uris);
            }
            Event::Embed(target) => push_canonical_attachment_uri(target, uris),
            Event::InlineTag(InlineTag {
                name,
                value: Some(target),
                closing: false,
                ..
            }) if name == "link" => push_canonical_attachment_uri(target, uris),
            _ => {}
        }
    }
}

/// Push `target` when it strictly parses as a canonical attachment URI.
/// Strict parsing keeps the index free of id-format and digest assumptions
/// and guarantees every key is the canonical `nole://attachment/<id>` form
/// [`AttachmentUri`]'s `Display` produces, so lookups from typed attachment
/// ids always match.
fn push_canonical_attachment_uri(target: &str, uris: &mut Vec<String>) {
    if let Ok(uri) = AttachmentUri::parse(target) {
        uris.push(uri.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage_with(directory: &tempfile::TempDir) -> Storage {
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage
    }

    /// A distinct, strictly canonical `nole://attachment/<uuid>` per seed.
    fn uri(seed: u8) -> String {
        format!("nole://attachment/{seed:08x}-0000-4000-8000-000000000000")
    }

    #[test]
    fn scanner_indexes_links_images_embeds_and_link_tags_only() {
        let a = uri(1);
        let b = uri(2);
        let text = format!(
            "see [report]({a}) and ![img]({b}) or ![[{a}]] and [link={b}]tag[/link]\n\
             autolink <{a}>\n\
             ref [ref][refdef]\n\
             \n\
             [refdef]: {b}\n\
             \n\
             ```text\n\
             {a} in a fence\n\
             ```\n\
             \n\
             inline `{a}` and <!-- {b} --> comment\n\
             escaped \\[x]({b})\n\
             bare {a} in prose\n\
             wikilink [[{b}]]\n"
        );
        let found = find_attachment_uris(&text);
        assert_eq!(
            found,
            vec![a.clone(), b.clone(), a.clone(), b.clone(), a, b]
        );
    }

    #[test]
    fn scanner_rejects_non_canonical_uris() {
        // Not a valid attachment id (not a UUID).
        let not_uuid = "nole://attachment/0000000000000000000000000000000000000000";
        assert!(find_attachment_uris(&format!("[x]({not_uuid})")).is_empty());
        // Uppercase ids are not canonical.
        let upper = "nole://attachment/123e4567-e89b-42d3-a456-426614174000".to_uppercase();
        assert!(find_attachment_uris(&format!("[x]({upper})")).is_empty());
        // The legacy digest scheme is not canonical.
        assert!(
            find_attachment_uris(&format!("[x](nole-attachment://sha256/{})", "a".repeat(64)))
                .is_empty()
        );
    }

    #[test]
    fn index_counts_shared_references_across_managed_groups() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
        let b = uri(2);
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
        assert!(!index.is_referenced(&uri(3)));
    }

    #[test]
    fn count_tracks_occurrences_and_locations_track_distinct_notes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
        fs::write(
            storage.data_dir.join("Note.md"),
            format!("[a]({a}) twice [a]({a})\n"),
        )
        .unwrap();
        fs::write(
            storage.daily_dir.join("2026-07-28.md"),
            format!("[a]({a})\n"),
        )
        .unwrap();

        let index = AttachmentReferenceIndex::build(&storage);
        assert_eq!(index.reference_count(&a), 3, "occurrences, not notes");
        assert_eq!(index.locations(&a).len(), 2, "distinct notes");
    }

    #[test]
    fn code_samples_comments_and_escaped_text_are_not_references() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
        fs::write(
            storage.data_dir.join("Note.md"),
            format!("```text\n{a}\n```\n<!-- {a} -->\n`{a}`\n\\[{a}](x)\nbare {a}\n"),
        )
        .unwrap();
        let index = AttachmentReferenceIndex::build(&storage);
        assert!(!index.is_referenced(&a));
        assert_eq!(index.reference_count(&a), 0);
    }

    #[test]
    fn references_inside_mbdown_containers_are_indexed() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
        fs::write(
            storage.data_dir.join("Note.mb"),
            format!("[box]\n[link={a}]open[/link]\n[/box]\n"),
        )
        .unwrap();
        let index = AttachmentReferenceIndex::build(&storage);
        assert_eq!(index.reference_count(&a), 1);
        assert_eq!(index.locations(&a).len(), 1);
    }

    #[test]
    fn refresh_removes_references_and_drops_deleted_files() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let a = uri(1);
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
        let a = uri(1);
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
    fn worker_publishes_incremental_updates_with_revisions() {
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let indexer = AttachmentIndexer::spawn(storage.clone());
        let (initial_revision, initial) = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(initial_revision, 0, "initial build publishes revision 0");
        let a = uri(1);
        assert_eq!(initial.reference_count(&a), 0);

        let note = storage.data_dir.join("Live.md");
        fs::write(&note, format!("[a]({a})\n")).unwrap();
        indexer.paths_changed(vec![note.clone()]);
        let (revision, created) = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(revision, 1);
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

    #[test]
    fn try_latest_update_returns_revisioned_snapshot_and_discards_stale() {
        let (command_sender, _command_receiver) = mpsc::channel();
        let (update_sender, update_receiver) = mpsc::channel();
        let indexer = AttachmentIndexer {
            commands: command_sender,
            updates: update_receiver,
            requested_revision: Cell::new(1),
        };
        let a = uri(1);
        let mut fresh = AttachmentReferenceIndex::default();
        fresh
            .files
            .insert(PathBuf::from("Note.md"), vec![a.clone()]);
        fresh.rebuild_references();
        assert_eq!(fresh.reference_count(&a), 1);

        // A snapshot queued before the requested revision is discarded.
        update_sender
            .send((0, AttachmentReferenceIndex::default()))
            .unwrap();
        update_sender.send((1, fresh.clone())).unwrap();
        let (revision, update) = indexer.try_latest_update().unwrap();
        assert_eq!(revision, 1);
        assert_eq!(update.reference_count(&a), fresh.reference_count(&a));
        assert_eq!(update.locations(&a), fresh.locations(&a));
    }
}
