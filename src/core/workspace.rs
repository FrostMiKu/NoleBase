//! The workspace application service: one Nole workspace's data, indexes,
//! and attachments, owned as a single lifecycle object that any client
//! (TUI today, other clients later) can depend on instead of assembling
//! storage and index handles itself.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::core::attachment::{AttachmentId, AttachmentMetadata, AttachmentStore};
use crate::core::attachment_usage::{AttachmentUsageHandle, TrashError, TrashResult};
use crate::core::document_index::{document_group, DocumentGroup, DocumentIndex, DocumentIndexer};
use crate::core::model::{DailyNote, MutationConflict, NoteFile, SearchHit, TodoItem};
use crate::core::storage::{normalize_new_name, stage_replacement, AppendReceipt, Storage};
use crate::core::wiki_link_index::{
    matching_note_wiki_link_spans, replace_wiki_link_spans, validate_wiki_target, WikiLinkIndex,
    WikiLinkIndexHandle,
};
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

/// One document whose wiki links must be rewritten during a note rename: the
/// rewritten content plus where it is committed — the note's own new path
/// when the note self-links, the file itself for every other referencing
/// document.
struct NoteLinkRewrite {
    commit_at: PathBuf,
    before: String,
    after: String,
}

/// Managed documents containing a wiki target spelled `old_stem` or
/// `old_name` (case-insensitive) — every file whose links resolve to the
/// renamed note, deduplicated and sorted.
fn note_link_sources(index: &WikiLinkIndex, old_stem: &str, old_name: &str) -> Vec<PathBuf> {
    let mut files = index.locations_ignoring_case(old_stem);
    files.extend(index.locations_ignoring_case(old_name));
    files.sort();
    files.dedup();
    files
}

/// Whether another managed file shares `from`'s wikilink identity. Links
/// resolve by file name or stem case-insensitively, and either spelling of a
/// link to this note carries its stem, so any second file the stem resolves
/// to makes every `[[link]]` to the name ambiguous about which file it means.
fn note_identity_is_ambiguous(index: &WikiLinkIndex, from: &Path) -> bool {
    from.file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| index.resolve(stem).len() > 1)
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

    /// Read one dated daily note (`YYYY-MM-DD`) — the move flow's source.
    /// Fails on a malformed date or a day with no file.
    pub fn daily_note(&self, date: &str) -> Result<DailyNote> {
        self.storage.read_daily_by_date(date)
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
        self.append_today_tracked(body).map(|(note, _)| note)
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
        self.sync_index_paths(vec![target]);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Document lifecycle mutations
    //
    // Storage owns the raw filesystem operations; these methods own the
    // business rules — which transitions are legal for normal notes, archived
    // notes, and daily notes; wikilink rewriting on renames; and bringing the
    // shared indexes in step with the filesystem so callers never have to
    // refresh anything after a mutation returns.
    // -----------------------------------------------------------------------

    /// Create a new note in `data/` from a user-entered name, seeded with a
    /// heading. Returns its canonical path; existing entries are never
    /// overwritten.
    pub fn create_note(&self, name: &str) -> Result<PathBuf> {
        let path = self.storage.create_named_file(name)?;
        self.sync_index_paths(vec![path.clone()]);
        Ok(path)
    }

    /// Create a new note in `data/` from the workspace template. Returns its
    /// canonical path; existing entries are never overwritten.
    pub fn create_note_from_template(&self, name: &str) -> Result<PathBuf> {
        let path = self.storage.create_named_file_from_template(name)?;
        self.sync_index_paths(vec![path.clone()]);
        Ok(path)
    }

    /// Rename a note in `data/`, keeping `name.md`'s stem as the wikilink
    /// identity: every managed document whose `[[links]]` resolve to the note
    /// (by name or stem, case-insensitive — the renderer's rule) is rewritten
    /// to point at the new stem, through the same staged-atomic commit as the
    /// file move. Returns the new canonical path.
    ///
    /// Failure consistency: all rewrites are staged to sibling temp files
    /// before the note moves, so staging failures leave everything untouched;
    /// a failed publish rolls the already-committed documents back from the
    /// in-memory originals and moves the note back to its old name (best
    /// effort). The residual window is a hard crash between two publishes,
    /// which can leave part of the referencing set rewritten — the watcher
    /// re-indexes whatever state remains, and the error from any non-crash
    /// failure is always returned, never swallowed.
    pub fn rename_note(&self, from: &Path, new_name: &str) -> Result<PathBuf> {
        match document_group(&self.storage, from) {
            Some(DocumentGroup::Notes) => {}
            Some(DocumentGroup::Daily) => bail!(
                "daily notes are named by their date and cannot be renamed: {}",
                from.display()
            ),
            Some(DocumentGroup::Archives) => bail!(
                "note is archived; rename it in place with rename_archived_note: {}",
                from.display()
            ),
            None => bail!("not a managed note: {}", from.display()),
        }
        self.storage.validate_target(from)?;
        self.rename_managed_note(from, new_name)
    }

    /// Rename a note in `archives/`, rewriting wikilinks exactly like
    /// [`Workspace::rename_note`] (the wiki resolver treats archived notes as
    /// targets by name). Returns the new canonical path.
    pub fn rename_archived_note(&self, from: &Path, new_name: &str) -> Result<PathBuf> {
        match document_group(&self.storage, from) {
            Some(DocumentGroup::Archives) => {}
            Some(DocumentGroup::Daily) => bail!(
                "daily notes are named by their date and cannot be renamed: {}",
                from.display()
            ),
            Some(DocumentGroup::Notes) => bail!(
                "note is not archived; restore it first or use rename_note: {}",
                from.display()
            ),
            None => bail!("not a managed note: {}", from.display()),
        }
        self.storage.validate_archived_target(from)?;
        self.rename_managed_note(from, new_name)
    }

    /// Move a note from `data/` into `archives/`. Wikilinks are untouched:
    /// the wiki resolver matches targets by file name or stem, not by
    /// directory, so `[[note]]` keeps resolving to the archived note. Returns
    /// the canonical archive path.
    pub fn archive_note(&self, path: &Path) -> Result<PathBuf> {
        match document_group(&self.storage, path) {
            Some(DocumentGroup::Notes) => {}
            Some(DocumentGroup::Daily) => bail!(
                "daily notes are named by their date and cannot be archived: {}",
                path.display()
            ),
            Some(DocumentGroup::Archives) => {
                bail!("note is already archived: {}", path.display())
            }
            None => bail!("not a managed note: {}", path.display()),
        }
        let to = self.storage.archive_note(path)?;
        self.sync_index_paths(vec![path.to_path_buf(), to.clone()]);
        Ok(to)
    }

    /// Move a note from `archives/` back into `data/`. Wikilinks are
    /// untouched for the same reason as [`Workspace::archive_note`]. Returns
    /// the canonical note path.
    pub fn restore_note(&self, path: &Path) -> Result<PathBuf> {
        match document_group(&self.storage, path) {
            Some(DocumentGroup::Archives) => {}
            Some(DocumentGroup::Daily) => bail!(
                "daily notes are named by their date and are never archived: {}",
                path.display()
            ),
            Some(DocumentGroup::Notes) => bail!("note is not archived: {}", path.display()),
            None => bail!("not a managed note: {}", path.display()),
        }
        let to = self.storage.restore_archived_note(path)?;
        self.sync_index_paths(vec![path.to_path_buf(), to.clone()]);
        Ok(to)
    }

    /// Delete a managed note from `data/` or `archives/`. Wikilinks that
    /// pointed at it become dangling references, exactly as they are today
    /// for externally deleted notes; the wiki index stops resolving the note.
    pub fn delete_note(&self, path: &Path) -> Result<()> {
        match document_group(&self.storage, path) {
            Some(DocumentGroup::Notes) => self.storage.delete_file(path)?,
            Some(DocumentGroup::Archives) => self.storage.delete_archived_file(path)?,
            Some(DocumentGroup::Daily) => bail!(
                "daily notes are deleted by date; use delete_daily_note: {}",
                path.display()
            ),
            None => bail!("not a managed note: {}", path.display()),
        }
        self.sync_index_paths(vec![path.to_path_buf()]);
        Ok(())
    }

    /// Append to a dated daily note, creating `daily/YYYY-MM-DD.md` on the
    /// first append — the TUI's flow for opening and writing a past day.
    pub fn append_daily(&self, date: &str, body: &str) -> Result<DailyNote> {
        self.append_daily_tracked(date, body).map(|(note, _)| note)
    }

    /// The receipt-tracked form of [`Workspace::append_daily`] for callers
    /// that keep an undo stack.
    pub fn append_daily_tracked(
        &self,
        date: &str,
        body: &str,
    ) -> Result<(DailyNote, AppendReceipt)> {
        let (note, receipt) = self.storage.append_daily_tracked(date, body)?;
        self.sync_index_paths(vec![receipt.path().to_path_buf()]);
        Ok((note, receipt))
    }

    /// The receipt-tracked form of [`Workspace::append_to_today`].
    pub fn append_today_tracked(&self, body: &str) -> Result<(DailyNote, AppendReceipt)> {
        let (note, receipt) = self.storage.append_to_today_tracked(body)?;
        self.sync_index_paths(vec![receipt.path().to_path_buf()]);
        Ok((note, receipt))
    }

    /// Append to an article in `data/` or `archives/`, returning the receipt
    /// required to undo that exact write.
    pub fn append_note_tracked(&self, path: &Path, body: &str) -> Result<AppendReceipt> {
        let receipt = self.storage.append_document_tracked(path, body)?;
        self.sync_index_paths(vec![path.to_path_buf()]);
        Ok(receipt)
    }

    /// Undo the append described by `receipt` (see [`AppendReceipt`]),
    /// truncating the file back to its pre-append content or removing it when
    /// the append created it.
    pub fn undo_append(&self, receipt: &AppendReceipt) -> Result<()> {
        self.storage.undo_append(receipt)?;
        self.sync_index_paths(vec![receipt.path().to_path_buf()]);
        Ok(())
    }

    /// Delete the daily note for `date`. Returns whether a file was removed.
    pub fn delete_daily_note(&self, date: &str) -> Result<bool> {
        let path = self.storage.daily_file_path(date)?;
        let removed = self.storage.remove_daily(date)?;
        if removed {
            self.sync_index_paths(vec![path]);
        }
        Ok(removed)
    }

    /// Restore a deleted (or filed) daily note by rewriting its file.
    pub fn restore_daily_note(&self, note: &DailyNote) -> Result<()> {
        self.storage.restore_daily(note)?;
        let path = self.storage.daily_file_path(&note.date.to_string())?;
        self.sync_index_paths(vec![path]);
        Ok(())
    }

    /// File a daily note into a managed note (`data/` or `archives/`): append
    /// its body as a dated section, then remove the daily file. Returns the
    /// appended section, which undo needs to strip back out.
    ///
    /// The target must be an existing note: filing never creates a file, so
    /// an unmanaged kind refuses before existence is even checked.
    pub fn move_daily_to_note(&self, target: &Path, note: &DailyNote) -> Result<String> {
        match document_group(&self.storage, target) {
            Some(DocumentGroup::Notes) => {}
            Some(DocumentGroup::Archives) => bail!(
                "daily notes file into a note in data/, not the archive: {}",
                target.display()
            ),
            Some(DocumentGroup::Daily) => bail!(
                "daily notes file into a note in data/, not another day: {}",
                target.display()
            ),
            None => bail!("not a managed note: {}", target.display()),
        }
        std::fs::symlink_metadata(target)
            .with_context(|| format!("checking move target {}", target.display()))?;
        let daily_path = self.storage.daily_file_path(&note.date.to_string())?;
        let appended = self.storage.move_to_markdown(target, note)?;
        self.sync_index_paths(vec![target.to_path_buf(), daily_path]);
        Ok(appended)
    }

    /// Remove the first occurrence of `needle` from a managed document.
    /// Returns `true` if found and removed. Used by undo to clean up a filed
    /// daily-note copy.
    pub fn remove_first_occurrence(&self, path: &Path, needle: &str) -> Result<bool> {
        let removed = self.storage.remove_first_occurrence(path, needle)?;
        if removed {
            self.sync_index_paths(vec![path.to_path_buf()]);
        }
        Ok(removed)
    }

    /// Bring every index that can be refreshed in place — the workspace
    /// search/tag index and the wiki-link index — in step with the
    /// filesystem for `paths`, then queue the background document indexer so
    /// its published snapshot and on-disk cache reconverge (and republish the
    /// attachment-usage index). Called at the end of every mutation, so
    /// search, tags, wikilinks, and backlinks are correct the moment the
    /// mutation returns; callers never refresh anything themselves.
    fn sync_index_paths(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.workspace_index
            .refresh_paths(&self.storage, paths.clone());
        self.wiki_links.refresh_paths(&self.storage, paths.clone());
        self.refresh_documents(paths);
    }

    /// Shared body of [`Workspace::rename_note`] and
    /// [`Workspace::rename_archived_note`] once the group checks and full
    /// validation have passed: normalize the name, refuse collisions, plan
    /// the wikilink rewrites from the shared index, then stage-and-commit the
    /// whole set.
    fn rename_managed_note(&self, from: &Path, new_name: &str) -> Result<PathBuf> {
        let name = normalize_new_name(new_name)?;
        let stem = Path::new(&name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        // The stem becomes the `[[wikilink]]` identity of the renamed note, so
        // it must be a target the MBDown tokenizer can even represent.
        validate_wiki_target(stem)
            .with_context(|| format!("note names cannot become wiki targets: {new_name}"))?;
        let directory = from
            .parent()
            .context("managed note has no parent directory")?;
        let to = directory.join(&name);
        if to.file_name() == from.file_name() {
            return Ok(from.to_path_buf());
        }
        match std::fs::symlink_metadata(&to) {
            Ok(_) => {
                return Err(anyhow::Error::new(MutationConflict(format!(
                    "a file named {name} already exists"
                ))));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", to.display()));
            }
        }
        let rewrites = self.plan_note_link_rewrites(from, &to)?;
        self.apply_note_rename(from, &to, &rewrites)?;
        let mut paths = vec![from.to_path_buf(), to.clone()];
        paths.extend(rewrites.iter().map(|rewrite| rewrite.commit_at.clone()));
        self.sync_index_paths(paths);
        Ok(to)
    }

    /// Find every managed document whose wiki links resolve to `from` — the
    /// note itself included when it self-links — using the shared wiki-link
    /// index (`locations_ignoring_case` for both the stem and file-name
    /// spellings) and the MBDown parser for the exact source spans, then plan
    /// the rewritten text. Only real wiki-link events count: targets inside
    /// code, HTML comments, escaped text, or `![[...]]` embeds remain content
    /// and are left untouched, and links to other notes never match.
    fn plan_note_link_rewrites(&self, from: &Path, to: &Path) -> Result<Vec<NoteLinkRewrite>> {
        let new_stem = to
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let old_stem = from
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let old_name = from
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let (sources, ambiguous) = match self.wiki_links.with_index(|index| {
            (
                note_link_sources(index, &old_stem, &old_name),
                note_identity_is_ambiguous(index, from),
            )
        }) {
            Some(found) => found,
            None => {
                // The shared snapshot has not published yet (the startup
                // window). Derive the referencing set from one synchronous
                // checked scan instead of refusing the rename — the scan
                // fails closed, so an unreadable managed directory aborts
                // rather than silently missing backlinks.
                let index =
                    WikiLinkIndex::from_documents(&DocumentIndex::build_checked(&self.storage)?);
                (
                    note_link_sources(&index, &old_stem, &old_name),
                    note_identity_is_ambiguous(&index, from),
                )
            }
        };
        // A `[[wikilink]]` names a note by stem alone, so when another
        // managed file shares this note's stem no link can say which of the
        // two it means. Migrating any such link to the new name would
        // silently retarget the ones that meant the other file, and leaving
        // it behind would strand them, so the rename is refused while any
        // real backlink exists.
        if ambiguous && !sources.is_empty() {
            return Err(anyhow::Error::new(MutationConflict(format!(
                "cannot safely rename an ambiguously named note while backlinks exist: {old_stem}"
            ))));
        }
        let mut rewrites = Vec::new();
        for path in sources {
            // Reject candidates that resolve outside the managed
            // directories: a managed directory swapped for a symlink must
            // never redirect the rewrite outside daily/, data/, archives/.
            self.storage.validate_wiki_note(&path)?;
            let before = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let spans = matching_note_wiki_link_spans(&before, from);
            if spans.is_empty() {
                continue;
            }
            let after = replace_wiki_link_spans(&before, &spans, &new_stem);
            rewrites.push(NoteLinkRewrite {
                commit_at: if path == from { to.to_path_buf() } else { path },
                before,
                after,
            });
        }
        Ok(rewrites)
    }

    /// Stage every rewritten document next to its target, move the note
    /// itself, then publish the staged rewrites atomically. A staging or move
    /// failure leaves every original untouched; a publish failure rolls the
    /// already-committed documents back from the in-memory originals and
    /// moves the note back to its old name (best effort). The residual
    /// failure window is documented on [`Workspace::rename_note`].
    fn apply_note_rename(
        &self,
        from: &Path,
        to: &Path,
        rewrites: &[NoteLinkRewrite],
    ) -> Result<()> {
        let mut staged: Vec<Option<PathBuf>> = vec![None; rewrites.len()];
        for (index, rewrite) in rewrites.iter().enumerate() {
            match stage_replacement(&rewrite.commit_at, &rewrite.after) {
                Ok(temp) => staged[index] = Some(temp),
                Err(error) => {
                    for pending in staged.iter().flatten() {
                        let _ = std::fs::remove_file(pending);
                    }
                    return Err(error);
                }
            }
        }
        if let Err(error) = std::fs::rename(from, to) {
            for pending in staged.iter().flatten() {
                let _ = std::fs::remove_file(pending);
            }
            return Err(error)
                .with_context(|| format!("renaming {} to {}", from.display(), to.display()));
        }
        for (index, rewrite) in rewrites.iter().enumerate() {
            let temp = staged[index]
                .take()
                .expect("every staged replacement exists after staging");
            if let Err(error) =
                crate::core::storage::replace_file_atomically(&temp, &rewrite.commit_at)
            {
                for done in rewrites.iter().take(index) {
                    let _ = std::fs::write(&done.commit_at, &done.before);
                }
                for pending in staged.iter().flatten() {
                    let _ = std::fs::remove_file(pending);
                }
                let _ = std::fs::rename(to, from);
                return Err(error).with_context(|| {
                    format!("updating wiki links in {}", rewrite.commit_at.display())
                });
            }
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

    /// A workspace whose initial index snapshot already covers the files the
    /// `setup` closure wrote, so mutations can assert immediate index state.
    fn indexed_workspace(setup: impl FnOnce(&Storage)) -> (Workspace, tempfile::TempDir, Storage) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        setup(&storage);
        let workspace = Workspace::open(storage.clone()).unwrap();
        assert!(
            wait_for_index(&workspace),
            "initial index snapshot must settle"
        );
        (workspace, directory, storage)
    }

    /// Every search hit path for `query`.
    fn search_paths(workspace: &Workspace, query: &str) -> Vec<PathBuf> {
        workspace
            .search(query)
            .expect("index must be published")
            .into_iter()
            .filter_map(|hit| match hit {
                SearchHit::FileLine { path, .. } => Some(path),
                SearchHit::DocumentLine { .. } => None,
            })
            .collect()
    }

    fn resolve(workspace: &Workspace, target: &str) -> Vec<PathBuf> {
        workspace
            .wiki_links()
            .with_index(|index| index.resolve(target))
            .expect("wiki index must be published")
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

    // ---------------------------------------------------------------------
    // Document lifecycle mutations
    // ---------------------------------------------------------------------

    #[test]
    fn create_note_creates_reads_and_indexes_immediately() {
        let (workspace, directory, _storage) = indexed_workspace(|_| {});
        let storage = Storage::new(directory.path()).unwrap();

        let path = workspace.create_note("foo").unwrap();
        assert_eq!(path, storage.data_dir.join("foo.md"));
        assert!(path.is_file());
        assert_eq!(workspace.read_document("data/foo.md").unwrap(), "# foo\n\n");

        // The mutation returns with the search index already consistent —
        // no poll, refresh, or reload needed.
        let hits = search_paths(&workspace, "# foo");
        assert_eq!(hits, vec![storage.data_dir.join("foo.md")]);
        let files = workspace.note_files().unwrap();
        assert!(files.iter().any(|file| file.path == path));
    }

    #[test]
    fn create_note_rejects_invalid_names_and_never_overwrites() {
        let (workspace, _directory, _storage) = indexed_workspace(|_| {});

        for rejected in ["", "  ", "a/b", "..", "a\\b"] {
            assert!(
                workspace.create_note(rejected).is_err(),
                "name must be rejected: {rejected}"
            );
        }
        workspace.create_note("existing").unwrap();
        assert!(workspace.create_note("existing").is_err());
    }

    #[test]
    fn daily_note_lifecycle_follows_the_tui_semantics() {
        let (workspace, _directory, storage) = indexed_workspace(|_| {});

        // The TUI creates a day's file by appending to it: first append
        // writes `YYYY-MM-DD.md`, later appends join the same card.
        let first = workspace.append_daily("2026-01-15", "hello").unwrap();
        assert_eq!(first.date.to_string(), "2026-01-15");
        assert_eq!(first.body, "hello");
        let daily_path = storage.daily_dir.join("2026-01-15.md");
        assert!(daily_path.is_file());
        assert_eq!(
            workspace.read_document("daily/2026-01-15.md").unwrap(),
            "hello\n"
        );
        let hits = search_paths(&workspace, "hello");
        assert_eq!(hits, vec![daily_path.clone()]);

        workspace.append_daily("2026-01-15", "world").unwrap();
        let notes = workspace.daily_notes().unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].body, "hello\n\nworld");

        // Delete + undo restore through the workspace keep the index current.
        assert!(workspace.delete_daily_note("2026-01-15").unwrap());
        assert!(!daily_path.exists());
        assert!(search_paths(&workspace, "hello").is_empty());
        workspace.restore_daily_note(&notes[0]).unwrap();
        assert_eq!(
            workspace.read_document("daily/2026-01-15.md").unwrap(),
            "hello\n\nworld\n"
        );
        assert_eq!(search_paths(&workspace, "hello"), vec![daily_path]);
    }

    #[test]
    fn rename_note_without_backlinks_moves_the_identity_everywhere() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "needle-one\n").unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let bar = storage.data_dir.join("bar.md");

        let to = workspace.rename_note(&foo, "bar").unwrap();
        assert_eq!(to, bar);
        assert!(!foo.exists());
        assert!(bar.is_file());
        assert_eq!(
            workspace.read_document("data/bar.md").unwrap(),
            "needle-one\n"
        );

        // Search and the wiki identity reflect the new note immediately.
        assert_eq!(search_paths(&workspace, "needle-one"), vec![bar.clone()]);
        assert!(resolve(&workspace, "foo").is_empty());
        assert_eq!(resolve(&workspace, "bar"), vec![bar.clone()]);
    }

    #[test]
    fn rename_note_rewrites_wikilinks_and_backlinks() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "the target\n").unwrap();
            fs::write(storage.data_dir.join("a.md"), "see [[foo]] today\n").unwrap();
            fs::write(
                storage.data_dir.join("b.mb"),
                "[[FOO]] case [[foo.md]] ext\n",
            )
            .unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let bar = storage.data_dir.join("bar.md");
        let a = storage.data_dir.join("a.md");
        let b = storage.data_dir.join("b.mb");

        assert_eq!(
            workspace.backlinks(&foo).unwrap(),
            vec![a.clone(), b.clone()]
        );

        workspace.rename_note(&foo, "bar").unwrap();

        // Every spelling that resolved to foo now resolves to bar; nothing
        // else in the source changed.
        assert_eq!(
            fs::read_to_string(&a).unwrap(),
            "see [[bar]] today\n",
            "plain link rewritten, surrounding text kept"
        );
        assert_eq!(
            fs::read_to_string(&b).unwrap(),
            "[[bar]] case [[bar]] ext\n",
            "case-insensitive and file-name spellings rewritten"
        );
        assert_eq!(
            workspace.backlinks(&bar).unwrap(),
            vec![a.clone(), b.clone()]
        );
        assert!(workspace.backlinks(&foo).unwrap().is_empty());
        assert_eq!(resolve(&workspace, "foo"), Vec::<PathBuf>::new());
        assert_eq!(resolve(&workspace, "bar"), vec![bar.clone()]);

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
    fn rename_note_rewrites_a_self_link_in_place() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "self [[foo]] link\n").unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let bar = storage.data_dir.join("bar.md");

        workspace.rename_note(&foo, "bar").unwrap();

        assert_eq!(
            fs::read_to_string(&bar).unwrap(),
            "self [[bar]] link\n",
            "the note's own reference follows it to the new name"
        );
        assert_eq!(resolve(&workspace, "bar"), vec![bar.clone()]);
    }

    #[test]
    fn rename_note_refuses_an_ambiguous_identity_while_backlinks_exist() {
        // `[[foo]]` cannot say which of the two foo.md files it means, so
        // renaming either one must refuse instead of migrating the links.
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "the target\n").unwrap();
            fs::write(storage.archives_dir.join("foo.md"), "the other target\n").unwrap();
            fs::write(
                storage.data_dir.join("a.md"),
                "see [[foo]] and [[foo.md]]\n",
            )
            .unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let other = storage.archives_dir.join("foo.md");
        let a = storage.data_dir.join("a.md");
        let a_before = fs::read_to_string(&a).unwrap();

        let error = workspace.rename_note(&foo, "bar").unwrap_err();
        assert!(
            error.to_string().contains("ambiguous"),
            "error must state the ambiguity: {error:#}"
        );

        // Nothing moved, nothing rewritten, index state unchanged.
        assert!(foo.is_file());
        assert!(other.is_file());
        assert!(!storage.data_dir.join("bar.md").exists());
        assert_eq!(fs::read_to_string(&a).unwrap(), a_before);
        assert_eq!(workspace.backlinks(&foo).unwrap(), vec![a.clone()]);
        assert_eq!(
            resolve(&workspace, "foo"),
            vec![other.clone(), foo.clone()],
            "both spellings of the name stay ambiguous"
        );

        // The archived twin is refused for the same reason.
        let error = workspace.rename_archived_note(&other, "bar").unwrap_err();
        assert!(error.to_string().contains("ambiguous"), "{error:#}");
        assert!(other.is_file());
    }

    #[test]
    fn rename_note_allows_an_ambiguous_identity_without_backlinks() {
        // No `[[foo]]` exists anywhere, so nothing needs migrating and the
        // rename is free to move one of the same-named files.
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "data copy\n").unwrap();
            fs::write(storage.archives_dir.join("foo.md"), "archive copy\n").unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let other = storage.archives_dir.join("foo.md");

        let to = workspace.rename_note(&foo, "bar").unwrap();
        assert_eq!(to, storage.data_dir.join("bar.md"));
        assert!(!foo.exists());
        assert!(other.is_file(), "the archive twin is untouched");
        // The surviving archive copy keeps the name identity.
        assert_eq!(resolve(&workspace, "foo"), vec![other.clone()]);
        assert_eq!(resolve(&workspace, "bar"), vec![to]);
    }

    #[test]
    fn rename_note_touches_only_real_wikilinks() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "target\n").unwrap();
            let source = "plain foo and foo.md in prose\n\
                          real [[foo]] link\n\
                          near-miss [[foobar]] link\n\
                          fenced ```text\n[[foo]]```\n\
                          inline `[[foo]]` code\n\
                          escaped \\[[foo]]\n\
                          embed ![[foo]]\n";
            fs::write(storage.data_dir.join("doc.md"), source).unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let doc = storage.data_dir.join("doc.md");
        let before = fs::read_to_string(&doc).unwrap();

        workspace.rename_note(&foo, "bar").unwrap();

        let after = fs::read_to_string(&doc).unwrap();
        assert_ne!(before, after);
        // Exactly the one real link changed; prose, near-misses, code,
        // escapes, and embeds are byte-identical.
        let expected = before.replace("real [[foo]] link", "real [[bar]] link");
        assert_eq!(after, expected);
    }

    #[test]
    fn rename_note_failure_leaves_every_source_untouched() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "target\n").unwrap();
            fs::write(storage.data_dir.join("bar.md"), "collision\n").unwrap();
            fs::write(storage.data_dir.join("a.md"), "see [[foo]]\n").unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let a = storage.data_dir.join("a.md");
        let a_before = fs::read_to_string(&a).unwrap();

        assert!(workspace.rename_note(&foo, "bar").is_err());
        assert!(foo.is_file());
        assert_eq!(
            fs::read_to_string(storage.data_dir.join("bar.md")).unwrap(),
            "collision\n"
        );
        assert_eq!(fs::read_to_string(&a).unwrap(), a_before);
        assert_eq!(
            workspace.backlinks(&foo).unwrap(),
            vec![a.clone()],
            "index untouched after the failed rename"
        );

        // Renaming to the same name is a no-op returning the current path.
        assert_eq!(workspace.rename_note(&foo, "foo").unwrap(), foo);
    }

    #[test]
    fn archive_note_moves_without_touching_wikilinks() {
        // The wiki resolver matches by file name or stem, not directory, so
        // `[[foo]]` keeps resolving to the archived note: archiving must not
        // rewrite any source file.
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "needle-one\n").unwrap();
            fs::write(storage.data_dir.join("a.md"), "see [[foo]]\n").unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let a = storage.data_dir.join("a.md");
        let a_before = fs::read_to_string(&a).unwrap();

        let archived = workspace.archive_note(&foo).unwrap();
        assert_eq!(archived, storage.archives_dir.join("foo.md"));
        assert!(!foo.exists());
        assert!(archived.is_file());
        assert_eq!(fs::read_to_string(&a).unwrap(), a_before);

        // Identity and search follow the note to its archive location.
        assert_eq!(resolve(&workspace, "foo"), vec![archived.clone()]);
        assert_eq!(workspace.backlinks(&archived).unwrap(), vec![a.clone()]);
        assert_eq!(search_paths(&workspace, "needle-one"), vec![archived]);
    }

    #[test]
    fn restore_note_round_trips_without_touching_wikilinks() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("a.md"), "see [[foo]]\n").unwrap();
        });
        let a = storage.data_dir.join("a.md");
        let foo = workspace.create_note("foo").unwrap();
        let archived = workspace.archive_note(&foo).unwrap();

        let restored = workspace.restore_note(&archived).unwrap();
        assert_eq!(restored, foo);
        assert!(foo.is_file());
        assert!(!archived.exists());
        assert_eq!(fs::read_to_string(&a).unwrap(), "see [[foo]]\n");
        assert_eq!(resolve(&workspace, "foo"), vec![foo.clone()]);
        assert_eq!(workspace.backlinks(&foo).unwrap(), vec![a.clone()]);
    }

    #[test]
    fn delete_note_updates_indexes_and_keeps_dangling_references() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "needle-one\n").unwrap();
            fs::write(storage.data_dir.join("a.md"), "see [[foo]]\n").unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let a = storage.data_dir.join("a.md");

        workspace.delete_note(&foo).unwrap();
        assert!(!foo.exists());
        assert!(search_paths(&workspace, "needle-one").is_empty());
        assert!(resolve(&workspace, "foo").is_empty());

        // Dangling references keep their established representation: the
        // index still records that a.md mentions [[foo]]; the note itself is
        // simply no longer resolvable. The source is never rewritten.
        assert_eq!(fs::read_to_string(&a).unwrap(), "see [[foo]]\n");
        let dangling = workspace
            .wiki_links()
            .with_index(|index| index.locations_ignoring_case("foo"))
            .unwrap();
        assert_eq!(dangling, vec![a.clone()]);
    }

    #[test]
    fn delete_archived_note_updates_indexes() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("a.md"), "see [[foo]]\n").unwrap();
        });
        let a = storage.data_dir.join("a.md");
        let foo = workspace.create_note("foo").unwrap();
        let archived = workspace.archive_note(&foo).unwrap();

        workspace.delete_note(&archived).unwrap();
        assert!(!archived.exists());
        assert!(resolve(&workspace, "foo").is_empty());
        // Backlinks are name-based, so the dangling `[[foo]]` in a.md still
        // matches the deleted note's name — the established representation
        // for references to a note that no longer exists.
        assert_eq!(workspace.backlinks(&archived).unwrap(), vec![a.clone()]);
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.data_dir.join("foo.md"), "target\n").unwrap();
            fs::write(storage.archives_dir.join("old.md"), "archived\n").unwrap();
            fs::write(storage.daily_dir.join("2026-01-15.md"), "day\n").unwrap();
        });
        let foo = storage.data_dir.join("foo.md");
        let old = storage.archives_dir.join("old.md");
        let daily = storage.daily_dir.join("2026-01-15.md");
        let missing = storage.data_dir.join("missing.md");

        let refuses = |result: anyhow::Result<PathBuf>, label: &str| {
            assert!(result.is_err(), "{label} must be refused");
        };

        // archive/restore transitions
        refuses(workspace.archive_note(&old), "archive(archived)");
        refuses(workspace.archive_note(&daily), "archive(daily)");
        refuses(workspace.restore_note(&foo), "restore(normal)");
        refuses(workspace.restore_note(&daily), "restore(daily)");
        refuses(
            workspace.rename_archived_note(&foo, "x"),
            "rename_archived(normal)",
        );
        refuses(workspace.rename_note(&old, "x"), "rename(archived)");
        refuses(workspace.rename_note(&daily, "x"), "rename(daily)");

        // Daily files are deleted by date, never by note path.
        assert!(workspace.delete_note(&daily).is_err());
        // Unmanaged and missing paths refuse too.
        assert!(workspace
            .delete_note(&storage.root.join("outside.md"))
            .is_err());
        assert!(workspace.rename_note(&missing, "x").is_err());
        assert!(workspace.archive_note(&missing).is_err());
        // A name that cannot be a wiki target refuses: brackets would leave
        // every rewritten link unparsable.
        assert!(workspace.rename_note(&foo, "bad[name").is_err());
        assert!(foo.is_file(), "refused rename leaves the note in place");

        // The daily note itself is untouched by every refused operation.
        assert!(daily.is_file());
    }

    #[test]
    fn undo_flows_through_the_workspace() {
        let (workspace, _directory, storage) = indexed_workspace(|_| {});

        // Append + recall (undo) via the tracked receipt.
        let (_, receipt) = workspace.append_today_tracked("today line").unwrap();
        workspace.undo_append(&receipt).unwrap();
        assert!(search_paths(&workspace, "today line").is_empty());

        // Filing a daily note into an article, then undoing it.
        let note = workspace.append_daily("2026-02-02", "filed idea").unwrap();
        let target = workspace.create_note("journal").unwrap();
        let appended = workspace.move_daily_to_note(&target, &note).unwrap();
        assert!(appended.contains("## 2026-02-02"));
        assert!(!storage.daily_dir.join("2026-02-02.md").exists());
        assert!(search_paths(&workspace, "filed idea").len() == 1);

        workspace.restore_daily_note(&note).unwrap();
        assert!(workspace
            .remove_first_occurrence(&target, &appended)
            .unwrap());
        let journal = fs::read_to_string(&target).unwrap();
        assert!(!journal.contains("## 2026-02-02"));
        assert_eq!(search_paths(&workspace, "filed idea").len(), 1);
    }

    #[test]
    fn rename_archived_note_rewrites_wikilinks_like_a_plain_rename() {
        // The resolver treats archived notes as link targets by name, so an
        // archives/ rename breaks `[[...]]` links exactly like a data rename.
        let (workspace, _directory, storage) = indexed_workspace(|storage| {
            fs::write(storage.archives_dir.join("old.md"), "archived body\n").unwrap();
            fs::write(storage.data_dir.join("a.md"), "see [[old]]\n").unwrap();
        });
        let old = storage.archives_dir.join("old.md");
        let a = storage.data_dir.join("a.md");

        let to = workspace.rename_archived_note(&old, "new").unwrap();
        assert_eq!(to, storage.archives_dir.join("new.md"));
        assert_eq!(fs::read_to_string(&a).unwrap(), "see [[new]]\n");
        assert_eq!(resolve(&workspace, "new"), vec![to.clone()]);
        assert_eq!(workspace.backlinks(&to).unwrap(), vec![a.clone()]);
    }
}
