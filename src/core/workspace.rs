//! The workspace application service: one Nole workspace's data, indexes,
//! and attachments, owned as a single lifecycle object that any client
//! (TUI today, other clients later) can depend on instead of assembling
//! storage and index handles itself.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::core::attachment::{AttachmentId, AttachmentMetadata, AttachmentStore};
use crate::core::attachment_usage::{AttachmentUsageHandle, TrashError, TrashResult};
use crate::core::document_index::DocumentIndexer;
use crate::core::model::{DailyNote, NoteFile, SearchHit, TodoItem};
use crate::core::storage::Storage;
use crate::core::wiki_link_index::WikiLinkIndexHandle;
use crate::core::workspace_index::{TagDocument, TagSummary, WorkspaceIndex, WorkspaceIndexHandle};

/// A Nole workspace: storage plus the shared document-derived indexes and
/// the attachment store/usage services built on it.
///
/// The index handles are shared with the agent runtime and the background
/// [`DocumentIndexer`]; publishing flows through this service so every
/// client observes the same snapshots. Clients that need shared ownership
/// (the TUI app and the agent service) keep an `Arc<Workspace>`.
pub struct Workspace {
    storage: Storage,
    /// The background indexer is `Send` but not `Sync` (channel receiver
    /// plus a revision `Cell`); the mutex keeps the whole workspace sharable
    /// across threads via `Arc<Workspace>`.
    document_indexer: std::sync::Mutex<DocumentIndexer>,
    workspace_index: WorkspaceIndexHandle,
    wiki_links: WikiLinkIndexHandle,
    attachments: AttachmentStore,
    attachment_usage: AttachmentUsageHandle,
}

impl Workspace {
    /// Open the workspace rooted at `storage`, spawning the background
    /// document indexer that publishes the initial index snapshot.
    pub fn open(storage: Storage) -> Result<Self> {
        let document_indexer = DocumentIndexer::spawn(storage.clone());
        Ok(Self {
            attachments: AttachmentStore::new(storage.attachments_dir.clone()),
            attachment_usage: AttachmentUsageHandle::new(),
            workspace_index: WorkspaceIndexHandle::default(),
            wiki_links: WikiLinkIndexHandle::default(),
            storage,
            document_indexer: std::sync::Mutex::new(document_indexer),
        })
    }

    /// Transitional accessor for code that still talks to storage directly;
    /// new code should prefer the workspace-level operations below.
    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    pub fn workspace_index(&self) -> &WorkspaceIndexHandle {
        &self.workspace_index
    }

    pub fn wiki_links(&self) -> &WikiLinkIndexHandle {
        &self.wiki_links
    }

    pub fn attachments(&self) -> &AttachmentStore {
        &self.attachments
    }

    pub fn attachment_usage(&self) -> &AttachmentUsageHandle {
        &self.attachment_usage
    }

    pub fn daily_notes(&self) -> Result<Vec<DailyNote>> {
        self.storage.load_daily_notes()
    }

    /// Managed note files, notes first (recent-first) then archives.
    pub fn note_files(&self) -> Result<Vec<NoteFile>> {
        let mut files = self.storage.list_note_files()?;
        files.extend(self.storage.list_archived_note_files()?);
        Ok(files)
    }

    pub fn todos(&self) -> Result<Vec<TodoItem>> {
        self.storage.load_todo_tasks()
    }

    /// Workspace-wide search. `None` while the document index has not
    /// published its first snapshot yet.
    pub fn search(&self, query: &str) -> Option<Vec<SearchHit>> {
        self.workspace_index.with_index(|index| index.search(query))
    }

    /// All tag summaries. `None` while the index is still building.
    pub fn tags(&self) -> Option<Vec<TagSummary>> {
        self.workspace_index.with_index(WorkspaceIndex::tags)
    }

    /// Distinct managed documents containing `tag`, oldest-first.
    /// `None` while the index is still building.
    pub fn tag_documents(&self, tag: &str) -> Option<Vec<TagDocument>> {
        self.workspace_index
            .with_index(|index| index.tag_documents(tag))
    }

    /// Distinct managed notes linking to `path` through wiki links.
    /// `None` while the index is still building.
    pub fn backlinks(&self, path: &Path) -> Option<Vec<PathBuf>> {
        self.wiki_links.with_index(|index| index.backlinks(path))
    }

    /// Feed changed managed-note paths into the background indexer; the
    /// refreshed snapshots arrive through [`Workspace::poll_index_update`].
    pub fn refresh_documents(&self, paths: Vec<PathBuf>) {
        if let Ok(indexer) = self.document_indexer.lock() {
            indexer.paths_changed(paths);
        }
    }

    /// Refresh the shared workspace index in place for `paths`. Used after
    /// synchronous rewrites (tag renames) so tag-dependent queries are
    /// correct before the background indexer reconverges.
    pub fn refresh_index_paths(&self, paths: Vec<PathBuf>) {
        self.workspace_index.refresh_paths(&self.storage, paths);
    }

    /// Apply the latest document-index snapshot, if a new one has settled.
    /// Returns whether the shared indexes were republished.
    pub fn poll_index_update(&self) -> bool {
        let Ok(indexer) = self.document_indexer.lock() else {
            return false;
        };
        let Some(snapshot) = indexer.try_latest_update() else {
            return false;
        };
        self.workspace_index.replace(snapshot.workspace);
        self.attachment_usage
            .publish_snapshot(snapshot.revision, snapshot.attachments);
        self.wiki_links.replace(snapshot.wiki_links);
        true
    }

    /// Attachment metadata lookup for display purposes.
    pub fn attachment_metadata(&self, id: AttachmentId) -> Result<AttachmentMetadata> {
        self.attachments.metadata(id)
    }

    /// The application-managed content path for an attachment; external
    /// saves update the attachment in place.
    pub fn open_attachment(&self, id: AttachmentId) -> Result<PathBuf> {
        self.attachments.open(id)
    }

    /// Move an attachment to trash through the shared usage service: the
    /// usage revision is validated and an authoritative reference scan
    /// always runs before the atomic store trash.
    pub fn trash_attachment(
        &self,
        id: AttachmentId,
        expected_revision: u64,
    ) -> std::result::Result<TrashResult, TrashError> {
        self.attachment_usage
            .trash(&self.attachments, &self.storage, id, expected_revision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace() -> (Workspace, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        (Workspace::open(storage).unwrap(), directory)
    }

    fn wait_for_index(workspace: &Workspace) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if workspace.poll_index_update() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn opens_on_a_fresh_directory_and_reads_domain_data() {
        let (workspace, directory) = workspace();
        let storage = Storage::new(directory.path()).unwrap();
        storage.append_to_today("- a daily line").unwrap();
        storage.append_to_today("- [ ] a task").unwrap();
        fs::write(storage.data_dir.join("note.md"), "#note #hello").unwrap();

        let daily = workspace.daily_notes().unwrap();
        assert_eq!(daily.len(), 1);
        let todos = workspace.todos().unwrap();
        assert_eq!(todos.len(), 1);
        let files = workspace.note_files().unwrap();
        assert!(files.iter().any(|file| file.path.ends_with("note.md")));
    }

    #[test]
    fn note_files_list_notes_before_archives_with_flags() {
        let (workspace, directory) = workspace();
        let storage = Storage::new(directory.path()).unwrap();
        fs::write(storage.data_dir.join("note.md"), "note").unwrap();
        fs::write(storage.archives_dir.join("old.md"), "old").unwrap();

        let files = workspace.note_files().unwrap();
        let note = files.iter().find(|file| !file.archived).unwrap();
        let archived = files.iter().find(|file| file.archived).unwrap();
        assert!(note.path.ends_with("note.md"));
        assert!(archived.path.ends_with("old.md"));
        assert!(
            files.iter().position(|f| f.path == note.path).unwrap()
                < files.iter().position(|f| f.path == archived.path).unwrap()
        );
    }

    #[test]
    fn index_queries_settle_after_refresh() {
        let (workspace, directory) = workspace();
        let storage = Storage::new(directory.path()).unwrap();
        let note = storage.data_dir.join("tagged.md");
        fs::write(&note, "#intro [[other]] note body").unwrap();
        fs::write(storage.data_dir.join("other.md"), "target note").unwrap();
        let other = storage.data_dir.join("other.md");

        assert!(
            wait_for_index(&workspace),
            "initial index snapshot must settle"
        );
        workspace.refresh_documents(vec![note.clone(), other.clone()]);
        assert!(
            wait_for_index(&workspace),
            "refreshed index snapshot must settle"
        );

        let hits = workspace.search("body").unwrap();
        assert!(hits.iter().any(|hit| match hit {
            SearchHit::FileLine { path, .. } => path == &note,
            SearchHit::DocumentLine { .. } => false,
        }));
        let tags = workspace.tags().unwrap();
        assert!(tags.iter().any(|tag| tag.name == "intro"));
        let documents = workspace.tag_documents("intro").unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].path, note);
        let backlinks = workspace.backlinks(&other).unwrap();
        assert_eq!(backlinks, vec![note]);
    }
}
