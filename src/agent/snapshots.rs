//! Versioned, drift-recovering snapshot store gating `edit`.
//!
//! Each tracked path keeps a small history of normalized-content snapshots.
//! The four-hex FNV tag is the compact model-facing handle shown in
//! `[path#TAG]` headers, while the SHA-256 identity is the authoritative
//! stale-write check. Lines are normalized before hashing (trailing spaces,
//! tabs, and CR stripped, a single `\n` appended) so CRLF versus LF files and
//! trailing whitespace never invalidate an anchor, and full normalized text is
//! retained (bounded) so a later drift can be detected and recovered from.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

/// Largest normalized snapshot text retained per version.
pub(crate) const MAX_SNAPSHOT_TEXT_BYTES: usize = 1024 * 1024;

/// Maximum versions retained per path.
const MAX_SNAPSHOT_VERSIONS: usize = 4;

/// Maximum tracked paths before the least-recently-recorded path is evicted.
const MAX_TRACKED_PATHS: usize = 32;

/// Total normalized text budget across every retained snapshot; older text is
/// dropped to `None` (the version survives) rather than evicting the version.
const MAX_TOTAL_RETAINED_TEXT: usize = 16 * 1024 * 1024;

/// Strips trailing spaces, tabs, and carriage returns from one line, mirroring
/// the upstream `/[ \t\r]+(?=\n|$)/g` normalization: the strip applies before
/// an optional final LF, which is preserved. A trailing `\n` is not part of
/// the strip set; callers hash the normalized line followed by a single
/// `b"\n"`, so line-ending style and a missing final newline never distinguish
/// two snapshots.
pub(crate) fn normalize_hash_line(line: &str) -> Cow<'_, str> {
    let Some(body) = line.strip_suffix('\n') else {
        // No trailing LF: strip trailing spaces, tabs, and CR at the end.
        return Cow::Borrowed(line.trim_end_matches([' ', '\t', '\r']));
    };
    let trimmed = body.trim_end_matches([' ', '\t', '\r']);
    if trimmed.len() == body.len() {
        // Nothing stripped before the LF, so the line is already normalized.
        Cow::Borrowed(line)
    } else {
        Cow::Owned(format!("{trimmed}\n"))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SnapshotTagHasher(u32);

impl Default for SnapshotTagHasher {
    fn default() -> Self {
        Self(0x811c_9dc5)
    }
}

impl SnapshotTagHasher {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0 = bytes.iter().fold(self.0, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
        });
    }

    pub(crate) fn finish(self) -> String {
        format!("{:04X}", (self.0 ^ (self.0 >> 16)) & 0xffff)
    }
}

#[derive(Default)]
pub(crate) struct SnapshotIdentityHasher(Sha256);

impl SnapshotIdentityHasher {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub(crate) fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// One immutable file revision retained for a canonical path, newest first per
/// path history.
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) identity: [u8; 32],
    pub(crate) tag: String,
    pub(crate) total_lines: usize,
    /// Retained normalized text, `None` when it exceeds the size cap or was
    /// evicted by the global text budget.
    pub(crate) text: Option<String>,
    /// Merged, sorted, 0-based half-open `[start, end)` line ranges read from
    /// this exact revision.
    seen: Vec<(usize, usize)>,
}

impl Snapshot {
    /// Whether the 0-based half-open `[start, end)` range was fully read; an
    /// empty range (pure insertion point) is always considered covered.
    pub(crate) fn covers(&self, start: usize, end: usize) -> bool {
        start == end
            || self
                .seen
                .iter()
                .any(|range| range.0 <= start && range.1 >= end)
    }

    /// Gates an edit on prior read coverage of the same revision, reporting the
    /// 1-based lines the model must read first.
    pub(crate) fn ensure_seen(&self, start: usize, end: usize) -> Result<()> {
        if start < end {
            if !self.covers(start, end) {
                bail!("edit must read lines {} through {} first", start + 1, end);
            }
        } else if self.total_lines > 0 {
            let anchor_start = start.saturating_sub(1);
            let anchor_end = (start + 1).min(self.total_lines);
            if !self.covers(anchor_start, anchor_end) {
                bail!(
                    "edit must read lines {} through {} first",
                    anchor_start + 1,
                    anchor_end
                );
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct PathHistory {
    /// Newest version first.
    versions: Vec<Snapshot>,
    /// Monotonic last-record time driving global LRU eviction.
    recorded_at: u64,
}

#[derive(Default)]
struct SnapshotStoreInner {
    history: HashMap<PathBuf, PathHistory>,
    clock: u64,
}

impl SnapshotStoreInner {
    fn retained_text(&self) -> usize {
        self.history
            .values()
            .flat_map(|history| &history.versions)
            .filter_map(|version| version.text.as_ref())
            .map(|text| text.len())
            .sum()
    }

    /// Drops the oldest retained text (`None`) until the total text budget
    /// fits, never dropping a version. Oldest means least-recently-recorded
    /// path first, then the oldest version within the path.
    fn enforce_text_budget(&mut self) {
        while self.retained_text() > MAX_TOTAL_RETAINED_TEXT {
            let oldest = self
                .history
                .iter_mut()
                .flat_map(|(_, history)| {
                    let recorded_at = history.recorded_at;
                    history
                        .versions
                        .iter_mut()
                        .enumerate()
                        .map(move |(index, version)| (recorded_at, index, version))
                })
                .filter(|(_, _, version)| version.text.is_some())
                .min_by_key(|(recorded_at, index, _)| (*recorded_at, *index));
            match oldest {
                Some((_, _, version)) => version.text = None,
                None => break,
            }
        }
    }

    /// Removes the least-recently-recorded path when the path count is over
    /// budget.
    fn enforce_path_budget(&mut self) {
        if self.history.len() <= MAX_TRACKED_PATHS {
            return;
        }
        let oldest = self
            .history
            .iter()
            .min_by_key(|(_, history)| history.recorded_at)
            .map(|(path, _)| path.clone());
        if let Some(path) = oldest {
            self.history.remove(&path);
        }
    }
}

/// Bounded in-memory snapshot records keyed by canonical path.
#[derive(Default)]
pub(crate) struct SnapshotStore {
    inner: Mutex<SnapshotStoreInner>,
}

impl SnapshotStore {
    /// Records one read observation for `path` and returns the tag that
    /// anchors it. Recording identical content again refreshes recency, merges
    /// the seen range into the matching version, promotes it to head, and
    /// returns the same tag; recording different content unshifts a new
    /// version.
    pub(crate) fn record(
        &self,
        path: PathBuf,
        identity: [u8; 32],
        tag: String,
        total_lines: usize,
        text: Option<String>,
        seen: (usize, usize),
    ) -> Result<String> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot store lock poisoned"))?;
        let now = inner.clock.saturating_add(1);
        inner.clock = now;
        let text = text.filter(|content| content.len() <= MAX_SNAPSHOT_TEXT_BYTES);
        let mut seen_range = Vec::new();
        if seen.0 < seen.1 {
            seen_range.push(seen);
        }
        let history = inner
            .history
            .entry(path)
            .or_insert_with(PathHistory::default);
        history.recorded_at = now;

        if let Some(index) = history
            .versions
            .iter()
            .position(|version| version.identity == identity)
        {
            let mut fused = history.versions.remove(index);
            merge_seen(&mut fused.seen, &seen_range);
            history.versions.insert(0, fused);
            return Ok(history.versions[0].tag.clone());
        }

        history.versions.insert(
            0,
            Snapshot {
                identity,
                tag,
                total_lines,
                text,
                seen: seen_range,
            },
        );
        while history.versions.len() > MAX_SNAPSHOT_VERSIONS {
            history.versions.pop();
        }
        let recorded_tag = history.versions[0].tag.clone();
        inner.enforce_text_budget();
        inner.enforce_path_budget();
        Ok(recorded_tag)
    }

    /// The most recently recorded snapshot for `path`.
    pub(crate) fn head(&self, path: &Path) -> Result<Option<Snapshot>> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot store lock poisoned"))?;
        Ok(inner
            .history
            .get(path)
            .and_then(|history| history.versions.first().cloned()))
    }

    /// The most recently recorded snapshot whose tag matches
    /// case-insensitively.
    pub(crate) fn by_tag(&self, path: &Path, tag: &str) -> Result<Option<Snapshot>> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot store lock poisoned"))?;
        Ok(inner.history.get(path).and_then(|history| {
            history
                .versions
                .iter()
                .find(|version| version.tag.eq_ignore_ascii_case(tag))
                .cloned()
        }))
    }

    /// Moves a path's whole history to a new canonical location (used by `MV`);
    /// the destination becomes the most recently recorded path.
    pub(crate) fn relocate(&self, from: &Path, to: &Path) -> Result<()> {
        if from == to {
            return Ok(());
        }
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot store lock poisoned"))?;
        if let Some(mut history) = inner.history.remove(from) {
            let now = inner.clock.saturating_add(1);
            inner.clock = now;
            history.recorded_at = now;
            inner.history.insert(to.to_path_buf(), history);
        }
        Ok(())
    }

    /// Drops the tracked history for `path`.
    pub(crate) fn consume(&self, path: &Path) -> Result<()> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot store lock poisoned"))?
            .history
            .remove(path);
        Ok(())
    }

    /// Drops every tracked path that is `path` or lives under it.
    pub(crate) fn invalidate(&self, path: &Path) -> Result<()> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot store lock poisoned"))?
            .history
            .retain(|tracked, _| !tracked.starts_with(&path));
        Ok(())
    }

    /// Empties every tracked path.
    pub(crate) fn clear(&self) -> Result<()> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("snapshot store lock poisoned"))?
            .history
            .clear();
        Ok(())
    }
}

/// Appends and merges 0-based half-open ranges into a sorted, merged list.
fn merge_seen(seen: &mut Vec<(usize, usize)>, extra: &[(usize, usize)]) {
    let mut merged = std::mem::take(seen);
    merged.extend_from_slice(extra);
    merged.sort_unstable_by_key(|range| range.0);
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(merged.len());
    for range in merged {
        if let Some(last) = out.last_mut().filter(|last| range.0 <= last.1) {
            last.1 = last.1.max(range.1);
        } else {
            out.push(range);
        }
    }
    *seen = out;
}

/// Stable incremental FNV-1a tag folded to the four hexadecimal digits used by
/// hashline anchors, over normalized lines. A full SHA-256 identity remains the
/// authoritative stale-write check; the compact tag is only the model-facing
/// handle.
#[cfg(test)]
pub(crate) fn snapshot_tag(content: &str) -> String {
    let mut hasher = SnapshotTagHasher::default();
    for line in content.lines() {
        hasher.update(normalize_hash_line(line).as_bytes());
        hasher.update(b"\n");
    }
    hasher.finish()
}

#[cfg(test)]
pub(crate) fn snapshot_identity(content: &str) -> [u8; 32] {
    let mut hasher = SnapshotIdentityHasher::default();
    for line in content.lines() {
        hasher.update(normalize_hash_line(line).as_bytes());
        hasher.update(b"\n");
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn record_version(
        store: &SnapshotStore,
        path: &Path,
        content: &str,
        seen: (usize, usize),
    ) -> String {
        store
            .record(
                path.to_path_buf(),
                snapshot_identity(content),
                snapshot_tag(content),
                content.lines().count(),
                None,
                seen,
            )
            .unwrap()
    }

    #[test]
    fn normalize_hash_line_strips_trailing_whitespace_only() {
        assert_eq!(normalize_hash_line("plain\n"), "plain\n");
        assert_eq!(normalize_hash_line("trailing  \t\r\n"), "trailing\n");
        assert_eq!(normalize_hash_line("no newline  \t\r"), "no newline");
        assert_eq!(normalize_hash_line("  \t  \r\n"), "\n");
        assert_eq!(normalize_hash_line("  kept  "), "  kept");
    }

    #[test]
    fn normalization_equivalence_lf_crlf_and_trailing_whitespace() {
        let lf = "first\nsecond  \nthird\n";
        let crlf = "first\r\nsecond  \r\nthird\r\n";
        let trailing_no_newline = "first\nsecond\nthird  \t";
        assert_eq!(snapshot_tag(lf), snapshot_tag(crlf));
        assert_eq!(snapshot_tag(lf), snapshot_tag(trailing_no_newline));
        assert_eq!(snapshot_identity(lf), snapshot_identity(crlf));
        assert_eq!(
            snapshot_identity(lf),
            snapshot_identity(trailing_no_newline)
        );
    }

    #[test]
    fn incremental_hashers_match_whole_content_normalized() {
        let content = "first\nsecond  \nthird\r\n";
        let mut tag = SnapshotTagHasher::default();
        let mut identity = SnapshotIdentityHasher::default();
        for line in content.lines() {
            let normalized = normalize_hash_line(line);
            tag.update(normalized.as_bytes());
            tag.update(b"\n");
            identity.update(normalized.as_bytes());
            identity.update(b"\n");
        }
        assert_eq!(tag.finish(), snapshot_tag(content));
        assert_eq!(identity.finish(), snapshot_identity(content));
    }

    #[test]
    fn record_fuses_identical_content_and_merges_seen() {
        let store = SnapshotStore::default();
        let path = PathBuf::from("/tmp/fuse.md");
        let first = store
            .record(
                path.clone(),
                snapshot_identity("a\nb\n"),
                snapshot_tag("a\nb\n"),
                2,
                Some("a\nb\n".to_string()),
                (0, 1),
            )
            .unwrap();
        let second = store
            .record(
                path.clone(),
                snapshot_identity("a\nb\n"),
                snapshot_tag("a\nb\n"),
                2,
                Some("a\nb\n".to_string()),
                (1, 2),
            )
            .unwrap();
        assert_eq!(first, second);
        let inner = store.inner.lock().unwrap();
        assert_eq!(inner.history[&path].versions.len(), 1);
        let head = inner.history[&path].versions[0].clone();
        assert_eq!(head.text.as_deref(), Some("a\nb\n"));
        assert!(head.covers(0, 2));
        assert!(!head.covers(0, 3));
    }

    #[test]
    fn record_different_content_unshifts_and_retains_older_versions() {
        let store = SnapshotStore::default();
        let path = PathBuf::from("/tmp/versions.md");
        let old = record_version(&store, &path, "old\n", (0, 1));
        let new = record_version(&store, &path, "new\n", (0, 1));
        assert_ne!(old, new);
        assert_eq!(store.head(&path).unwrap().unwrap().tag, new);
        assert_eq!(store.by_tag(&path, &old).unwrap().unwrap().tag, old);
        assert_eq!(store.by_tag(&path, &new).unwrap().unwrap().tag, new);
    }

    #[test]
    fn version_history_is_capped() {
        let store = SnapshotStore::default();
        let path = PathBuf::from("/tmp/cap.md");
        let mut tags = Vec::new();
        for index in 0..=MAX_SNAPSHOT_VERSIONS {
            tags.push(record_version(
                &store,
                &path,
                &format!("content {index}\n"),
                (0, 1),
            ));
        }
        assert!(store.by_tag(&path, &tags[0]).unwrap().is_none());
        for tag in &tags[1..] {
            assert!(store.by_tag(&path, tag).unwrap().is_some());
        }
        assert_eq!(
            store.head(&path).unwrap().unwrap().tag,
            tags[MAX_SNAPSHOT_VERSIONS]
        );
    }

    #[test]
    fn by_tag_is_case_insensitive_and_returns_most_recent() {
        let store = SnapshotStore::default();
        let path = PathBuf::from("/tmp/case.md");
        let first = record_version(&store, &path, "first\n", (0, 1));
        record_version(&store, &path, "second\n", (0, 1));
        assert_eq!(
            store
                .by_tag(&path, &first.to_lowercase())
                .unwrap()
                .unwrap()
                .tag,
            first
        );
        assert_eq!(
            store
                .by_tag(&path, &first.to_uppercase())
                .unwrap()
                .unwrap()
                .tag,
            first
        );
    }

    #[test]
    fn seen_ranges_merge_and_gate_unread_edits() {
        let store = SnapshotStore::default();
        let path = PathBuf::from("/tmp/seen.md");
        store
            .record(
                path.clone(),
                snapshot_identity("a\nb\nc\nd\n"),
                snapshot_tag("a\nb\nc\nd\n"),
                4,
                None,
                (1, 3),
            )
            .unwrap();
        store
            .record(
                path.clone(),
                snapshot_identity("a\nb\nc\nd\n"),
                snapshot_tag("a\nb\nc\nd\n"),
                4,
                None,
                (3, 5),
            )
            .unwrap();
        let head = store.head(&path).unwrap().unwrap();
        assert!(head.covers(1, 5));
        assert!(head.covers(2, 4));
        assert!(!head.covers(0, 2));
        assert!(head.ensure_seen(1, 5).is_ok());
        assert!(head.ensure_seen(2, 4).is_ok());
        let error = head.ensure_seen(0, 2).unwrap_err();
        assert_eq!(error.to_string(), "edit must read lines 1 through 2 first");
    }

    #[test]
    fn ensure_seen_gates_insertion_anchors() {
        let store = SnapshotStore::default();
        let path = PathBuf::from("/tmp/anchor.md");
        store
            .record(
                path.clone(),
                snapshot_identity("a\nb\nc\n"),
                snapshot_tag("a\nb\nc\n"),
                3,
                None,
                (0, 1),
            )
            .unwrap();
        let head = store.head(&path).unwrap().unwrap();
        assert!(head.ensure_seen(0, 0).is_ok());
        let error = head.ensure_seen(1, 1).unwrap_err();
        assert_eq!(error.to_string(), "edit must read lines 1 through 2 first");
        let error = head.ensure_seen(2, 2).unwrap_err();
        assert_eq!(error.to_string(), "edit must read lines 2 through 3 first");
    }

    #[test]
    fn text_over_size_cap_is_not_retained() {
        let store = SnapshotStore::default();
        let path = PathBuf::from("/tmp/oversize.md");
        store
            .record(
                path.clone(),
                snapshot_identity("tiny\n"),
                snapshot_tag("tiny\n"),
                1,
                Some("x".repeat(MAX_SNAPSHOT_TEXT_BYTES + 1)),
                (0, 1),
            )
            .unwrap();
        assert!(store.head(&path).unwrap().unwrap().text.is_none());

        let small = PathBuf::from("/tmp/small.md");
        let small_text = "y".repeat(1024);
        store
            .record(
                small.clone(),
                snapshot_identity("small\n"),
                snapshot_tag("small\n"),
                1,
                Some(small_text.clone()),
                (0, 1),
            )
            .unwrap();
        assert_eq!(
            store.head(&small).unwrap().unwrap().text.as_deref(),
            Some(small_text.as_str())
        );
    }

    #[test]
    fn total_text_budget_evicts_oldest_text_keeping_versions() {
        let store = SnapshotStore::default();
        let chunk = "x".repeat(MAX_SNAPSHOT_TEXT_BYTES);
        // 17 MiB across 17 paths exceeds the 16 MiB budget.
        for index in 0..17 {
            let path = PathBuf::from(format!("/tmp/budget{index}.md"));
            let content = format!("content {index}\n");
            store
                .record(
                    path.clone(),
                    snapshot_identity(&content),
                    snapshot_tag(&content),
                    1,
                    Some(chunk.clone()),
                    (0, 1),
                )
                .unwrap();
        }
        let first_path = PathBuf::from("/tmp/budget0.md");
        let first = store.head(&first_path).unwrap().unwrap();
        assert_eq!(first.identity, snapshot_identity("content 0\n"));
        assert!(first.text.is_none());
        let last_path = PathBuf::from("/tmp/budget16.md");
        let last = store.head(&last_path).unwrap().unwrap();
        assert_eq!(last.text, Some(chunk));
    }

    #[test]
    fn tracked_path_count_is_bounded_by_lru() {
        let store = SnapshotStore::default();
        for index in 0..=MAX_TRACKED_PATHS {
            let path = PathBuf::from(format!("/tmp/lru{index}.md"));
            record_version(&store, &path, &format!("content {index}\n"), (0, 1));
        }
        let evicted = PathBuf::from("/tmp/lru0.md");
        assert!(store.head(&evicted).unwrap().is_none());
        assert!(store
            .head(&PathBuf::from("/tmp/lru32.md"))
            .unwrap()
            .is_some());

        // Re-recording refreshes recency, so the next eviction skips it.
        let refreshed = PathBuf::from("/tmp/lru5.md");
        record_version(&store, &refreshed, "content 5\n", (0, 1));
        let extra = PathBuf::from("/tmp/lru33.md");
        record_version(&store, &extra, "content 33\n", (0, 1));
        assert!(store
            .head(&PathBuf::from("/tmp/lru1.md"))
            .unwrap()
            .is_none());
        assert!(store.head(&refreshed).unwrap().is_some());
    }

    #[test]
    fn relocate_moves_whole_history() {
        let store = SnapshotStore::default();
        let from = PathBuf::from("/tmp/old.md");
        let to = PathBuf::from("/tmp/new.md");
        let old_tag = record_version(&store, &from, "old\n", (0, 1));
        let new_tag = record_version(&store, &from, "new\n", (0, 1));
        assert_ne!(old_tag, new_tag);

        store.relocate(&from, &to).unwrap();
        assert!(store.head(&from).unwrap().is_none());
        assert!(store.by_tag(&from, &old_tag).unwrap().is_none());
        let moved = store.head(&to).unwrap().unwrap();
        assert_eq!(moved.tag, new_tag);
        assert_eq!(store.by_tag(&to, &old_tag).unwrap().unwrap().tag, old_tag);

        // Relocating onto an existing history replaces it wholesale: the
        // moved history (which carries both `new_tag` and `old_tag`)
        // entirely displaces the destination's own versions.
        let existing = PathBuf::from("/tmp/existing.md");
        let existing_tag = record_version(&store, &existing, "existing\n", (0, 1));
        store.relocate(&to, &existing).unwrap();
        assert!(store.by_tag(&existing, &old_tag).unwrap().is_some());
        assert!(store.by_tag(&existing, &new_tag).unwrap().is_some());
        assert!(store.by_tag(&existing, &existing_tag).unwrap().is_none());
    }

    #[test]
    fn invalidate_drops_path_and_descendants() {
        let directory = tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let data = root.join("data");
        fs::create_dir(&data).unwrap();
        let alpha = data.join("alpha.md");
        let beta = data.join("beta.md");
        let config = root.join("config.md");
        let store = SnapshotStore::default();
        for (index, path) in [&alpha, &beta, &config].iter().enumerate() {
            record_version(&store, path, &format!("line {index}\n"), (0, 1));
        }

        store.invalidate(&beta).unwrap();
        assert!(store.head(&beta).unwrap().is_none());
        assert!(store.head(&alpha).unwrap().is_some());
        assert!(store.head(&config).unwrap().is_some());

        store.invalidate(&data).unwrap();
        assert!(store.head(&alpha).unwrap().is_none());
        assert!(store.head(&beta).unwrap().is_none());
        assert!(store.head(&config).unwrap().is_some());

        store.invalidate(&root).unwrap();
        assert!(store.head(&config).unwrap().is_none());
    }

    #[test]
    fn consume_drops_a_path_history() {
        let store = SnapshotStore::default();
        let path = PathBuf::from("/tmp/consume.md");
        record_version(&store, &path, "line\n", (0, 1));
        assert!(store.head(&path).unwrap().is_some());
        store.consume(&path).unwrap();
        assert!(store.head(&path).unwrap().is_none());
    }

    #[test]
    fn clear_empties_everything() {
        let store = SnapshotStore::default();
        let first = PathBuf::from("/tmp/clear1.md");
        let second = PathBuf::from("/tmp/clear2.md");
        record_version(&store, &first, "one\n", (0, 1));
        record_version(&store, &second, "two\n", (0, 1));
        store.clear().unwrap();
        assert!(store.head(&first).unwrap().is_none());
        assert!(store.head(&second).unwrap().is_none());
    }
}
