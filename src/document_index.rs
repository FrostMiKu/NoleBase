//! Shared parsed-document index and its disposable on-disk cache.
//!
//! Managed notes are read and parsed once, then projected into the workspace
//! search/tag index and the attachment-reference index. The cache stores only
//! derived per-file records. It is validated against the current direct files
//! by relative path, modification time, and size before any snapshot is
//! published; corruption or a format mismatch falls back to rebuilding.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::attachment_index::{collect_attachment_uris, AttachmentReferenceIndex};
use crate::storage::Storage;
use crate::wiki_link_index::{collect_wiki_links, WikiLinkIndex};
use crate::workspace_index::{collect_document_tags, WorkspaceIndex};

const CACHE_DIR: &str = "cache";
const CACHE_FILE: &str = "document-index.json";
const CACHE_FORMAT_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DocumentGroup {
    Daily,
    Notes,
    Archives,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct IndexedLine {
    pub(crate) line_no: usize,
    pub(crate) text: String,
    pub(crate) lowercase: String,
    pub(crate) tags: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct FileStamp {
    modified_secs: u64,
    modified_nanos: u32,
    size: u64,
    content_sha256: [u8; 32],
}

impl FileStamp {
    fn from_metadata(metadata: &fs::Metadata, content: &[u8]) -> Self {
        let modified = metadata
            .modified()
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        Self {
            modified_secs: modified.as_secs(),
            modified_nanos: modified.subsec_nanos(),
            size: content.len() as u64,
            content_sha256: Sha256::digest(content).into(),
        }
    }

    pub(crate) fn modified(self) -> SystemTime {
        UNIX_EPOCH + Duration::new(self.modified_secs, self.modified_nanos)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct IndexedDocument {
    pub(crate) group: DocumentGroup,
    stamp: FileStamp,
    pub(crate) lines: Vec<IndexedLine>,
    pub(crate) attachment_uris: Vec<String>,
    pub(crate) wiki_links: Vec<String>,
}

impl IndexedDocument {
    pub(crate) fn modified(&self) -> SystemTime {
        self.stamp.modified()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DocumentIndex {
    files: HashMap<PathBuf, IndexedDocument>,
}

/// Aggregated occurrences of one parsed reference target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReferenceEntry {
    pub(crate) count: usize,
    pub(crate) locations: Vec<PathBuf>,
}

/// Build occurrence counts and sorted distinct locations for parsed targets.
pub(crate) fn aggregate_references(
    files: &HashMap<PathBuf, Vec<String>>,
) -> HashMap<String, ReferenceEntry> {
    let mut references: HashMap<String, ReferenceEntry> = HashMap::new();
    let mut locations: HashMap<String, HashSet<PathBuf>> = HashMap::new();
    for (path, targets) in files {
        for target in targets {
            let entry = references.entry(target.clone()).or_default();
            entry.count += 1;
            locations
                .entry(target.clone())
                .or_default()
                .insert(path.clone());
        }
    }
    for (target, entry) in references.iter_mut() {
        let mut paths: Vec<PathBuf> = locations
            .remove(target)
            .unwrap_or_default()
            .into_iter()
            .collect();
        paths.sort();
        entry.locations = paths;
    }
    references
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheSnapshot {
    format_version: u32,
    files: Vec<CachedDocument>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedDocument {
    relative_path: PathBuf,
    document: IndexedDocument,
}

#[derive(Serialize)]
struct CacheSnapshotRef<'a> {
    format_version: u32,
    files: Vec<CachedDocumentRef<'a>>,
}

#[derive(Serialize)]
struct CachedDocumentRef<'a> {
    relative_path: &'a Path,
    document: &'a IndexedDocument,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CacheStats {
    pub(crate) reused: usize,
    pub(crate) parsed: usize,
}

impl DocumentIndex {
    #[cfg(test)]
    pub(crate) fn build(storage: &Storage) -> Self {
        Self::scan(storage, HashMap::new()).0
    }

    /// Build an index without skipping unreadable directories or documents.
    /// Destructive decisions use this variant so incomplete evidence fails
    /// closed instead of looking like an absence of references.
    pub(crate) fn build_checked(storage: &Storage) -> Result<Self> {
        let mut files = HashMap::new();
        for directory in [&storage.daily_dir, &storage.data_dir, &storage.archives_dir] {
            let entries = fs::read_dir(directory)
                .with_context(|| format!("reading indexed directory {}", directory.display()))?;
            for entry in entries {
                let path = entry
                    .with_context(|| format!("reading entry in {}", directory.display()))?
                    .path();
                if let Some(document) = index_file(storage, &path)? {
                    files.insert(path, document);
                }
            }
        }
        Ok(Self { files })
    }

    pub(crate) fn load_or_build(storage: &Storage) -> (Self, CacheStats) {
        let cached = read_cache(storage).unwrap_or_default();
        let (index, stats) = Self::scan(storage, cached);
        let _ = index.persist(storage);
        (index, stats)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&PathBuf, &IndexedDocument)> {
        self.files.iter()
    }

    pub(crate) fn refresh_paths(&mut self, storage: &Storage, paths: Vec<PathBuf>) -> bool {
        let mut changed = false;
        for path in paths.into_iter().collect::<HashSet<_>>() {
            changed |= self.apply_path_refresh(path.clone(), index_file(storage, &path));
        }
        if changed {
            let _ = self.persist(storage);
        }
        changed
    }

    fn apply_path_refresh(
        &mut self,
        path: PathBuf,
        outcome: Result<Option<IndexedDocument>>,
    ) -> bool {
        match outcome {
            Ok(Some(document)) => {
                let changed = self.files.get(&path).is_none_or(|previous| {
                    previous.group != document.group || previous.stamp != document.stamp
                });
                self.files.insert(path, document);
                changed
            }
            Ok(None) => self.files.remove(&path).is_some(),
            // Preserve the last valid snapshot across transient I/O failures;
            // a later watcher event will retry the read.
            Err(_) => false,
        }
    }

    fn scan(
        storage: &Storage,
        mut cached: HashMap<PathBuf, IndexedDocument>,
    ) -> (Self, CacheStats) {
        let mut files = HashMap::new();
        let mut stats = CacheStats::default();
        for directory in [&storage.daily_dir, &storage.data_dir, &storage.archives_dir] {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(group) = document_group(storage, &path) else {
                    continue;
                };
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
                    continue;
                }
                let Ok(content) = fs::read(&path) else {
                    continue;
                };
                let stamp = FileStamp::from_metadata(&metadata, &content);
                let relative = path.strip_prefix(&storage.root).unwrap_or(&path);
                let document = match cached.remove(relative) {
                    Some(document) if document.group == group && document.stamp == stamp => {
                        stats.reused += 1;
                        document
                    }
                    _ => {
                        let Ok(source) = String::from_utf8(content) else {
                            continue;
                        };
                        let document = parse_source(&source, group, stamp);
                        stats.parsed += 1;
                        document
                    }
                };
                files.insert(path, document);
            }
        }
        (Self { files }, stats)
    }

    fn persist(&self, storage: &Storage) -> Result<()> {
        let directory = storage.root.join(CACHE_DIR);
        fs::create_dir_all(&directory)
            .with_context(|| format!("creating index cache {}", directory.display()))?;
        let destination = directory.join(CACHE_FILE);
        let temporary = directory.join(format!(
            ".{CACHE_FILE}.tmp-{}-{:016x}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let result = (|| {
            let mut files = self
                .files
                .iter()
                .filter_map(|(path, document)| {
                    path.strip_prefix(&storage.root)
                        .ok()
                        .map(|relative_path| CachedDocumentRef {
                            relative_path,
                            document,
                        })
                })
                .collect::<Vec<_>>();
            files.sort_by(|left, right| left.relative_path.cmp(right.relative_path));
            let snapshot = CacheSnapshotRef {
                format_version: CACHE_FORMAT_VERSION,
                files,
            };
            let encoded =
                serde_json::to_vec(&snapshot).context("serializing document index cache")?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .with_context(|| format!("creating index cache {}", temporary.display()))?;
            file.write_all(&encoded)
                .with_context(|| format!("writing index cache {}", temporary.display()))?;
            file.sync_all()
                .with_context(|| format!("syncing index cache {}", temporary.display()))?;
            drop(file);
            crate::storage::replace_file_atomically(&temporary, &destination).with_context(
                || {
                    format!(
                        "publishing index cache {} to {}",
                        temporary.display(),
                        destination.display()
                    )
                },
            )?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

pub(crate) struct DocumentIndexer {
    commands: Sender<IndexCommand>,
    updates: Receiver<IndexSnapshot>,
    requested_revision: Cell<u64>,
}

pub(crate) struct IndexSnapshot {
    pub(crate) revision: u64,
    pub(crate) workspace: WorkspaceIndex,
    pub(crate) attachments: AttachmentReferenceIndex,
    pub(crate) wiki_links: WikiLinkIndex,
}

enum IndexCommand {
    PathsChanged { revision: u64, paths: Vec<PathBuf> },
    Stop,
}

impl DocumentIndexer {
    pub(crate) fn spawn(storage: Storage) -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (update_sender, update_receiver) = mpsc::channel();
        thread::spawn(move || index_worker(storage, command_receiver, update_sender));
        Self {
            commands: command_sender,
            updates: update_receiver,
            requested_revision: Cell::new(0),
        }
    }

    pub(crate) fn paths_changed(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let revision = self.requested_revision.get().saturating_add(1);
        if self
            .commands
            .send(IndexCommand::PathsChanged { revision, paths })
            .is_ok()
        {
            self.requested_revision.set(revision);
        }
    }

    pub(crate) fn try_latest_update(&self) -> Option<IndexSnapshot> {
        let requested = self.requested_revision.get();
        self.updates
            .try_iter()
            .filter(|snapshot| snapshot.revision >= requested)
            .last()
    }
}

impl Drop for DocumentIndexer {
    fn drop(&mut self) {
        let _ = self.commands.send(IndexCommand::Stop);
    }
}

fn index_worker(
    storage: Storage,
    commands: Receiver<IndexCommand>,
    updates: Sender<IndexSnapshot>,
) {
    let (mut documents, _) = DocumentIndex::load_or_build(&storage);
    let mut revision = 0;
    if !apply_pending_commands(&storage, &commands, &mut documents, &mut revision) {
        return;
    }
    if !publish_snapshot(&updates, revision, &documents) {
        return;
    }

    while let Ok(command) = commands.recv() {
        match command {
            IndexCommand::PathsChanged {
                revision: changed_revision,
                paths,
            } => {
                revision = revision.max(changed_revision);
                documents.refresh_paths(&storage, paths);
                if !apply_pending_commands(&storage, &commands, &mut documents, &mut revision) {
                    return;
                }
                if !publish_snapshot(&updates, revision, &documents) {
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
    documents: &mut DocumentIndex,
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
    documents.refresh_paths(storage, paths);
    true
}

fn publish_snapshot(
    updates: &Sender<IndexSnapshot>,
    revision: u64,
    documents: &DocumentIndex,
) -> bool {
    updates
        .send(IndexSnapshot {
            revision,
            workspace: WorkspaceIndex::from_documents(documents),
            attachments: AttachmentReferenceIndex::from_documents(documents),
            wiki_links: WikiLinkIndex::from_documents(documents),
        })
        .is_ok()
}

fn read_cache(storage: &Storage) -> Result<HashMap<PathBuf, IndexedDocument>> {
    let path = storage.root.join(CACHE_DIR).join(CACHE_FILE);
    let file =
        File::open(&path).with_context(|| format!("opening index cache {}", path.display()))?;
    let snapshot: CacheSnapshot = serde_json::from_reader(file)
        .with_context(|| format!("parsing index cache {}", path.display()))?;
    if snapshot.format_version != CACHE_FORMAT_VERSION {
        return Ok(HashMap::new());
    }
    Ok(snapshot
        .files
        .into_iter()
        .map(|cached| (cached.relative_path, cached.document))
        .collect())
}

pub(crate) fn index_file(storage: &Storage, path: &Path) -> Result<Option<IndexedDocument>> {
    let Some(group) = document_group(storage, path) else {
        return Ok(None);
    };
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("checking indexed file {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Ok(None);
    }
    let content =
        fs::read(path).with_context(|| format!("reading indexed file {}", path.display()))?;
    let stamp = FileStamp::from_metadata(&metadata, &content);
    let source = String::from_utf8(content)
        .with_context(|| format!("indexed file is not UTF-8: {}", path.display()))?;
    Ok(Some(parse_source(&source, group, stamp)))
}

fn parse_source(source: &str, group: DocumentGroup, stamp: FileStamp) -> IndexedDocument {
    let mut tags_by_line: HashMap<usize, Vec<String>> = HashMap::new();
    let mut attachment_uris = Vec::new();
    let mut wiki_links = Vec::new();
    if let Ok(document) = mbdown::parse(source) {
        collect_document_tags(document.nodes(), source, &mut tags_by_line);
        attachment_uris = collect_attachment_uris(document.nodes());
        wiki_links = collect_wiki_links(document.nodes());
    }
    let lines = source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let text = line.trim();
            (!text.is_empty()).then(|| IndexedLine {
                line_no: index + 1,
                text: text.to_string(),
                lowercase: text.to_lowercase(),
                tags: tags_by_line.remove(&(index + 1)).unwrap_or_default(),
            })
        })
        .collect();
    IndexedDocument {
        group,
        stamp,
        lines,
        attachment_uris,
        wiki_links,
    }
}

pub(crate) fn document_group(storage: &Storage, path: &Path) -> Option<DocumentGroup> {
    let extension = path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("md") && !extension.eq_ignore_ascii_case("mb") {
        return None;
    }
    match path.parent() {
        Some(parent) if parent == storage.daily_dir => Some(DocumentGroup::Daily),
        Some(parent) if parent == storage.data_dir => Some(DocumentGroup::Notes),
        Some(parent) if parent == storage.archives_dir => Some(DocumentGroup::Archives),
        _ => None,
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

    #[test]
    fn transient_refresh_failure_preserves_last_valid_document() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let path = storage.data_dir.join("Note.md");
        fs::write(&path, "stable #tag\n").unwrap();
        let mut index = DocumentIndex::build(&storage);
        let original = index.files.get(&path).unwrap().clone();

        let changed =
            index.apply_path_refresh(path.clone(), Err(anyhow::anyhow!("temporary read failure")));

        assert!(!changed);
        assert_eq!(index.files.get(&path).unwrap(), &original);
    }

    fn uri() -> String {
        "nole://attachment/00000001-0000-4000-8000-000000000000".to_string()
    }

    #[test]
    fn cache_reuses_unchanged_documents_and_reparses_changes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let note = storage.data_dir.join("Cached.md");
        fs::write(
            &note,
            format!("cached needle #one [[linked]] [file]({})\n", uri()),
        )
        .unwrap();

        let (first, first_stats) = DocumentIndex::load_or_build(&storage);
        assert_eq!(
            first_stats,
            CacheStats {
                reused: 0,
                parsed: 1
            }
        );
        assert_eq!(first.iter().count(), 1);
        let cache_path = storage.root.join(CACHE_DIR).join(CACHE_FILE);
        assert!(cache_path.is_file());

        let (second, second_stats) = DocumentIndex::load_or_build(&storage);
        assert_eq!(
            second_stats,
            CacheStats {
                reused: 1,
                parsed: 0
            }
        );
        assert_eq!(
            WorkspaceIndex::from_documents(&second)
                .search("needle")
                .len(),
            1
        );
        assert_eq!(
            AttachmentReferenceIndex::from_documents(&second).reference_count(&uri()),
            1
        );
        assert_eq!(
            WikiLinkIndex::from_documents(&second).reference_count("linked"),
            1
        );

        fs::write(&note, "changed content with a different size #two\n").unwrap();
        let (third, third_stats) = DocumentIndex::load_or_build(&storage);
        assert_eq!(
            third_stats,
            CacheStats {
                reused: 0,
                parsed: 1
            }
        );
        let workspace = WorkspaceIndex::from_documents(&third);
        assert!(workspace.search("needle").is_empty());
        assert_eq!(workspace.tags()[0].name, "two");
        assert_eq!(
            AttachmentReferenceIndex::from_documents(&third).reference_count(&uri()),
            0
        );
        assert_eq!(
            WikiLinkIndex::from_documents(&third).reference_count("linked"),
            0
        );
    }

    #[test]
    fn corrupt_or_version_mismatched_cache_rebuilds_and_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        fs::write(storage.data_dir.join("Note.md"), "content #tag\n").unwrap();
        let _ = DocumentIndex::load_or_build(&storage);
        let cache_path = storage.root.join(CACHE_DIR).join(CACHE_FILE);

        fs::write(&cache_path, b"not json").unwrap();
        let (_, corrupt_stats) = DocumentIndex::load_or_build(&storage);
        assert_eq!(
            corrupt_stats,
            CacheStats {
                reused: 0,
                parsed: 1
            }
        );

        let mut cache: serde_json::Value =
            serde_json::from_slice(&fs::read(&cache_path).unwrap()).unwrap();
        cache["format_version"] = serde_json::json!(CACHE_FORMAT_VERSION + 1);
        fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();
        let (_, version_stats) = DocumentIndex::load_or_build(&storage);
        assert_eq!(
            version_stats,
            CacheStats {
                reused: 0,
                parsed: 1
            }
        );
        let repaired: serde_json::Value =
            serde_json::from_slice(&fs::read(cache_path).unwrap()).unwrap();
        assert_eq!(repaired["format_version"], CACHE_FORMAT_VERSION);
    }

    #[test]
    fn same_size_and_timestamp_content_changes_rebuild_the_document() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let note = storage.data_dir.join("Stable.md");
        fs::write(&note, "a\n").unwrap();
        let initial_modified = fs::metadata(&note).unwrap().modified().unwrap();
        let (_, initial_stats) = DocumentIndex::load_or_build(&storage);
        assert_eq!(initial_stats.parsed, 1);

        fs::write(&note, "b\n").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&note)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(initial_modified))
            .unwrap();

        let (rebuilt, stats) = DocumentIndex::load_or_build(&storage);
        assert_eq!(stats.reused, 0);
        assert_eq!(stats.parsed, 1);
        let workspace = WorkspaceIndex::from_documents(&rebuilt);
        assert!(workspace.search("a").is_empty());
        assert_eq!(workspace.search("b").len(), 1);
    }

    #[test]
    fn shared_worker_publishes_workspace_and_attachment_updates_together() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let note = storage.data_dir.join("Live.md");
        fs::write(
            &note,
            format!("first needle #one [[linked]] [file]({})\n", uri()),
        )
        .unwrap();
        let indexer = DocumentIndexer::spawn(storage.clone());

        let initial = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(initial.revision, 0);
        assert_eq!(initial.workspace.search("needle").len(), 1);
        assert_eq!(initial.attachments.reference_count(&uri()), 1);
        assert_eq!(initial.wiki_links.reference_count("linked"), 1);
        assert_eq!(
            initial.wiki_links.backlinks(&note),
            Vec::<std::path::PathBuf>::new(),
            "self backlinks are excluded"
        );

        fs::write(&note, "second value #two\n").unwrap();
        indexer.paths_changed(vec![note.clone()]);
        let modified = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(modified.revision, 1);
        assert!(modified.workspace.search("needle").is_empty());
        assert_eq!(modified.workspace.tags()[0].name, "two");
        assert_eq!(modified.attachments.reference_count(&uri()), 0);
        assert_eq!(modified.wiki_links.reference_count("linked"), 0);

        fs::remove_file(&note).unwrap();
        indexer.paths_changed(vec![note]);
        let deleted = indexer
            .updates
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(deleted.revision, 2);
        assert!(deleted.workspace.tags().is_empty());
        assert_eq!(deleted.attachments.reference_count(&uri()), 0);
        assert!(!deleted.wiki_links.is_referenced("linked"));
    }
}
