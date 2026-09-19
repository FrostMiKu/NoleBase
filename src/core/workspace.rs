//! The workspace application service: one Nole workspace's data, indexes,
//! and attachments, owned as a single lifecycle object that any client
//! (TUI today, other clients later) can depend on instead of assembling
//! storage and index handles itself.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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

    /// Flip the completion state of the indexed daily task (same ordering as
    /// [`Workspace::todos`]). Returns whether a task was toggled.
    pub fn toggle_todo_task(&self, index: usize) -> Result<bool> {
        self.storage.toggle_todo_task(index)
    }

    /// Append content to today's daily note, creating it on first send —
    /// the web quick-input primitive mirroring the TUI's daily send.
    pub fn append_to_today(&self, body: &str) -> Result<DailyNote> {
        self.storage.append_to_today(body)
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

    /// Read the source of a managed document addressed by its
    /// workspace-relative path (`data/note.md`, `daily/2026-09-19.md`,
    /// `archives/old.mb`). The address is validated lexically and the target
    /// is validated in full (regular non-symlink `.md`/`.mb` file inside the
    /// Nole root) before reading.
    pub fn read_document(&self, relative: &str) -> Result<String> {
        let target = self.document_path(relative)?;
        self.storage.read_document_file(&target)
    }

    /// Write the source of a managed document addressed by its
    /// workspace-relative path. The same addressing rules as
    /// [`Workspace::read_document`] apply; existing documents are replaced
    /// atomically through a same-directory temporary file, so the filesystem
    /// watcher and the index pipeline observe one consistent change.
    ///
    /// There is no revision or optimistic-concurrency token on document
    /// storage; a write unconditionally replaces the current content.
    pub fn write_document(&self, relative: &str, source: &str) -> Result<()> {
        let target = self.document_path(relative)?;
        let directory = target
            .parent()
            .context("managed document path has no parent directory")?;
        if let Ok(metadata) = std::fs::symlink_metadata(&target) {
            if metadata.file_type().is_symlink() {
                anyhow::bail!("managed document must not be a symlink: {relative}");
            }
        }
        let temporary = directory.join(format!(
            ".{}.{}.web.tmp",
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("document"),
            fastrand::u64(..)
        ));
        std::fs::write(&temporary, source)
            .with_context(|| format!("staging document write for {relative}"))?;
        if let Err(error) = crate::core::storage::replace_file_atomically(&temporary, &target) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error).with_context(|| format!("publishing document {relative}"));
        }
        Ok(())
    }

    /// Validate a workspace-relative document address and resolve it to an
    /// absolute path without checking existence. Accepted addresses are
    /// exactly one managed directory component (`data`, `daily`, or
    /// `archives`) followed by one `.md`/`.mb` file name. Absolute paths,
    /// parent-directory components, backslashes, and deeper or shallower
    /// paths are rejected, so the resolved path can never escape the managed
    /// note directories. Clients use this to translate a wire address into a
    /// path for index queries such as backlinks.
    pub fn document_path(&self, relative: &str) -> Result<PathBuf> {
        if relative.is_empty() {
            anyhow::bail!("document path must not be empty");
        }
        if relative.contains('\\') {
            anyhow::bail!("document path must use '/' separators");
        }
        let path = std::path::Path::new(relative);
        if path.is_absolute() || path.has_root() {
            anyhow::bail!("document path must be workspace-relative");
        }
        let mut components = path.components();
        let (directory, name) = match (components.next(), components.next(), components.next()) {
            (Some(first), Some(second), None) => (first, second),
            _ => anyhow::bail!(
                "document path must be `<data|daily|archives>/<name>.<md|mb>`: {relative}"
            ),
        };
        let directory = match directory.as_os_str().to_str() {
            Some("data") => self.storage.data_dir.clone(),
            Some("daily") => self.storage.daily_dir.clone(),
            Some("archives") => self.storage.archives_dir.clone(),
            _ => anyhow::bail!(
                "document path must be `<data|daily|archives>/<name>.<md|mb>`: {relative}"
            ),
        };
        let name = name
            .as_os_str()
            .to_str()
            .context("document name is not UTF-8")?;
        if name.is_empty() || name == "." || name == ".." {
            anyhow::bail!("document name must be a file name: {relative}");
        }
        let candidate = directory.join(name);
        if !crate::core::storage::is_note_path(&candidate) {
            anyhow::bail!("document must be a .md or .mb file: {relative}");
        }
        Ok(candidate)
    }

    /// The workspace-relative, `/`-separated form of `path` when it lives
    /// inside this workspace, for clients that must not observe absolute
    /// local paths. Falls back to `None` for outside paths.
    pub fn relative_path(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.storage.root).ok().or_else(|| {
            let canonical_root = std::fs::canonicalize(&self.storage.root).ok()?;
            path.strip_prefix(canonical_root).ok()
        })?;
        if relative.as_os_str().is_empty() {
            return None;
        }
        let mut joined = String::new();
        for component in relative.components() {
            if !joined.is_empty() {
                joined.push('/');
            }
            joined.push_str(&component.as_os_str().to_string_lossy());
        }
        Some(joined)
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
    fn toggle_todo_task_flips_the_indexed_daily_task() {
        let (workspace, directory) = workspace();
        let storage = Storage::new(directory.path()).unwrap();
        storage.append_to_today("- [ ] first task").unwrap();
        storage.append_to_today("- [x] second task").unwrap();

        assert!(workspace.toggle_todo_task(0).unwrap());
        let todos = workspace.todos().unwrap();
        assert!(todos[0].checked);
        assert!(todos[1].checked, "untouched task keeps its state");

        // Toggling again restores the open state.
        assert!(workspace.toggle_todo_task(0).unwrap());
        assert!(!workspace.todos().unwrap()[0].checked);

        // Out-of-range indices report "nothing toggled", not an error.
        assert!(!workspace.toggle_todo_task(9).unwrap());
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

    #[test]
    fn read_document_resolves_relative_addresses_and_rejects_escapes() {
        let (workspace, directory) = workspace();
        let storage = Storage::new(directory.path()).unwrap();
        fs::write(storage.data_dir.join("note.md"), "# managed").unwrap();
        fs::write(storage.daily_dir.join("2026-09-19.md"), "day").unwrap();
        let secret = directory.path().join("secret.txt");
        fs::write(&secret, "outside").unwrap();

        assert_eq!(
            workspace.read_document("data/note.md").unwrap(),
            "# managed"
        );
        assert_eq!(
            workspace.read_document("daily/2026-09-19.md").unwrap(),
            "day"
        );

        for rejected in [
            "",
            "/etc/passwd",
            "data/note.md/extra",
            "data",
            "data/../data/note.md",
            "../secret.txt",
            "data/../../secret.txt",
            "config/settings.toml",
            "attachments/0000/content.png",
            "data\\note.md",
            "data/note.txt",
            "MEMORY.md",
            "data/.hidden",
        ] {
            assert!(
                workspace.read_document(rejected).is_err(),
                "address must be rejected: {rejected}"
            );
        }
    }

    #[test]
    fn write_document_creates_and_replaces_atomically() {
        let (workspace, directory) = workspace();
        let storage = Storage::new(directory.path()).unwrap();

        workspace.write_document("data/new.md", "# first").unwrap();
        assert_eq!(
            fs::read_to_string(storage.data_dir.join("new.md")).unwrap(),
            "# first"
        );

        workspace.write_document("data/new.md", "# second").unwrap();
        assert_eq!(
            fs::read_to_string(storage.data_dir.join("new.md")).unwrap(),
            "# second"
        );
        workspace.write_document("daily/2026-09-19.md", "").unwrap();
        assert_eq!(
            fs::read_to_string(storage.daily_dir.join("2026-09-19.md")).unwrap(),
            ""
        );

        // No staging files are left behind in managed directories.
        for directory in [&storage.data_dir, &storage.daily_dir, &storage.archives_dir] {
            let strays: Vec<_> = fs::read_dir(directory)
                .unwrap()
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
                .collect();
            assert!(strays.is_empty(), "stray staging files: {strays:?}");
        }
    }

    #[test]
    fn write_document_rejects_traversal_and_special_locations() {
        let (workspace, directory) = workspace();
        let secret = directory.path().join("secret.txt");
        fs::write(&secret, "outside").unwrap();

        for rejected in [
            "",
            "/tmp/evil.md",
            "data/../../../evil.md",
            "config/settings.toml",
            "workspace/main/scratch.md",
            "attachments/0000/content.md",
            "skills/tool.md",
            "data/sub/nested.md",
            "note.md",
        ] {
            assert!(
                workspace.write_document(rejected, "x").is_err(),
                "write address must be rejected: {rejected}"
            );
        }
        assert_eq!(fs::read_to_string(&secret).unwrap(), "outside");
        assert!(!directory.path().join("tmp").join("evil.md").exists());
    }

    #[test]
    fn relative_path_strips_the_workspace_root_with_forward_slashes() {
        let (workspace, directory) = workspace();
        let storage = Storage::new(directory.path()).unwrap();
        assert_eq!(
            workspace.relative_path(&storage.data_dir.join("note.md")),
            Some("data/note.md".to_string())
        );
        assert_eq!(workspace.relative_path(&storage.root), None);
        assert_eq!(
            workspace.relative_path(Path::new("/definitely/outside.md")),
            None
        );
    }
}
