//! Document format detection, Markdown extraction, and paginated-read cache.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use tokio::sync::{Mutex, OnceCell};

const MAX_CACHE_ENTRIES: usize = 16;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_MARKDOWN_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DocumentFormat(anydoc::Format);

impl DocumentFormat {
    pub(super) fn from_path(path: &Path) -> Option<Self> {
        anydoc::Format::from_path(path).map(Self)
    }

    pub(super) fn from_bytes_or_path(bytes: &[u8], path: &Path) -> Option<Self> {
        anydoc::Format::from_bytes(bytes)
            .or_else(|| anydoc::Format::from_path(path))
            .map(Self)
    }

    pub(super) fn label(self) -> &'static str {
        match self.0 {
            anydoc::Format::Doc => "doc",
            anydoc::Format::Docx => "docx",
            anydoc::Format::Odt => "odt",
            anydoc::Format::Pdf => "pdf",
            anydoc::Format::Ppt => "ppt",
            anydoc::Format::Pptx => "pptx",
            anydoc::Format::Rtf => "rtf",
            anydoc::Format::Epub => "epub",
            anydoc::Format::Excel => "excel",
            anydoc::Format::Ods => "ods",
            anydoc::Format::Odp => "odp",
            anydoc::Format::Csv => "csv",
        }
    }
}

#[derive(Clone, Debug, Eq)]
pub(super) enum DocumentSourceKey {
    Local {
        path: PathBuf,
        len: u64,
        modified: Option<SystemTime>,
    },
    Attachment {
        id: String,
        len: u64,
        modified: Option<SystemTime>,
    },
    Content([u8; 32]),
}

impl PartialEq for DocumentSourceKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Local {
                    path: a,
                    len: al,
                    modified: am,
                },
                Self::Local {
                    path: b,
                    len: bl,
                    modified: bm,
                },
            ) => a == b && al == bl && am == bm,
            (
                Self::Attachment {
                    id: a,
                    len: al,
                    modified: am,
                },
                Self::Attachment {
                    id: b,
                    len: bl,
                    modified: bm,
                },
            ) => a == b && al == bl && am == bm,
            (Self::Content(a), Self::Content(b)) => a == b,
            _ => false,
        }
    }
}

impl Hash for DocumentSourceKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Local {
                path,
                len,
                modified,
            } => {
                path.hash(state);
                len.hash(state);
                modified.hash(state);
            }
            Self::Attachment { id, len, modified } => {
                id.hash(state);
                len.hash(state);
                modified.hash(state);
            }
            Self::Content(digest) => digest.hash(state),
        }
    }
}

struct CacheEntry {
    value: OnceCell<Arc<ExtractedDocument>>,
    accounted: AtomicBool,
}

impl CacheEntry {
    fn new() -> Self {
        Self {
            value: OnceCell::new(),
            accounted: AtomicBool::new(false),
        }
    }
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<DocumentSourceKey, Arc<CacheEntry>>,
    lru: VecDeque<DocumentSourceKey>,
    bytes: usize,
}

#[derive(Clone, Default)]
pub(super) struct DocumentCache(Arc<Mutex<CacheState>>);

#[derive(Debug)]
pub(super) struct ExtractedDocument {
    pub(super) format: DocumentFormat,
    pub(super) markdown: Arc<str>,
}

impl DocumentCache {
    pub(super) async fn get_or_extract<F, Fut>(
        &self,
        key: DocumentSourceKey,
        extract: F,
    ) -> Result<Arc<ExtractedDocument>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<ExtractedDocument>>,
    {
        let entry = {
            let mut state = self.0.lock().await;
            touch(&mut state.lru, &key);
            if let Some(entry) = state.entries.get(&key) {
                entry.clone()
            } else {
                if state.entries.len() >= MAX_CACHE_ENTRIES {
                    evict_oldest_completed(&mut state, Some(&key));
                }
                let entry = Arc::new(CacheEntry::new());
                state.entries.insert(key.clone(), entry.clone());
                entry
            }
        };

        let value = match entry
            .value
            .get_or_try_init(|| async { Ok::<_, anyhow::Error>(Arc::new(extract().await?)) })
            .await
        {
            Ok(value) => value.clone(),
            Err(error) => {
                let mut state = self.0.lock().await;
                if state
                    .entries
                    .get(&key)
                    .is_some_and(|cached| Arc::ptr_eq(cached, &entry))
                {
                    state.entries.remove(&key);
                    state.lru.retain(|candidate| candidate != &key);
                }
                return Err(error);
            }
        };

        if !entry.accounted.swap(true, Ordering::AcqRel) {
            let mut state = self.0.lock().await;
            state.bytes = state.bytes.saturating_add(value.markdown.len());
            while (state.bytes > MAX_CACHE_BYTES || state.entries.len() > MAX_CACHE_ENTRIES)
                && state.entries.len() > 1
            {
                if !evict_oldest_completed(&mut state, Some(&key)) {
                    break;
                }
            }
        }
        Ok(value)
    }
}

fn touch(lru: &mut VecDeque<DocumentSourceKey>, key: &DocumentSourceKey) {
    lru.retain(|candidate| candidate != key);
    lru.push_back(key.clone());
}

fn evict_oldest_completed(state: &mut CacheState, preserve: Option<&DocumentSourceKey>) -> bool {
    let attempts = state.lru.len();
    for _ in 0..attempts {
        let Some(key) = state.lru.pop_front() else {
            return false;
        };
        if preserve.is_some_and(|preserve| preserve == &key) {
            state.lru.push_back(key);
            continue;
        }
        let Some(entry) = state.entries.get(&key) else {
            continue;
        };
        let Some(value) = entry.value.get() else {
            state.lru.push_back(key);
            continue;
        };
        state.bytes = state.bytes.saturating_sub(value.markdown.len());
        state.entries.remove(&key);
        return true;
    }
    false
}

pub(super) async fn extract_markdown(
    bytes: Vec<u8>,
    format: DocumentFormat,
) -> Result<ExtractedDocument> {
    tokio::task::spawn_blocking(move || {
        let markdown = anydoc::to_markdown_bytes(&bytes, format.0)
            .map_err(anyhow::Error::new)
            .with_context(|| format!("extracting {} document", format.label()))?;
        if markdown.len() > MAX_MARKDOWN_BYTES {
            anyhow::bail!("extracted document exceeds the {MAX_MARKDOWN_BYTES} byte limit");
        }
        Ok(ExtractedDocument {
            format,
            markdown: Arc::from(markdown),
        })
    })
    .await
    .context("joining document extraction")?
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test]
    async fn concurrent_pages_share_one_extraction() {
        let cache = DocumentCache::default();
        let key = DocumentSourceKey::Content([7; 32]);
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first = {
            let cache = cache.clone();
            let key = key.clone();
            let calls = calls.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                cache
                    .get_or_extract(key, || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        entered.notify_one();
                        release.notified().await;
                        Ok(ExtractedDocument {
                            format: DocumentFormat(anydoc::Format::Docx),
                            markdown: Arc::from("page one\npage two"),
                        })
                    })
                    .await
                    .unwrap()
            })
        };
        entered.notified().await;
        let second = {
            let cache = cache.clone();
            let calls = calls.clone();
            tokio::spawn(async move {
                cache
                    .get_or_extract(key, || async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(ExtractedDocument {
                            format: DocumentFormat(anydoc::Format::Docx),
                            markdown: Arc::from("unexpected"),
                        })
                    })
                    .await
                    .unwrap()
            })
        };
        tokio::task::yield_now().await;
        release.notify_one();

        assert_eq!(&*first.await.unwrap().markdown, "page one\npage two");
        assert_eq!(&*second.await.unwrap().markdown, "page one\npage two");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn changed_identity_is_extracted_again() {
        let cache = DocumentCache::default();
        for (digest, expected) in [(1, "first"), (2, "second")] {
            let value = cache
                .get_or_extract(DocumentSourceKey::Content([digest; 32]), || async move {
                    Ok(ExtractedDocument {
                        format: DocumentFormat(anydoc::Format::Docx),
                        markdown: Arc::from(expected),
                    })
                })
                .await
                .unwrap();
            assert_eq!(&*value.markdown, expected);
        }
    }
}
