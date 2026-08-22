use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use mbdown::{Event, Node};

use crate::document_index::{
    self, DocumentGroup as FileGroup, DocumentIndex, IndexedDocument, IndexedLine,
};
use crate::model::SearchHit;
use crate::storage::Storage;

const SEARCH_RESULT_CAP: usize = 200;

#[derive(Clone, Debug, Default)]
pub struct WorkspaceIndex {
    files: HashMap<PathBuf, IndexedFile>,
    tags: HashMap<String, TagEntry>,
}

#[derive(Clone, Debug)]
struct IndexedFile {
    group: FileGroup,
    modified: SystemTime,
    lines: Vec<IndexedLine>,
}

#[derive(Clone, Debug, Default)]
struct TagEntry {
    display: String,
    occurrences: Vec<TagOccurrence>,
}

#[derive(Clone, Debug)]
struct TagOccurrence {
    path: PathBuf,
    line_no: usize,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagSummary {
    pub name: String,
    pub documents: usize,
    pub mentions: usize,
}

/// One distinct managed document containing an exact tag, as reported by
/// [`WorkspaceIndex::tag_documents`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagDocument {
    pub path: PathBuf,
    pub modified: SystemTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagScope {
    Daily,
    Notes,
    Archives,
}

impl TagScope {
    fn includes(self, group: FileGroup) -> bool {
        matches!(
            (self, group),
            (Self::Daily, FileGroup::Daily)
                | (Self::Notes, FileGroup::Notes)
                | (Self::Archives, FileGroup::Archives)
        )
    }
}

#[derive(Clone, Default)]
pub struct WorkspaceIndexHandle(Arc<RwLock<Option<WorkspaceIndex>>>);

impl WorkspaceIndexHandle {
    pub fn replace(&self, index: WorkspaceIndex) {
        if let Ok(mut current) = self.0.write() {
            *current = Some(index);
        }
    }

    pub fn with_index<T>(&self, f: impl FnOnce(&WorkspaceIndex) -> T) -> Option<T> {
        let current = self.0.read().ok()?;
        current.as_ref().map(f)
    }

    pub fn refresh_paths(&self, storage: &Storage, paths: Vec<PathBuf>) {
        if let Ok(mut current) = self.0.write() {
            if let Some(index) = current.as_mut() {
                apply_paths(storage, index, paths);
            }
        }
    }
}

#[derive(Debug)]
pub struct TagRenamePlan {
    pub from: String,
    pub to: String,
    changes: Vec<TagFileChange>,
}

#[derive(Debug)]
struct TagFileChange {
    path: PathBuf,
    before: String,
    after: String,
    mentions: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagRenameOutcome {
    pub from: String,
    pub to: String,
    pub documents: usize,
    pub mentions: usize,
    pub paths: Vec<PathBuf>,
}

impl WorkspaceIndex {
    #[cfg(test)]
    pub(crate) fn build(storage: &Storage) -> Self {
        build_index(storage)
    }

    pub(crate) fn from_documents(documents: &DocumentIndex) -> Self {
        let files = documents
            .iter()
            .map(|(path, document)| {
                (
                    path.clone(),
                    IndexedFile {
                        group: document.group,
                        modified: document.modified(),
                        lines: document.lines.clone(),
                    },
                )
            })
            .collect();
        let mut index = Self {
            files,
            tags: HashMap::new(),
        };
        index.rebuild_tags();
        index
    }

    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        if let Some(tag) = exact_tag_query(query) {
            return self
                .search_tag(&tag, None)
                .into_iter()
                .take(SEARCH_RESULT_CAP)
                .collect();
        }

        let query = query.to_lowercase();
        let mut hits = Vec::new();
        for (path, file) in self.sorted_files() {
            for line in &file.lines {
                if line.lowercase.contains(&query) {
                    hits.push(SearchHit::FileLine {
                        path: path.clone(),
                        line_no: line.line_no,
                        text: line.text.clone(),
                    });
                    if hits.len() >= SEARCH_RESULT_CAP {
                        return hits;
                    }
                }
            }
        }
        hits
    }

    pub fn tags(&self) -> Vec<TagSummary> {
        self.tags_scoped(None)
    }

    pub fn tags_scoped(&self, scope: Option<TagScope>) -> Vec<TagSummary> {
        let mut tags = self
            .tags
            .values()
            .filter_map(|entry| {
                let occurrences = entry
                    .occurrences
                    .iter()
                    .filter(|occurrence| {
                        scope.is_none_or(|scope| {
                            self.files
                                .get(&occurrence.path)
                                .is_some_and(|file| scope.includes(file.group))
                        })
                    })
                    .collect::<Vec<_>>();
                (!occurrences.is_empty()).then(|| TagSummary {
                    name: entry.display.clone(),
                    documents: occurrences
                        .iter()
                        .map(|occurrence| &occurrence.path)
                        .collect::<HashSet<_>>()
                        .len(),
                    mentions: occurrences.len(),
                })
            })
            .collect::<Vec<_>>();
        tags.sort_by(|left, right| {
            right
                .documents
                .cmp(&left.documents)
                .then_with(|| right.mentions.cmp(&left.mentions))
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        tags
    }

    pub fn exact_tag_hits(&self, tag: &str, scope: Option<TagScope>) -> Vec<SearchHit> {
        let normalized = normalize_tag(tag.trim().strip_prefix('#').unwrap_or(tag.trim()));
        self.search_tag(&normalized, scope)
    }

    /// Every distinct managed document containing the exact tag, ordered
    /// oldest-to-newest by filesystem modified time with the path as the
    /// tie-breaker (matching Daily's chronological direction).
    pub fn tag_documents(&self, tag: &str) -> Vec<TagDocument> {
        let normalized = normalize_tag(tag.trim().strip_prefix('#').unwrap_or(tag.trim()));
        let Some(entry) = self.tags.get(&normalized) else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut documents = Vec::new();
        for occurrence in &entry.occurrences {
            if !seen.insert(&occurrence.path) {
                continue;
            }
            let Some(file) = self.files.get(&occurrence.path) else {
                continue;
            };
            documents.push(TagDocument {
                path: occurrence.path.clone(),
                modified: file.modified,
            });
        }
        documents.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.path.cmp(&right.path))
        });
        documents
    }

    pub fn tag_paths(&self, tag: &str) -> Vec<PathBuf> {
        let normalized = normalize_tag(tag.trim().strip_prefix('#').unwrap_or(tag.trim()));
        let Some(entry) = self.tags.get(&normalized) else {
            return Vec::new();
        };
        let mut paths = entry
            .occurrences
            .iter()
            .map(|occurrence| occurrence.path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn search_tag(&self, normalized: &str, scope: Option<TagScope>) -> Vec<SearchHit> {
        let Some(entry) = self.tags.get(normalized) else {
            return Vec::new();
        };
        let mut occurrences = entry
            .occurrences
            .iter()
            .filter(|occurrence| {
                scope.is_none_or(|scope| {
                    self.files
                        .get(&occurrence.path)
                        .is_some_and(|file| scope.includes(file.group))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        occurrences.sort_by(|left, right| {
            self.compare_paths(&left.path, &right.path)
                .then_with(|| left.line_no.cmp(&right.line_no))
        });
        occurrences
            .dedup_by(|left, right| left.path == right.path && left.line_no == right.line_no);
        occurrences
            .into_iter()
            .map(|occurrence| SearchHit::FileLine {
                path: occurrence.path,
                line_no: occurrence.line_no,
                text: occurrence.text,
            })
            .collect()
    }

    fn sorted_files(&self) -> Vec<(&PathBuf, &IndexedFile)> {
        let mut files = self.files.iter().collect::<Vec<_>>();
        files.sort_by(|(left_path, left), (right_path, right)| {
            left.group
                .cmp(&right.group)
                .then_with(|| right.modified.cmp(&left.modified))
                .then_with(|| left_path.file_name().cmp(&right_path.file_name()))
        });
        files
    }

    fn compare_paths(&self, left: &Path, right: &Path) -> std::cmp::Ordering {
        let left_file = self.files.get(left).expect("tag path is indexed");
        let right_file = self.files.get(right).expect("tag path is indexed");
        left_file
            .group
            .cmp(&right_file.group)
            .then_with(|| right_file.modified.cmp(&left_file.modified))
            .then_with(|| left.file_name().cmp(&right.file_name()))
    }

    fn replace_path(&mut self, storage: &Storage, path: &Path) {
        self.files.remove(path);
        if let Some(file) = index_file(storage, path) {
            self.files.insert(path.to_path_buf(), file);
        }
    }

    fn rebuild_tags(&mut self) {
        self.tags.clear();
        for (path, file) in &self.files {
            for line in &file.lines {
                for tag in &line.tags {
                    let normalized = normalize_tag(tag);
                    let entry = self.tags.entry(normalized).or_default();
                    if entry.display.is_empty() || tag < &entry.display {
                        entry.display = tag.clone();
                    }
                    entry.occurrences.push(TagOccurrence {
                        path: path.clone(),
                        line_no: line.line_no,
                        text: line.text.clone(),
                    });
                }
            }
        }
    }
}

impl TagRenamePlan {
    pub fn prepare(storage: &Storage, paths: Vec<PathBuf>, from: &str, to: &str) -> Result<Self> {
        let from = valid_tag_name(from)?;
        let to = valid_tag_name(to)?;
        if normalize_tag(&from) == normalize_tag(&to) {
            bail!("source and destination tags are the same");
        }

        let mut changes = Vec::new();
        for path in paths {
            if file_group(storage, &path).is_none() {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("checking {}", path.display()))?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            let before =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            let spans = matching_tag_spans(&before, &from)?;
            if spans.is_empty() {
                continue;
            }
            let after = replace_tag_spans(&before, &spans, &to);
            changes.push(TagFileChange {
                path,
                before,
                after,
                mentions: spans.len(),
            });
        }
        if changes.is_empty() {
            bail!("no managed note contains tag #{from}");
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Self { from, to, changes })
    }

    pub fn documents(&self) -> usize {
        self.changes.len()
    }

    pub fn mentions(&self) -> usize {
        self.changes.iter().map(|change| change.mentions).sum()
    }

    pub fn changes(&self) -> impl Iterator<Item = (&Path, &str, &str, usize)> {
        self.changes.iter().map(|change| {
            (
                change.path.as_path(),
                change.before.as_str(),
                change.after.as_str(),
                change.mentions,
            )
        })
    }

    pub fn apply(self) -> Result<TagRenameOutcome> {
        for change in &self.changes {
            let current = fs::read_to_string(&change.path)
                .with_context(|| format!("rechecking {}", change.path.display()))?;
            if current != change.before {
                bail!(
                    "{} changed while the tag rename was being reviewed",
                    change.path.display()
                );
            }
        }
        for change in &self.changes {
            fs::write(&change.path, &change.after)
                .with_context(|| format!("updating {}", change.path.display()))?;
        }
        let documents = self.documents();
        let mentions = self.mentions();
        Ok(TagRenameOutcome {
            from: self.from,
            to: self.to,
            documents,
            mentions,
            paths: self.changes.into_iter().map(|change| change.path).collect(),
        })
    }
}

fn apply_paths(storage: &Storage, index: &mut WorkspaceIndex, paths: Vec<PathBuf>) {
    let mut changed = false;
    for path in paths.into_iter().collect::<HashSet<_>>() {
        if file_group(storage, &path).is_some() {
            index.replace_path(storage, &path);
            changed = true;
        }
    }
    if changed {
        index.rebuild_tags();
    }
}
#[cfg(test)]
fn build_index(storage: &Storage) -> WorkspaceIndex {
    WorkspaceIndex::from_documents(&DocumentIndex::build(storage))
}

fn index_file(storage: &Storage, path: &Path) -> Option<IndexedFile> {
    document_index::index_file(storage, path).map(indexed_file)
}

fn indexed_file(document: IndexedDocument) -> IndexedFile {
    IndexedFile {
        group: document.group,
        modified: document.modified(),
        lines: document.lines,
    }
}

pub(crate) fn collect_document_tags(
    nodes: &[Node<'_>],
    source: &str,
    tags_by_line: &mut HashMap<usize, Vec<String>>,
) {
    for node in nodes {
        match node {
            Node::Markdown(markdown) => {
                for event in markdown.events() {
                    if let Event::Hashtag(tag) = &event.event {
                        let document_offset = markdown.source_span().start + event.span.start;
                        let line_no = source[..document_offset]
                            .bytes()
                            .filter(|byte| *byte == b'\n')
                            .count()
                            + 1;
                        tags_by_line
                            .entry(line_no)
                            .or_default()
                            .push(tag.to_string());
                    }
                }
            }
            Node::Box { children, .. }
            | Node::Center { children }
            | Node::Right { children }
            | Node::Indent { children, .. }
            | Node::Columns { children, .. }
            | Node::Column { children, .. } => {
                collect_document_tags(children, source, tags_by_line)
            }
        }
    }
}

fn valid_tag_name(value: &str) -> Result<String> {
    let name = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if name.is_empty() {
        bail!("tag name is empty");
    }
    let source = format!("#{name}");
    let document = mbdown::parse(&source).context("validating tag name")?;
    let mut spans = Vec::new();
    collect_matching_tag_spans(document.nodes(), name, &mut spans);
    if spans.len() != 1 || spans[0].start != 0 || spans[0].end != source.len() {
        bail!("invalid tag name: {value}");
    }
    Ok(name.to_string())
}

fn matching_tag_spans(source: &str, tag: &str) -> Result<Vec<Range<usize>>> {
    let document = mbdown::parse(source).context("parsing Markdown for tag rename")?;
    let mut spans = Vec::new();
    collect_matching_tag_spans(document.nodes(), tag, &mut spans);
    spans.sort_by_key(|span| span.start);
    Ok(spans)
}

fn collect_matching_tag_spans(nodes: &[Node<'_>], tag: &str, spans: &mut Vec<Range<usize>>) {
    let normalized = normalize_tag(tag);
    for node in nodes {
        match node {
            Node::Markdown(markdown) => {
                for event in markdown.events() {
                    if let Event::Hashtag(found) = &event.event {
                        if normalize_tag(found) == normalized {
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
            | Node::Column { children, .. } => collect_matching_tag_spans(children, tag, spans),
        }
    }
}

fn replace_tag_spans(source: &str, spans: &[Range<usize>], tag: &str) -> String {
    let mut output = source.to_string();
    for span in spans.iter().rev() {
        output.replace_range(span.clone(), &format!("#{tag}"));
    }
    output
}

fn file_group(storage: &Storage, path: &Path) -> Option<FileGroup> {
    document_index::document_group(storage, path)
}

fn exact_tag_query(query: &str) -> Option<String> {
    let tag = query.strip_prefix('#')?;
    (!tag.is_empty() && !tag.chars().any(char::is_whitespace)).then(|| normalize_tag(tag))
}

fn normalize_tag(tag: &str) -> String {
    tag.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn index_orders_groups_and_supports_exact_unicode_tags() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            storage.daily_dir.join("2026-07-28.md"),
            "daily #开发/日志\n",
        )
        .unwrap();
        fs::write(storage.data_dir.join("Note.md"), "note #开发/日志\n").unwrap();
        fs::write(
            storage.archives_dir.join("Old.md"),
            "archive #开发/日志 and #开发\n",
        )
        .unwrap();

        let index = build_index(&storage);
        let hits = index.search("#开发/日志");
        assert_eq!(hits.len(), 3);
        let paths = hits
            .iter()
            .filter_map(|hit| match hit {
                SearchHit::FileLine { path, .. } => Some(path),
                SearchHit::DocumentLine { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(paths[0].parent(), Some(storage.daily_dir.as_path()));
        assert_eq!(paths[1].parent(), Some(storage.data_dir.as_path()));
        assert_eq!(paths[2].parent(), Some(storage.archives_dir.as_path()));
        assert_eq!(index.search("#开发").len(), 1);

        let tags = index.tags();
        assert_eq!(tags[0].name, "开发/日志");
        assert_eq!(tags[0].documents, 3);
        assert_eq!(tags[0].mentions, 3);
    }

    #[test]
    fn exact_tags_exclude_code_and_escapes_and_count_mentions() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            storage.data_dir.join("Tags.md"),
            "#rust twice #rust\n`#rust` and \\#rust\n#rustlang\n[#2652 (#paren foo:#suffix\n",
        )
        .unwrap();

        let index = build_index(&storage);
        let hits = index.search("#rust");
        assert_eq!(hits.len(), 1, "one result per matching source line");
        assert_eq!(index.search("#rustlang").len(), 1);
        assert!(index.search("#2652").is_empty());
        assert!(index.search("#paren").is_empty());
        assert!(index.search("#suffix").is_empty());
        let rust = index
            .tags()
            .into_iter()
            .find(|tag| tag.name == "rust")
            .unwrap();
        assert_eq!(rust.documents, 1);
        assert_eq!(rust.mentions, 2);
    }

    #[test]
    fn structural_mbdown_tags_keep_document_line_numbers() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("Layout.mb");
        fs::write(&path, "before\n[box width=20]\ninside #layout\n[/box]\n").unwrap();

        assert!(matches!(
            build_index(&storage).search("#layout").as_slice(),
            [SearchHit::FileLine { path: hit_path, line_no: 3, .. }] if hit_path == &path
        ));
    }

    #[test]
    fn scoped_tag_queries_keep_group_counts_and_order() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(storage.daily_dir.join("2026-07-28.md"), "#shared\n").unwrap();
        fs::write(storage.data_dir.join("Note.md"), "#shared #notes\n").unwrap();
        fs::write(storage.archives_dir.join("Old.md"), "#shared\n").unwrap();
        let index = build_index(&storage);

        let notes = index.tags_scoped(Some(TagScope::Notes));
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().all(|tag| tag.documents == 1));
        let hits = index.exact_tag_hits("shared", Some(TagScope::Archives));
        assert!(matches!(
            hits.as_slice(),
            [SearchHit::FileLine { path, .. }] if path.parent() == Some(storage.archives_dir.as_path())
        ));
    }

    #[test]
    fn tag_rename_uses_ast_spans_across_all_managed_groups() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let daily = storage.daily_dir.join("2026-07-28.md");
        let note = storage.data_dir.join("Note.mb");
        let archive = storage.archives_dir.join("Old.md");
        fs::write(&daily, "#Rust and #rust\n`#rust` \\#rust #rustlang\n").unwrap();
        fs::write(&note, "[box width=20]\n#RUST\n[/box]\n").unwrap();
        fs::write(&archive, "archive #rust\n").unwrap();
        let index = build_index(&storage);
        let plan = TagRenamePlan::prepare(&storage, index.tag_paths("#rust"), "#rust", "语言/rust")
            .unwrap();
        assert_eq!(plan.documents(), 3);
        assert_eq!(plan.mentions(), 4);
        let outcome = plan.apply().unwrap();
        assert_eq!(outcome.paths.len(), 3);
        assert_eq!(
            fs::read_to_string(daily).unwrap(),
            "#语言/rust and #语言/rust\n`#rust` \\#rust #rustlang\n"
        );
        assert!(fs::read_to_string(note).unwrap().contains("#语言/rust"));
        assert!(fs::read_to_string(archive).unwrap().contains("#语言/rust"));
    }

    #[test]
    fn tag_documents_are_distinct_oldest_first_and_exact() {
        fn set_modified(path: &Path, seconds: u64) {
            let file = fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_times(
                fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
            )
            .unwrap();
        }

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let daily = storage.daily_dir.join("2026-07-28.md");
        let note = storage.data_dir.join("Note.md");
        let other = storage.data_dir.join("Other.md");
        let archive = storage.archives_dir.join("Old.md");
        fs::write(&daily, "daily #rust twice #rust\n").unwrap();
        fs::write(&note, "note #RUST\n").unwrap();
        fs::write(&other, "note #rustlang\n").unwrap();
        fs::write(&archive, "archive #rust\n").unwrap();
        set_modified(&daily, 100);
        set_modified(&note, 300);
        set_modified(&other, 200);
        set_modified(&archive, 100);
        let index = build_index(&storage);

        let documents = index.tag_documents("#rust");
        // The daily note (two mentions, one document) and archive share the
        // oldest mtime; the lexical path tie-breaker puts archives first.
        // Note.md is newest; Other.md carries only #rustlang.
        assert_eq!(
            documents,
            vec![
                TagDocument {
                    path: archive.clone(),
                    modified: UNIX_EPOCH + std::time::Duration::from_secs(100),
                },
                TagDocument {
                    path: daily.clone(),
                    modified: UNIX_EPOCH + std::time::Duration::from_secs(100),
                },
                TagDocument {
                    path: note.clone(),
                    modified: UNIX_EPOCH + std::time::Duration::from_secs(300),
                },
            ]
        );
        assert_eq!(index.tag_documents("rust").len(), 3);
        assert!(index
            .tag_documents("#rustlang")
            .iter()
            .all(|doc| doc.path == other));
        assert!(index.tag_documents("#missing").is_empty());
    }

    #[test]
    fn tag_rename_rejects_invalid_names_and_changed_files() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("Note.md");
        fs::write(&path, "#old\n").unwrap();
        let index = build_index(&storage);
        assert!(
            TagRenamePlan::prepare(&storage, index.tag_paths("old"), "old", "invalid tag").is_err()
        );

        let plan = TagRenamePlan::prepare(&storage, index.tag_paths("old"), "old", "new").unwrap();
        fs::write(&path, "changed #old\n").unwrap();
        assert!(plan.apply().is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "changed #old\n");
    }
}
