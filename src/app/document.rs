//! Center-pane documents and the cross-document Markdown render cache.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;

pub(crate) const DOCUMENT_CACHE_CAPACITY: usize = 8;
pub(crate) const DOCUMENT_CACHE_MAX_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentKind {
    File(PathBuf),
    Daily(NaiveDate),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentReturn {
    Daily,
    Search,
}

/// A regular or daily note rendered as Markdown in the center pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub kind: DocumentKind,
    pub title: String,
    pub source: String,
    pub scroll: u16,
    /// One-based source line to reveal on the next render.
    pub target_line: Option<usize>,
    pub return_to: DocumentReturn,
    pub(crate) render_cache: Option<DocumentRenderCache>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocumentRenderCache {
    pub width: usize,
    pub rendered: crate::markdown::RenderedMarkup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CachedDocumentRender {
    pub(super) kind: DocumentKind,
    pub(super) source: String,
    pub(super) render: DocumentRenderCache,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DocumentRenderLru {
    pub(super) entries: VecDeque<CachedDocumentRender>,
}

impl DocumentRenderLru {
    pub(super) fn insert(
        &mut self,
        kind: DocumentKind,
        source: String,
        render: DocumentRenderCache,
    ) {
        self.remove(&kind);
        self.entries.push_front(CachedDocumentRender {
            kind,
            source,
            render,
        });
        while self.entries.len() > DOCUMENT_CACHE_CAPACITY
            || (self.entries.len() > 1 && self.approximate_cells() > DOCUMENT_CACHE_MAX_CELLS)
        {
            self.entries.pop_back();
        }
    }

    pub(super) fn take(
        &mut self,
        kind: &DocumentKind,
        source: &str,
    ) -> Option<DocumentRenderCache> {
        let index = self.entries.iter().position(|entry| &entry.kind == kind)?;
        let entry = self.entries.remove(index)?;
        (entry.source == source).then_some(entry.render)
    }

    pub(super) fn remove(&mut self, kind: &DocumentKind) {
        self.entries.retain(|entry| &entry.kind != kind);
    }

    pub(super) fn retarget_file(&mut self, from: &Path, to: &Path) {
        for entry in &mut self.entries {
            if matches!(&entry.kind, DocumentKind::File(path) if path == from) {
                entry.kind = DocumentKind::File(to.to_path_buf());
            }
        }
    }

    pub(super) fn approximate_cells(&self) -> usize {
        self.entries.iter().fold(0usize, |total, entry| {
            total.saturating_add(
                entry
                    .render
                    .width
                    .saturating_mul(entry.render.rendered.lines.len()),
            )
        })
    }
}

impl Document {
    pub(crate) fn replace_source(&mut self, source: String) {
        if self.source != source {
            self.source = source;
            self.render_cache = None;
        }
    }

    pub(crate) fn ensure_rendered(&mut self, width: usize, theme: crate::theme::Theme) -> bool {
        if self
            .render_cache
            .as_ref()
            .is_some_and(|cache| cache.width == width)
        {
            return false;
        }
        self.render_cache = Some(DocumentRenderCache {
            width,
            rendered: crate::markdown::render_at_width(&self.source, width, theme),
        });
        true
    }
}
