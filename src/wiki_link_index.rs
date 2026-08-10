//! Derived index of wiki-link targets referenced by managed notes.
//!
//! The index parses each managed Markdown/MBDown file (daily/, data/,
//! archives/) with the same MBDown parser the renderer uses and records the
//! `[[...]]` targets that appear as wiki-link events. Targets inside fenced or
//! inline code, HTML comments, escaped text, and `![[...]]` embed bodies never
//! produce those events and are therefore not references: the renderer itself
//! does not render them as wiki links.
//!
//! Per target the index preserves the total occurrence count and the distinct
//! set of referencing managed notes. The shared document indexer rebuilds at
//! startup (reusing its validated cache when possible), then refreshes from
//! watcher events and publishes snapshots with the last applied revision.
//!
//! Backlinks resolve from the same index: a note at `path` is referenced by
//! every managed file containing a wiki target that matches `path` by file
//! name or stem, exactly as the renderer resolves a clicked `[[target]]`.

use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use mbdown::{Event, Node};

use crate::document_index::DocumentIndex;
#[cfg(test)]
use crate::storage::Storage;

/// Aggregate reference data for one wiki-link target.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReferenceEntry {
    /// Total occurrences across all managed files.
    count: usize,
    /// Distinct managed files that reference the target, sorted by path.
    locations: Vec<PathBuf>,
}

/// Derived wiki-link index: wiki target -> referencing notes.
#[derive(Clone, Debug, Default)]
pub struct WikiLinkIndex {
    /// Per managed file, the wiki targets it references (with duplicates).
    files: HashMap<PathBuf, Vec<String>>,
    /// Wiki target -> aggregate reference data.
    references: HashMap<String, ReferenceEntry>,
}

impl WikiLinkIndex {
    pub(crate) fn from_documents(documents: &DocumentIndex) -> Self {
        let files = documents
            .iter()
            .map(|(path, document)| (path.clone(), document.wiki_links.clone()))
            .collect();
        let mut index = Self {
            files,
            references: HashMap::new(),
        };
        index.rebuild_references();
        index
    }

    /// Scan every managed Markdown/MBDown file and index its wiki targets.
    #[cfg(test)]
    pub fn build(storage: &Storage) -> Self {
        Self::from_documents(&DocumentIndex::build(storage))
    }

    /// Re-index the given paths (created, modified, or removed). Paths that are
    /// not managed Markdown files are ignored, so watcher events for the
    /// attachments and workspace directories never disturb the index.
    #[cfg(test)]
    pub fn refresh_paths(&mut self, storage: &Storage, paths: &[PathBuf]) {
        let mut unique = paths.iter().cloned().collect::<HashSet<_>>();
        let mut changed = false;
        for path in unique.drain() {
            if self.files.remove(&path).is_some() {
                changed = true;
            }
            if let Some(document) = crate::document_index::index_file(storage, &path) {
                self.files.insert(path, document.wiki_links);
                changed = true;
            }
        }
        if changed {
            self.rebuild_references();
        }
    }

    #[cfg(test)]
    /// Total occurrences of the wiki target across all managed files.
    pub fn reference_count(&self, target: &str) -> usize {
        self.references
            .get(target)
            .map(|entry| entry.count)
            .unwrap_or(0)
    }

    #[cfg(test)]
    /// Distinct managed files referencing the wiki target, sorted by path.
    pub fn locations(&self, target: &str) -> Vec<PathBuf> {
        self.references
            .get(target)
            .map(|entry| entry.locations.clone())
            .unwrap_or_default()
    }

    /// Distinct managed files referencing any case-insensitive spelling of the
    /// wiki target, sorted by path. The index keys on the exact target text as
    /// written, so rename discovery must match case-insensitively like
    /// [`matching_wiki_link_spans`] or `[[old]]` would never be found when the
    /// requested target is `Old`.
    pub fn locations_ignoring_case(&self, target: &str) -> Vec<PathBuf> {
        let mut locations = Vec::new();
        for (key, entry) in &self.references {
            if key.eq_ignore_ascii_case(target) {
                for path in &entry.locations {
                    if !locations.contains(path) {
                        locations.push(path.clone());
                    }
                }
            }
        }
        locations.sort();
        locations
    }

    #[cfg(test)]
    /// Whether any managed file references the wiki target.
    pub fn is_referenced(&self, target: &str) -> bool {
        self.references.contains_key(target)
    }

    /// Resolve a wiki target to the managed notes it names, by file name or
    /// stem, case-insensitively — the same matching the renderer uses to
    /// activate a `[[target]]`. The result is sorted by path.
    pub fn resolve(&self, target: &str) -> Vec<PathBuf> {
        let mut resolved = self
            .files
            .keys()
            .filter(|path| wiki_name_matches(path, target))
            .cloned()
            .collect::<Vec<_>>();
        resolved.sort();
        resolved
    }

    /// Distinct managed files that link to the note at `path`: every file
    /// containing a wiki target that matches `path` by file name or stem
    /// (case-insensitive), the same matching the renderer uses to activate a
    /// `[[target]]`. The note itself is excluded, and the result is sorted by
    /// path.
    pub fn backlinks(&self, path: &Path) -> Vec<PathBuf> {
        let mut backlinks = Vec::new();
        for (file, targets) in &self.files {
            if file == path {
                continue;
            }
            if targets.iter().any(|target| wiki_name_matches(path, target)) {
                backlinks.push(file.clone());
            }
        }
        backlinks.sort();
        backlinks
    }

    fn rebuild_references(&mut self) {
        self.references.clear();
        for (path, targets) in &self.files {
            for target in targets {
                let entry = self.references.entry(target.clone()).or_default();
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

/// Whether `path`'s file name or stem matches `requested`, case-insensitively —
/// the resolution rule for `[[wikilink]]` targets.
pub(crate) fn wiki_name_matches(path: &Path, requested: &str) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(requested))
        || path
            .file_stem()
            .is_some_and(|stem| stem.to_string_lossy().eq_ignore_ascii_case(requested))
}

/// Shared, read-only wiki-link index state. Cheap to clone: every clone
/// observes the same latest published snapshot, so the UI and the agent tools
/// resolve backlinks against one consistent index.
#[derive(Clone, Default)]
pub struct WikiLinkIndexHandle(Arc<RwLock<Option<WikiLinkIndex>>>);

impl WikiLinkIndexHandle {
    /// Publish the latest index snapshot from the background indexer.
    pub fn replace(&self, index: WikiLinkIndex) {
        if let Ok(mut current) = self.0.write() {
            *current = Some(index);
        }
    }

    /// Run `f` against the latest index, or `None` before the first snapshot.
    pub fn with_index<T>(&self, f: impl FnOnce(&WikiLinkIndex) -> T) -> Option<T> {
        let current = self.0.read().ok()?;
        current.as_ref().map(f)
    }
}

/// Source spans of every `[[target]]` whose target equals `from`
/// (case-insensitive), in source order. Only real wiki-link events count:
/// targets inside code, HTML comments, escaped text, or embeds never produce
/// those events and are left untouched.
pub fn matching_wiki_link_spans(source: &str, from: &str) -> Vec<Range<usize>> {
    let Ok(document) = mbdown::parse(source) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    collect_matching_wiki_link_spans(document.nodes(), from, &mut spans);
    spans.sort_by_key(|span| span.start);
    spans
}

fn collect_matching_wiki_link_spans(nodes: &[Node<'_>], from: &str, spans: &mut Vec<Range<usize>>) {
    for node in nodes {
        match node {
            Node::Markdown(markdown) => {
                for event in markdown.events() {
                    if let Event::WikiLink(target) = &event.event {
                        if target.eq_ignore_ascii_case(from) {
                            let offset = markdown.source_span().start;
                            spans.push(offset + event.span.start..offset + event.span.end);
                        }
                    }
                }
            }
            Node::Box { children, .. }
            | Node::Center { children }
            | Node::Right { children }
            | Node::Indent { children, .. }
            | Node::Columns { children, .. }
            | Node::Column { children, .. } => {
                collect_matching_wiki_link_spans(children, from, spans)
            }
        }
    }
}

/// Replace every span in `spans` (the full `[[...]]` region) with `[[to]]`,
/// applying replacements back-to-front so earlier offsets stay valid.
pub fn replace_wiki_link_spans(source: &str, spans: &[Range<usize>], to: &str) -> String {
    let mut output = source.to_string();
    for span in spans.iter().rev() {
        output.replace_range(span.clone(), &format!("[[{to}]]"));
    }
    output
}

/// Every wiki-link target in `text` that the MBDown renderer would render as
/// a wiki link, in source order, including duplicates. Targets inside code,
/// HTML comments, escaped text, or `![[...]]` embed bodies never produce those
/// events and are therefore not references.
#[cfg(test)]
pub fn find_wiki_links(text: &str) -> Vec<String> {
    let Ok(document) = mbdown::parse(text) else {
        return Vec::new();
    };
    collect_wiki_links(document.nodes())
}

pub(crate) fn collect_wiki_links(nodes: &[Node<'_>]) -> Vec<String> {
    let mut links = Vec::new();
    collect_node_wiki_links(nodes, &mut links);
    links
}

fn collect_node_wiki_links(nodes: &[Node<'_>], links: &mut Vec<String>) {
    for node in nodes {
        match node {
            Node::Markdown(markdown) => {
                for item in markdown.events() {
                    if let Event::WikiLink(target) = &item.event {
                        links.push(target.to_string());
                    }
                }
            }
            Node::Box { children, .. }
            | Node::Center { children }
            | Node::Right { children }
            | Node::Indent { children, .. }
            | Node::Columns { children, .. }
            | Node::Column { children, .. } => collect_node_wiki_links(children, links),
        }
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
    fn scanner_collects_wiki_links_only() {
        let text = "see [[Project]] and [[Project]] again\n\
                    ```text\n\
                    [[fenced]]\n\
                    ```\n\
                    inline `[[coded]]` and <!-- [[commented]] -->\n\
                    escaped \\[[escaped]]\n\
                    embed ![[assets/pic.png]]\n\
                    bare [[ in prose\n";
        assert_eq!(
            find_wiki_links(text),
            vec!["Project".to_string(), "Project".to_string()]
        );
    }

    #[test]
    fn index_counts_shared_references_across_managed_groups() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        fs::write(
            storage.daily_dir.join("2026-07-28.md"),
            "daily [[Project]]\n",
        )
        .unwrap();
        fs::write(
            storage.data_dir.join("Note.mb"),
            "[[Project]] and [[Other]]\n",
        )
        .unwrap();
        fs::write(storage.archives_dir.join("Old.md"), "[[Project]] again\n").unwrap();

        let index = WikiLinkIndex::build(&storage);
        assert_eq!(index.reference_count("Project"), 3);
        assert_eq!(index.locations("Project").len(), 3);
        assert_eq!(index.reference_count("Other"), 1);
        assert_eq!(index.locations("Other").len(), 1);
        assert!(index.is_referenced("Project"));
        assert!(!index.is_referenced("Missing"));
    }

    #[test]
    fn count_tracks_occurrences_and_locations_track_distinct_notes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        fs::write(
            storage.data_dir.join("Note.md"),
            "[[Project]] twice [[Project]]\n",
        )
        .unwrap();
        fs::write(storage.daily_dir.join("2026-07-28.md"), "[[Project]]\n").unwrap();

        let index = WikiLinkIndex::build(&storage);
        assert_eq!(
            index.reference_count("Project"),
            3,
            "occurrences, not notes"
        );
        assert_eq!(index.locations("Project").len(), 2, "distinct notes");
    }

    #[test]
    fn backlinks_match_by_name_and_stem_case_insensitively_and_exclude_self() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let note = storage.data_dir.join("Project.md");
        fs::write(&note, "own [[Project]] self-link\n").unwrap();
        fs::write(storage.data_dir.join("A.md"), "[[Project]] exact\n").unwrap();
        fs::write(
            storage.data_dir.join("B.mb"),
            "[[project]] case-insensitive\n",
        )
        .unwrap();
        fs::write(
            storage.data_dir.join("C.md"),
            "[[Project.md]] with extension\n",
        )
        .unwrap();
        fs::write(
            storage.data_dir.join("Other.md"),
            "[[Different]] no match\n",
        )
        .unwrap();

        let index = WikiLinkIndex::build(&storage);
        assert_eq!(
            index.backlinks(&note),
            vec![
                storage.data_dir.join("A.md"),
                storage.data_dir.join("B.mb"),
                storage.data_dir.join("C.md"),
            ],
            "self-link excluded, stem and case-insensitive matches included, sorted"
        );
        assert!(index
            .backlinks(&storage.data_dir.join("Missing.md"))
            .is_empty());
    }

    #[test]
    fn refresh_paths_reindexes_created_modified_and_removed_files() {
        let directory = tempfile::tempdir().unwrap();
        let storage = storage_with(&directory);
        let note = storage.data_dir.join("Note.md");
        fs::write(&note, "[[Target]]\n").unwrap();
        let mut index = WikiLinkIndex::build(&storage);
        assert_eq!(index.reference_count("Target"), 1);

        fs::write(&note, "[[Changed]]\n").unwrap();
        index.refresh_paths(&storage, std::slice::from_ref(&note));
        assert_eq!(index.reference_count("Target"), 0);
        assert_eq!(index.reference_count("Changed"), 1);

        fs::remove_file(&note).unwrap();
        index.refresh_paths(&storage, &[note]);
        assert!(!index.is_referenced("Changed"));
    }
}
