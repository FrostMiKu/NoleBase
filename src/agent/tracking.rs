//! Tracks which file ranges the model has already read, to gate `edit_file`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Result};

#[derive(Default)]
pub(crate) struct ReadTracker {
    files: Mutex<HashMap<PathBuf, FileReadState>>,
}

#[derive(Clone)]
pub(crate) struct FileReadState {
    pub(crate) snapshot: String,
    ranges: Vec<(usize, usize)>,
    total_lines: usize,
}

impl ReadTracker {
    pub(crate) fn clear(&self) -> Result<()> {
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("file read tracker lock poisoned"))?
            .clear();
        Ok(())
    }

    pub(crate) fn invalidate(&self, path: &Path) -> Result<()> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("file read tracker lock poisoned"))?
            .retain(|tracked, _| !tracked.starts_with(&path));
        Ok(())
    }

    pub(crate) fn mark_file(
        &self,
        path: PathBuf,
        content: String,
        start: usize,
        end: usize,
        total_lines: usize,
    ) -> Result<()> {
        let mut files = self
            .files
            .lock()
            .map_err(|_| anyhow::anyhow!("file read tracker lock poisoned"))?;
        let state = files.entry(path).or_insert_with(|| FileReadState {
            snapshot: content.clone(),
            ranges: Vec::new(),
            total_lines,
        });
        if state.snapshot != content || state.total_lines != total_lines {
            *state = FileReadState {
                snapshot: content,
                ranges: Vec::new(),
                total_lines,
            };
        }
        if start < end {
            state.ranges.push((start, end));
            state.ranges.sort_unstable_by_key(|range| range.0);
            let mut merged: Vec<(usize, usize)> = Vec::with_capacity(state.ranges.len());
            for range in state.ranges.drain(..) {
                if let Some(last) = merged.last_mut().filter(|last| range.0 <= last.1) {
                    last.1 = last.1.max(range.1);
                } else {
                    merged.push(range);
                }
            }
            state.ranges = merged;
        }
        Ok(())
    }

    pub(crate) fn file_state(&self, path: &Path) -> Result<Option<FileReadState>> {
        let files = self
            .files
            .lock()
            .map_err(|_| anyhow::anyhow!("file read tracker lock poisoned"))?;
        Ok(files.get(path).cloned())
    }

    pub(crate) fn consume_file(&self, path: &Path) -> Result<()> {
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("file read tracker lock poisoned"))?
            .remove(path);
        Ok(())
    }
}

impl FileReadState {
    pub(crate) fn covers(&self, start: usize, end: usize) -> bool {
        start == end
            || self
                .ranges
                .iter()
                .any(|range| range.0 <= start && range.1 >= end)
    }

    pub(crate) fn ensure_edit_read(&self, start_line: usize, end_line: usize) -> Result<()> {
        if start_line < end_line {
            if !self.covers(start_line, end_line) {
                let last_line = end_line.saturating_sub(1);
                bail!(
                    "edit_file must read changed zero-based lines {start_line} through {last_line} first"
                );
            }
        } else if self.total_lines > 0 {
            let anchor_start = start_line.saturating_sub(1);
            let anchor_end = (start_line + 1).min(self.total_lines);
            if !self.covers(anchor_start, anchor_end) {
                bail!(
                    "edit_file must read insertion anchor lines {anchor_start}..{anchor_end} first"
                );
            }
        }
        Ok(())
    }
}
