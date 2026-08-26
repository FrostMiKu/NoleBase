//! Shared, authoritative attachment-usage state and the deletion service.
//!
//! One [`AttachmentUsageHandle`] lives for the whole process. The app publishes
//! reference-index snapshots as they arrive from the background indexer (each
//! carrying a monotonic revision), and every destructive delete — from the UI
//! browser or the Agent's `delete_attachment` tool — funnels through
//! [`AttachmentUsageHandle::trash`], the single deletion boundary around
//! `AttachmentStore::remove`.
//!
//! `delete_attachment` enforces the review's deletion rules:
//!
//! - requires the index's initial authoritative snapshot
//!   ([`TrashError::NotReady`]);
//! - requires the caller's `expected_revision` to match the published
//!   snapshot revision, so the decision reflects the state the user or agent
//!   reviewed ([`TrashError::Stale`]);
//! - performs a final, synchronous, authoritative scan of the managed notes
//!   (daily/, data/, archives/) immediately before trashing, reporting the
//!   distinct referencing note paths ([`TrashError::Referenced`]);
//! - then calls the store's atomic one-directory trash (`remove`).
//!
//! The synchronous scan closes the window between a file event and its index
//! refresh: a managed note reference always blocks trashing that attachment.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::attachment::{AttachmentId, AttachmentStore, AttachmentUri};
use crate::attachment_index::AttachmentReferenceIndex;
use crate::storage::Storage;

/// An immutable view of attachment usage at one point in time.
#[derive(Clone, Debug)]
pub struct AttachmentUsageSnapshot {
    /// Monotonic revision of the reference index this snapshot reflects.
    pub revision: u64,
    /// True once the initial authoritative index build has been published.
    pub ready: bool,
    /// Derived references from managed notes, keyed by canonical URI.
    pub references: AttachmentReferenceIndex,
}

/// Why a trash request was refused before touching the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrashError {
    /// The shared usage index is still building its initial snapshot.
    NotReady,
    /// The caller acted on an outdated snapshot (`expected_revision` no longer
    /// matches the current one); re-read the snapshot and retry.
    Stale {
        /// The revision the index has advanced to.
        current_revision: u64,
    },
    /// The final authoritative scan found managed notes still referencing the
    /// attachment. `locations` are the distinct referencing note paths.
    Referenced { locations: Vec<PathBuf> },
    /// Underlying store failure while moving the attachment to trash.
    Store(String),
}

impl fmt::Display for TrashError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrashError::NotReady => {
                write!(
                    formatter,
                    "attachment usage index is building its initial snapshot"
                )
            }
            TrashError::Stale { current_revision } => write!(
                formatter,
                "attachment references changed (index now at revision \
                 {current_revision}); review the current state and retry"
            ),
            TrashError::Referenced { locations } => write!(
                formatter,
                "attachment is still referenced by {}: {}",
                locations.len(),
                locations
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            TrashError::Store(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for TrashError {}

/// Outcome of a successful trash attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashResult {
    /// The attachment directory was atomically moved to trash.
    Trashed,
    /// No such attachment existed in the store.
    NotFound,
}

/// Shared, authoritative attachment-usage state with readiness and revisions.
///
/// Cheap to clone: every clone observes the same state, so the app and the
/// agent runtime can hold their own handles over one underlying snapshot.
#[derive(Clone, Debug, Default)]
pub struct AttachmentUsageHandle {
    inner: Arc<Mutex<UsageInner>>,
}

#[derive(Debug, Default)]
struct UsageInner {
    ready: bool,
    revision: u64,
    references: AttachmentReferenceIndex,
}

impl AttachmentUsageHandle {
    /// A fresh handle awaiting its initial snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a reference-index snapshot produced by the background indexer.
    /// The first publish marks the handle ready; later publishes advance the
    /// revision. Older snapshots are discarded so consumers always observe
    /// monotonic state even when publishers race.
    pub fn publish_snapshot(&self, revision: u64, references: AttachmentReferenceIndex) {
        let mut inner = self.inner.lock().unwrap();
        if inner.ready && revision <= inner.revision {
            return;
        }
        inner.ready = true;
        inner.revision = revision;
        inner.references = references;
    }

    /// The current snapshot (readiness, revision, and references).
    pub fn snapshot(&self) -> AttachmentUsageSnapshot {
        let inner = self.inner.lock().unwrap();
        AttachmentUsageSnapshot {
            revision: inner.revision,
            ready: inner.ready,
            references: inner.references.clone(),
        }
    }

    /// True once at least one index snapshot has been published.
    pub fn is_ready(&self) -> bool {
        self.inner.lock().unwrap().ready
    }

    /// Trash `id` after enforcing the deletion rules, using the store's atomic
    /// one-directory trash. This is the single entry point for both the UI and
    /// the Agent tools; callers use this operation as the deletion boundary
    /// instead of calling `AttachmentStore::remove` directly.
    ///
    /// `expected_revision` is the snapshot revision the caller showed the user
    /// (UI) or based its decision on (Agent) — typically captured when the
    /// confirmation flow started.
    pub fn trash(
        &self,
        store: &AttachmentStore,
        storage: &Storage,
        id: AttachmentId,
        expected_revision: u64,
    ) -> Result<TrashResult, TrashError> {
        {
            let inner = self.inner.lock().unwrap();
            if !inner.ready {
                return Err(TrashError::NotReady);
            }
            if inner.revision != expected_revision {
                return Err(TrashError::Stale {
                    current_revision: inner.revision,
                });
            }
        }

        // Final authoritative check: synchronously re-derive references from
        // the managed notes on disk, so deletion proceeds only when the scan
        // finds zero managed-note references.
        let uri = AttachmentUri::from_id(id).to_string();
        let authoritative = AttachmentReferenceIndex::build(storage);
        let locations = authoritative.locations(&uri);
        if !locations.is_empty() {
            // Publish the fresh scan so UI/agents observe the real state.
            self.publish_authoritative(authoritative);
            return Err(TrashError::Referenced { locations });
        }
        self.publish_authoritative(authoritative);

        match store.remove(id) {
            Ok(true) => Ok(TrashResult::Trashed),
            Ok(false) => Ok(TrashResult::NotFound),
            Err(error) => Err(TrashError::Store(format!("{error:#}"))),
        }
    }

    /// Replace the snapshot with a freshly scanned authoritative index, keeping
    /// the revision (the scan reflects the same event stream; the indexer's
    /// next publish advances it).
    fn publish_authoritative(&self, references: AttachmentReferenceIndex) {
        let mut inner = self.inner.lock().unwrap();
        inner.ready = true;
        inner.references = references;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn storage_with(directory: &tempfile::TempDir) -> Storage {
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage
    }

    #[test]
    fn refuses_before_the_initial_snapshot_is_published() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let store = AttachmentStore::new(storage.attachments_dir.clone());
        let metadata = store.import_bytes(b"payload", Some("report.pdf")).unwrap();
        let handle = AttachmentUsageHandle::new();
        assert!(!handle.is_ready());

        let outcome = handle.trash(&store, &storage, metadata.id, 0);
        assert_eq!(outcome, Err(TrashError::NotReady));
        assert!(store.lookup(metadata.id).unwrap().is_some());
    }

    #[test]
    fn refuses_when_the_expected_revision_is_stale() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let store = AttachmentStore::new(storage.attachments_dir.clone());
        let metadata = store.import_bytes(b"payload", Some("report.pdf")).unwrap();
        let handle = AttachmentUsageHandle::new();
        handle.publish_snapshot(0, AttachmentReferenceIndex::build(&storage));
        handle.publish_snapshot(1, AttachmentReferenceIndex::build(&storage));

        let outcome = handle.trash(&store, &storage, metadata.id, 0);
        assert_eq!(
            outcome,
            Err(TrashError::Stale {
                current_revision: 1
            })
        );
        assert!(store.lookup(metadata.id).unwrap().is_some());
    }

    #[test]
    fn ignores_snapshots_that_arrive_out_of_order() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let uri = "nole://attachment/00000001-0000-4000-8000-000000000000";
        let note = storage.data_dir.join("Reference.md");
        fs::write(&note, format!("[attachment]({uri})\n")).unwrap();
        let handle = AttachmentUsageHandle::new();
        let references = AttachmentReferenceIndex::build(&storage);
        assert_eq!(references.locations(uri), vec![note]);
        handle.publish_snapshot(2, references);
        handle.publish_snapshot(1, AttachmentReferenceIndex::default());
        handle.publish_snapshot(2, AttachmentReferenceIndex::default());

        let snapshot = handle.snapshot();
        assert!(snapshot.ready);
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.references.locations(uri).len(), 1);
    }

    #[test]
    fn final_scan_refuses_an_attachment_referenced_after_the_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let store = AttachmentStore::new(storage.attachments_dir.clone());
        let metadata = store.import_bytes(b"payload", Some("report.pdf")).unwrap();
        let handle = AttachmentUsageHandle::new();
        handle.publish_snapshot(0, AttachmentReferenceIndex::build(&storage));

        // A note starts referencing the attachment *after* the snapshot: the
        // async index is stale, but the final synchronous scan must catch it.
        let note = storage.data_dir.join("Note.md");
        fs::write(
            &note,
            format!("[report]({})\n", AttachmentUri::from_id(metadata.id)),
        )
        .unwrap();

        let outcome = handle.trash(&store, &storage, metadata.id, 0);
        match outcome {
            Err(TrashError::Referenced { locations }) => {
                assert_eq!(locations, vec![note]);
            }
            other => panic!("expected Referenced, got {other:?}"),
        }
        assert!(store.lookup(metadata.id).unwrap().is_some());
    }

    #[test]
    fn unreferenced_attachment_trashes_and_reports_not_found_on_repeat() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let store = AttachmentStore::new(storage.attachments_dir.clone());
        let metadata = store.import_bytes(b"payload", Some("report.pdf")).unwrap();
        let handle = AttachmentUsageHandle::new();
        handle.publish_snapshot(0, AttachmentReferenceIndex::build(&storage));

        assert_eq!(
            handle.trash(&store, &storage, metadata.id, 0),
            Ok(TrashResult::Trashed)
        );
        assert!(store.lookup(metadata.id).unwrap().is_none());
        // The published snapshot now reflects the authoritative scan.
        assert!(handle.is_ready());

        assert_eq!(
            handle.trash(&store, &storage, metadata.id, 0),
            Ok(TrashResult::NotFound)
        );
    }
}
