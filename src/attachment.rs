//! Application-managed attachment storage for Nole.
//!
//! Attachments are mutable note-app files. Each attachment has a stable,
//! opaque identity: a UUID v4 generated at import time. Identity is *not* a
//! digest: importing the same bytes twice produces two distinct attachments,
//! and the content of an attachment may change in place after import (the app
//! opens the real content file, so external saves update the attachment).
//! The canonical URI is `nole://attachment/<uuid>`.
//!
//! Physical layout is private to [`AttachmentStore`] and rooted at
//! `Storage.attachments_dir`. Each attachment is exactly one directory so a
//! trash operation is a single atomic directory rename:
//!
//! - `<uuid>/`                  one directory per attachment
//! - `<uuid>/content.<ext>`     attachment bytes, retaining the display extension
//! - `<uuid>/metadata.json`     JSON metadata (display name, provenance, MIME)
//! - `trash/`               attachment directories moved by `remove`
//!
//! Imports stream the source into a uniquely named staging directory under
//! the root and atomically rename it into place as `<uuid>/`; readers never
//! observe a half-published attachment. Normal attachments are capped at
//! [`MAX_ATTACHMENT_SIZE`] (256 MiB). Malformed ids, symlinks, non-regular
//! sources, and out-of-sync content/metadata pairs are rejected rather than
//! silently repaired. The store never verifies content digests: size is
//! recomputed live from the content file, and content is mutable by design.
//!
//! Content tokens are computed on demand, never verified on every read:
//! [`AttachmentStore::content_token`] hashes the live content and
//! [`AttachmentStore::copy_to_with_token`] hashes the bytes it copies. They
//! are deterministic `sha256:<hex>` optimistic-concurrency keys so an edited
//! workspace copy can be published back in place with stale-content
//! protection ([`AttachmentStore::replace_content`]).
//!
//! Attachment internals are application-managed: the app may open the real
//! content file through [`AttachmentStore::open`], while agent tools only
//! exchange ids, URIs, metadata, tokens, and bounded/streaming reads
//! ([`AttachmentStore::read_limited`], [`AttachmentStore::copy_to_with_token`]).
//! Copying an attachment into the agent workspace remains the explicit way to
//! make a separate editable copy; `update_attachment` publishes it back.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Stable opaque attachment identity: a random UUID v4 generated at import.
///
/// The canonical string form is the lowercase hyphenated UUID
/// (`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`). Only strictly canonical forms
/// can be parsed; uppercase, simple (hyphen-free), URN, and braced spellings
/// are rejected. Identity is deliberately independent of content: the same
/// bytes imported twice produce two different ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttachmentId(Uuid);

impl AttachmentId {
    /// Generate a fresh random (v4) attachment id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parse a strictly canonical id: a lowercase hyphenated UUID. Any other
    /// spelling (uppercase, simple, URN-wrapped, braced) is rejected.
    pub fn parse(input: &str) -> Result<Self> {
        let uuid = Uuid::parse_str(input)
            .with_context(|| format!("attachment id must be a UUID, got {input:?}"))?;
        if uuid.to_string() != input {
            bail!("attachment id must be lowercase hyphenated, got {input:?}");
        }
        Ok(Self(uuid))
    }
}

impl fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for AttachmentId {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        Self::parse(input)
    }
}

impl Serialize for AttachmentId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AttachmentId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Canonical URI of an attachment: `nole://attachment/<uuid>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttachmentUri(AttachmentId);

/// The canonical scheme prefix of every attachment URI, including the
/// trailing slash. `nole://attachment/<uuid>` is the only canonical form.
pub const URI_SCHEME: &str = "nole://attachment/";

impl AttachmentUri {
    /// Strictly parse a canonical attachment URI. Anything other than
    /// `nole://attachment/<lowercase hyphenated UUID>` is rejected.
    pub fn parse(input: &str) -> Result<Self> {
        let id = input
            .strip_prefix(URI_SCHEME)
            .with_context(|| format!("attachment URI must start with {URI_SCHEME}"))?;
        Ok(Self(AttachmentId::parse(id)?))
    }

    /// The attachment identity this URI points at.
    pub fn id(self) -> AttachmentId {
        self.0
    }

    /// Canonical URI for an attachment identity.
    pub fn from_id(id: AttachmentId) -> Self {
        Self(id)
    }

    /// True when `input` starts with the attachment URI scheme, regardless of
    /// whether the rest parses. Used to classify link targets before strict
    /// activation: a malformed `nole://attachment/…` link is still an
    /// attachment link and must fail at activation, never fall through to
    /// another handler.
    pub fn is_attachment_uri(input: &str) -> bool {
        input.starts_with(URI_SCHEME)
    }
}

impl From<AttachmentId> for AttachmentUri {
    fn from(id: AttachmentId) -> Self {
        Self::from_id(id)
    }
}

impl std::str::FromStr for AttachmentUri {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        Self::parse(input)
    }
}

impl fmt::Display for AttachmentUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{URI_SCHEME}{}", self.0)
    }
}

/// Mutable provenance for one attachment, persisted as `metadata.json` inside
/// the attachment's directory. `id` must equal the directory name and the
/// content file must exist next to the metadata; violations are reported as
/// store inconsistency. `size` is never persisted: it is recomputed live from
/// the content file on every load so external edits stay visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentMetadata {
    /// Stable attachment identity; must equal the attachment directory name.
    pub id: AttachmentId,
    /// Byte length of the content file, recomputed live on every load.
    #[serde(skip)]
    pub size: u64,
    /// Validated display name. This is the user-facing name for rendering,
    /// prompts, and Markdown labels; never a path.
    pub display_name: String,
    /// Provenance: the original source path or name provided at import time.
    pub source: String,
    /// Media type detected from content (magic bytes / UTF-8), falling back
    /// to the display name's extension when content is inconclusive.
    pub mime_type: Option<String>,
    /// Import timestamp (RFC 3339, UTC).
    pub imported_at: DateTime<Utc>,
}

impl AttachmentMetadata {
    /// The canonical URI of the attachment this metadata describes.
    pub fn uri(&self) -> AttachmentUri {
        AttachmentUri::from_id(self.id)
    }
}

/// Sort key for [`AttachmentQuery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentSortBy {
    /// Import time; the default order is newest first.
    #[default]
    ImportedAt,
    /// Display name (case-insensitive).
    Name,
    /// Live content size.
    Size,
    /// Media type.
    Type,
}

/// Sort direction for [`AttachmentQuery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentSortOrder {
    /// Ascending.
    Asc,
    /// Descending; the default.
    #[default]
    Desc,
}

/// Default page size returned by [`AttachmentStore::list`].
pub const DEFAULT_LIST_LIMIT: u64 = 50;

/// List query for [`AttachmentStore::list`]: substring filter over display
/// name and provenance, then sort and paginate. Offsets remain an internal
/// store concern; the Agent tool translates its one-based inclusive `range`
/// selector into this query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentQuery {
    /// Case-insensitive substring matched against `display_name` and
    /// `source`. Empty matches everything.
    #[serde(default)]
    pub query: String,
    /// Number of matching attachments to skip.
    #[serde(default)]
    pub offset: u64,
    /// Maximum number of attachments to return.
    #[serde(default = "default_list_limit")]
    pub limit: u64,
    /// Sort key.
    #[serde(default)]
    pub sort_by: AttachmentSortBy,
    /// Sort direction.
    #[serde(default)]
    pub order: AttachmentSortOrder,
}

fn default_list_limit() -> u64 {
    DEFAULT_LIST_LIMIT
}

impl Default for AttachmentQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            offset: 0,
            limit: DEFAULT_LIST_LIMIT,
            sort_by: AttachmentSortBy::ImportedAt,
            order: AttachmentSortOrder::Desc,
        }
    }
}

/// One page of [`AttachmentStore::list`] results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentPage {
    /// Matching attachments for this page.
    pub items: Vec<AttachmentMetadata>,
    /// Total number of matching attachments before pagination.
    pub total: u64,
    /// Offset this page starts at.
    pub offset: u64,
    /// Page size requested.
    pub limit: u64,
    /// True when another page follows this one.
    pub has_more: bool,
}

/// Private on-disk layout for attachments, rooted at `Storage.attachments_dir`.
///
/// All layout knowledge lives here; callers only ever handle ids, uris,
/// metadata, bounded bytes, and streamed copies.
#[derive(Debug, Clone)]
pub struct AttachmentStore {
    root: PathBuf,
    trash_dir: PathBuf,
}

/// Prefix of the content file inside each attachment directory. A safe,
/// bounded display-name extension is appended so system openers select the
/// expected application instead of treating every attachment as plain text.
const CONTENT_FILE_PREFIX: &str = "content";
const MAX_CONTENT_EXTENSION_BYTES: usize = 32;
/// Name of the metadata file inside each attachment directory.
const METADATA_FILE: &str = "metadata.json";
/// Directory under the root that holds trashed attachment directories.
const TRASH_DIR: &str = "trash";
/// Streaming buffer size for imports and copies.
const STREAM_BUFFER: usize = 64 * 1024;
/// Maximum size of a normal attachment, imposed at import and on every read.
pub const MAX_ATTACHMENT_SIZE: u64 = 256 * 1024 * 1024;
/// Bytes sniffed from the content head for MIME detection.
const MIME_SNIFF_BYTES: usize = 8192;
/// Maximum byte length of a display name.
pub const MAX_DISPLAY_NAME_BYTES: usize = 255;
/// Prefix of every content token; the token is `sha256:<lowercase hex>` of
/// the attachment bytes. Deterministic and stable: identical bytes always
/// produce the identical token.
pub const CONTENT_TOKEN_PREFIX: &str = "sha256:";

impl AttachmentStore {
    /// Build a store rooted at `attachments_dir` (typically
    /// `Storage.attachments_dir`). Does not touch the filesystem.
    pub fn new(attachments_dir: impl Into<PathBuf>) -> Self {
        let root = attachments_dir.into();
        Self {
            trash_dir: root.join(TRASH_DIR),
            root,
        }
    }

    /// Create the attachment root and `trash/` under it.
    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;
        fs::create_dir_all(&self.trash_dir)
            .with_context(|| format!("creating {}", self.trash_dir.display()))?;
        Ok(())
    }

    /// Import a regular file, using the source's basename as the display
    /// name. Symlinks and non-regular sources are rejected. Every import
    /// produces a fresh attachment identity; identical bytes are never
    /// deduplicated.
    #[cfg(test)]
    pub fn import_path(&self, source: &Path) -> Result<AttachmentMetadata> {
        let display_name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| source.display().to_string());
        self.import_path_as(source, &display_name)
    }

    /// Import a regular file while storing a caller-selected display name.
    pub fn import_path_as(&self, source: &Path, display_name: &str) -> Result<AttachmentMetadata> {
        let meta = fs::symlink_metadata(source)
            .with_context(|| format!("checking source {}", source.display()))?;
        if meta.file_type().is_symlink() {
            bail!("refusing to import symlink source: {}", source.display());
        }
        if !meta.file_type().is_file() {
            bail!(
                "refusing to import non-regular source: {}",
                source.display()
            );
        }
        let reader =
            File::open(source).with_context(|| format!("opening source {}", source.display()))?;
        self.import_reader(reader, display_name, &source.display().to_string())
    }

    /// Import raw bytes with a display name (provenance is the name). When
    /// `display_name` is `None` the attachment is named `attachment`.
    #[cfg(test)]
    pub fn import_bytes(
        &self,
        bytes: &[u8],
        display_name: Option<&str>,
    ) -> Result<AttachmentMetadata> {
        let name = display_name.unwrap_or("attachment");
        self.import_reader(bytes, name, name)
    }

    /// Stream `reader` into a new attachment, storing `display_name` and
    /// `source` provenance. Enforces [`MAX_ATTACHMENT_SIZE`] while streaming.
    pub fn import_reader(
        &self,
        reader: impl Read,
        display_name: &str,
        source: &str,
    ) -> Result<AttachmentMetadata> {
        self.import_reader_with_limit(reader, display_name, source, MAX_ATTACHMENT_SIZE)
    }

    /// Import with a custom byte cap, used by the public API with
    /// [`MAX_ATTACHMENT_SIZE`] and by tests with a small cap.
    fn import_reader_with_limit(
        &self,
        mut reader: impl Read,
        display_name: &str,
        source: &str,
        limit: u64,
    ) -> Result<AttachmentMetadata> {
        let display_name = validate_display_name(display_name)?;
        self.ensure_layout()?;
        let id = AttachmentId::new();
        let staged = self.root.join(staging_name());
        let publish = (|| {
            fs::create_dir(&staged)
                .with_context(|| format!("creating staging directory {}", staged.display()))?;
            // Stream the content into the staging directory, capping size.
            let mut content = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged.join(content_file_name(&display_name)))
                .with_context(|| format!("creating staging content {}", staged.display()))?;
            let mut sniff = Vec::with_capacity(MIME_SNIFF_BYTES.min(4096));
            let mut buffer = [0u8; STREAM_BUFFER];
            let mut size = 0u64;
            loop {
                let read = reader.read(&mut buffer).context("reading import source")?;
                if read == 0 {
                    break;
                }
                if size + read as u64 > limit {
                    bail!(
                        "attachment exceeds the {} byte limit ({} bytes read so far)",
                        limit,
                        size + read as u64
                    );
                }
                if sniff.len() < MIME_SNIFF_BYTES {
                    let take = (MIME_SNIFF_BYTES - sniff.len()).min(read);
                    sniff.extend_from_slice(&buffer[..take]);
                }
                content
                    .write_all(&buffer[..read])
                    .with_context(|| format!("writing staging content {}", staged.display()))?;
                size += read as u64;
            }
            content
                .sync_all()
                .with_context(|| format!("syncing staging content {}", staged.display()))?;
            // Publish metadata inside the same staging directory.
            let metadata = AttachmentMetadata {
                id,
                size,
                display_name: display_name.clone(),
                source: source.to_string(),
                mime_type: detect_mime_type(&sniff, source),
                imported_at: Utc::now(),
            };
            let json =
                serde_json::to_vec_pretty(&metadata).context("serializing attachment metadata")?;
            let metadata_path = staged.join(METADATA_FILE);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&metadata_path)
                .with_context(|| format!("creating staging metadata {}", staged.display()))?;
            file.write_all(&json)
                .with_context(|| format!("writing staging metadata {}", staged.display()))?;
            file.sync_all()
                .with_context(|| format!("syncing staging metadata {}", staged.display()))?;
            // One atomic rename makes the whole attachment visible.
            fs::rename(&staged, &self.attachment_dir(id)).with_context(|| {
                format!(
                    "publishing attachment {}",
                    self.attachment_dir(id).display()
                )
            })?;
            Ok(metadata)
        })();
        if publish.is_err() {
            fs::remove_dir_all(&staged).ok();
        }
        publish
    }

    /// Look up stored metadata for `id`. `Ok(None)` only when the attachment
    /// is entirely absent; any half-present or inconsistent state is an error.
    pub fn lookup(&self, id: AttachmentId) -> Result<Option<AttachmentMetadata>> {
        let dir = self.attachment_dir(id);
        let dir_present = match fs::symlink_metadata(&dir) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error).with_context(|| format!("checking {}", dir.display())),
        };
        if !dir_present {
            return Ok(None);
        }
        let metadata_present = match fs::symlink_metadata(self.metadata_path(id)) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("checking {}", self.metadata_path(id).display()));
            }
        };
        if metadata_present {
            return Ok(Some(
                self.load_metadata(id)
                    .context("attachment store inconsistent")?,
            ));
        }
        let has_entries = fs::read_dir(&dir)
            .with_context(|| format!("listing {}", dir.display()))?
            .next()
            .transpose()?
            .is_some();
        if has_entries {
            bail!("attachment store inconsistent: content and metadata out of sync for {id}");
        }
        Ok(None)
    }

    /// Stored metadata for `id`, erroring when the attachment is absent.
    pub fn metadata(&self, id: AttachmentId) -> Result<AttachmentMetadata> {
        self.lookup(id)?
            .with_context(|| format!("no such attachment: {id}"))
    }

    /// Safely open the real content file of an attachment for application
    /// use (editing, image decoding, opening with an external program).
    /// Verifies consistency, regularity, and the size limit before returning
    /// the path.
    pub fn open(&self, id: AttachmentId) -> Result<PathBuf> {
        let metadata = self.metadata(id)?;
        Ok(self.content_path(&metadata))
    }

    /// Stream the content of `id` into `writer`, enforcing
    /// [`MAX_ATTACHMENT_SIZE`]. Returns the number of bytes copied.
    #[cfg(test)]
    pub fn copy_to(&self, id: AttachmentId, writer: &mut impl Write) -> Result<u64> {
        self.copy_to_with_token(id, writer).map(|(bytes, _)| bytes)
    }

    /// Stream the content of `id` into `writer`, enforcing
    /// [`MAX_ATTACHMENT_SIZE`]. Returns the number of bytes copied and the
    /// deterministic content token of exactly those bytes, so a caller that
    /// materializes a workspace copy can later prove the attachment still
    /// holds the same content before publishing an edit.
    #[cfg(test)]
    pub fn copy_to_with_token(
        &self,
        id: AttachmentId,
        writer: &mut impl Write,
    ) -> Result<(u64, String)> {
        self.copy_to_with_token_limited(id, writer, MAX_ATTACHMENT_SIZE)
    }

    /// Stream the content of `id` into `writer`, enforcing both the
    /// attachment cap and the caller-provided limit. Returns the number of
    /// bytes copied and the token of exactly those bytes.
    pub fn copy_to_with_token_limited(
        &self,
        id: AttachmentId,
        writer: &mut impl Write,
        limit: u64,
    ) -> Result<(u64, String)> {
        // Validates existence, consistency, and the size limit up front.
        let metadata = self.metadata(id)?;
        let path = self.content_path(&metadata);
        let mut file =
            File::open(&path).with_context(|| format!("opening content {}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; STREAM_BUFFER];
        let mut total = 0u64;
        let limit = limit.min(MAX_ATTACHMENT_SIZE);
        loop {
            let read = file
                .read(&mut buffer)
                .with_context(|| format!("reading content {}", path.display()))?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > limit {
                bail!("attachment {id} exceeds the {limit} byte limit");
            }
            hasher.update(&buffer[..read]);
            writer
                .write_all(&buffer[..read])
                .with_context(|| format!("writing attachment {id} content"))?;
        }
        Ok((
            total,
            format!("{CONTENT_TOKEN_PREFIX}{}", hex_lower(&hasher.finalize())),
        ))
    }

    /// Deterministic content token (`sha256:<hex>`) of the current live
    /// content of `id`. Used as the optimistic-concurrency check before an
    /// in-place update publishes: a token that no longer matches means the
    /// content changed since it was checked out.
    pub fn content_token(&self, id: AttachmentId) -> Result<String> {
        let metadata = self.metadata(id)?;
        let path = self.content_path(&metadata);
        let mut file =
            File::open(&path).with_context(|| format!("opening content {}", path.display()))?;
        Ok(hash_stream(&mut file, MAX_ATTACHMENT_SIZE)?.1)
    }

    /// Stream `reader` in as the new content of `id`, atomically replacing
    /// the live content file while preserving the attachment identity and
    /// every piece of persisted provenance metadata (display name, original
    /// source, import time). The new bytes are staged inside the attachment
    /// directory, synced, checked against `expected_content_token`, and
    /// atomically replaced over `content`; staging is removed on any failure.
    /// The effective byte limit is the smaller of `limit` and
    /// [`MAX_ATTACHMENT_SIZE`]. Size and MIME are re-derived from live
    /// content. Returns updated metadata and the published content token.
    pub fn replace_content(
        &self,
        id: AttachmentId,
        expected_content_token: &str,
        mut reader: impl Read,
        limit: u64,
    ) -> Result<(AttachmentMetadata, String)> {
        // Validates existence, consistency, and the live size limit up front.
        let metadata = self.metadata(id)?;
        let content_path = self.content_path(&metadata);
        let dir = self.attachment_dir(id);
        let staged = dir.join(staging_name());
        let publish = (|| {
            let mut hasher = Sha256::new();
            let mut content = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)
                .with_context(|| format!("creating staging content {}", staged.display()))?;
            let mut buffer = [0u8; STREAM_BUFFER];
            let mut size = 0u64;
            let limit = limit.min(MAX_ATTACHMENT_SIZE);
            loop {
                let read = reader.read(&mut buffer).context("reading update source")?;
                if read == 0 {
                    break;
                }
                if size + read as u64 > limit {
                    bail!("attachment update exceeds the {limit} byte limit");
                }
                hasher.update(&buffer[..read]);
                content
                    .write_all(&buffer[..read])
                    .with_context(|| format!("writing staging content {}", staged.display()))?;
                size += read as u64;
            }
            content
                .sync_all()
                .with_context(|| format!("syncing staging content {}", staged.display()))?;
            // Re-check after staging and syncing, immediately before the
            // atomic publish, so changes while the source was streamed or
            // while approval was pending cannot be overwritten.
            let current_token = self.content_token(id)?;
            if current_token != expected_content_token {
                bail!(
                    "attachment content changed since checkout: expected {expected_content_token}, found {current_token}"
                );
            }
            atomic_replace(&staged, &content_path).with_context(|| {
                format!(
                    "publishing content {} for attachment {id}",
                    content_path.display()
                )
            })?;
            let metadata = self.metadata(id)?;
            let token = format!("{CONTENT_TOKEN_PREFIX}{}", hex_lower(&hasher.finalize()));
            Ok((metadata, token))
        })();
        if publish.is_err() {
            fs::remove_file(&staged).ok();
        }
        publish
    }

    /// Read at most `max_bytes` of content into memory, erroring when the
    /// attachment is larger than the caller's bound. This is the bounded
    /// text-read primitive for callers that need bytes in memory (for
    /// example agent text reads with a 1 MiB cap).
    pub fn read_limited(&self, id: AttachmentId, max_bytes: u64) -> Result<Vec<u8>> {
        let metadata = self.metadata(id)?;
        if metadata.size > max_bytes {
            bail!(
                "attachment {id} is {} bytes, exceeding the {} byte read limit",
                metadata.size,
                max_bytes
            );
        }
        let path = self.content_path(&metadata);
        fs::read(&path).with_context(|| format!("reading content {}", path.display()))
    }

    #[cfg(test)]
    /// Read the full content into memory, capped at [`MAX_ATTACHMENT_SIZE`].
    /// Intended for whole-content consumers such as image decoding; the
    /// store's primary read interfaces are [`AttachmentStore::open`] and
    /// [`AttachmentStore::copy_to_with_token`].
    pub fn read_all(&self, id: AttachmentId) -> Result<Vec<u8>> {
        self.read_limited(id, MAX_ATTACHMENT_SIZE)
    }

    /// List attachments matching `query`, sorted and paginated. Default sort
    /// is `imported_at` descending (newest first). Inconsistent entries are
    /// errors; empty or staging-only directories are skipped.
    pub fn list(&self, query: &AttachmentQuery) -> Result<AttachmentPage> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AttachmentPage {
                    items: Vec::new(),
                    total: 0,
                    offset: query.offset,
                    limit: query.limit,
                    has_more: false,
                });
            }
            Err(error) => {
                return Err(error).with_context(|| format!("listing {}", self.root.display()));
            }
        };
        let needle = query.query.to_lowercase();
        let mut matched = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            // Staging leftovers and the trash directory are never attachments.
            if name.starts_with('.') || name == TRASH_DIR {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                bail!(
                    "refusing symlink in attachment store: {}",
                    entry.path().display()
                );
            }
            if !file_type.is_dir() {
                continue;
            }
            let Ok(id) = AttachmentId::parse(name) else {
                continue;
            };
            let Some(metadata) = self.lookup(id)? else {
                continue; // empty or staging-only directory: not an attachment
            };
            if needle.is_empty()
                || metadata.display_name.to_lowercase().contains(&needle)
                || metadata.source.to_lowercase().contains(&needle)
            {
                matched.push(metadata);
            }
        }
        matched.sort_by(|a, b| {
            let key = |metadata: &AttachmentMetadata| -> (u64, String, u64, String, AttachmentId) {
                match query.sort_by {
                    AttachmentSortBy::ImportedAt => (
                        0,
                        String::new(),
                        metadata.imported_at.timestamp_nanos_opt().unwrap_or(0) as u64,
                        String::new(),
                        metadata.id,
                    ),
                    AttachmentSortBy::Name => (
                        1,
                        metadata.display_name.to_lowercase(),
                        0,
                        String::new(),
                        metadata.id,
                    ),
                    AttachmentSortBy::Size => {
                        (2, String::new(), metadata.size, String::new(), metadata.id)
                    }
                    AttachmentSortBy::Type => (
                        3,
                        String::new(),
                        0,
                        metadata.mime_type.clone().unwrap_or_default(),
                        metadata.id,
                    ),
                }
            };
            let order = match query.order {
                AttachmentSortOrder::Asc => key(a).cmp(&key(b)),
                AttachmentSortOrder::Desc => key(a).cmp(&key(b)).reverse(),
            };
            order
        });
        let total = matched.len() as u64;
        let start = usize::try_from(query.offset).unwrap_or(usize::MAX);
        let end = usize::try_from(query.limit)
            .map(|limit| start.saturating_add(limit))
            .unwrap_or(usize::MAX);
        let items: Vec<_> = matched
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        let has_more = query.offset.saturating_add(query.limit) < total;
        Ok(AttachmentPage {
            items,
            total,
            offset: query.offset,
            limit: query.limit,
            has_more,
        })
    }

    /// Move the whole attachment directory for `id` into `trash/` with one
    /// atomic rename. Returns `false` when the attachment does not exist.
    /// Inconsistent state is an error.
    pub fn remove(&self, id: AttachmentId) -> Result<bool> {
        if self.lookup(id)?.is_none() {
            return Ok(false);
        }
        self.ensure_layout()?;
        let source = self.attachment_dir(id);
        let name = id.to_string();
        let mut destination = self.trash_dir.join(&name);
        let mut index = 1;
        while destination.exists() {
            destination = self.trash_dir.join(format!("{name}-{index}"));
            index += 1;
        }
        fs::rename(&source, &destination)
            .with_context(|| format!("moving {} to trash", source.display()))?;
        Ok(true)
    }

    /// Read and validate the metadata for `id`, filling `size` from the live
    /// content file. Errors on any inconsistent or hostile state.
    fn load_metadata(&self, id: AttachmentId) -> Result<AttachmentMetadata> {
        let dir_meta = fs::symlink_metadata(&self.attachment_dir(id)).with_context(|| {
            format!(
                "checking attachment directory {}",
                self.attachment_dir(id).display()
            )
        })?;
        if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
            bail!(
                "attachment store inconsistent: {} is not a regular directory",
                self.attachment_dir(id).display()
            );
        }
        let metadata_path = self.metadata_path(id);
        let meta = fs::symlink_metadata(&metadata_path)
            .with_context(|| format!("checking {}", metadata_path.display()))?;
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            bail!(
                "attachment store inconsistent: {} is not a regular file",
                metadata_path.display()
            );
        }
        let json = fs::read(&metadata_path)
            .with_context(|| format!("reading metadata {}", metadata_path.display()))?;
        let mut metadata: AttachmentMetadata = serde_json::from_slice(&json)
            .with_context(|| format!("parsing metadata {}", metadata_path.display()))?;
        if metadata.id != id {
            bail!(
                "attachment store inconsistent: metadata at {} names id {} instead of {}",
                metadata_path.display(),
                metadata.id,
                id
            );
        }
        let content_path = self.content_path(&metadata);
        let content_meta = fs::symlink_metadata(&content_path)
            .with_context(|| format!("checking content {}", content_path.display()))?;
        if content_meta.file_type().is_symlink() || !content_meta.file_type().is_file() {
            bail!(
                "attachment store inconsistent: {} is not a regular file",
                content_path.display()
            );
        }
        let size = content_meta.len();
        if size > MAX_ATTACHMENT_SIZE {
            bail!(
                "attachment {id} exceeds the {} byte limit (content is {size} bytes)",
                MAX_ATTACHMENT_SIZE
            );
        }
        metadata.size = size;
        let mut content = File::open(&content_path)
            .with_context(|| format!("opening content {}", content_path.display()))?;
        let mut sniff = vec![0u8; MIME_SNIFF_BYTES];
        let read = content
            .read(&mut sniff)
            .with_context(|| format!("reading content {}", content_path.display()))?;
        sniff.truncate(read);
        metadata.mime_type = detect_mime_type(&sniff, &metadata.source);
        Ok(metadata)
    }

    /// Path of the attachment directory for `id`. Ids are validated UUIDs, so
    /// this can never traverse outside the root.
    fn attachment_dir(&self, id: AttachmentId) -> PathBuf {
        self.root.join(id.to_string())
    }

    fn content_path(&self, metadata: &AttachmentMetadata) -> PathBuf {
        self.attachment_dir(metadata.id)
            .join(content_file_name(&metadata.display_name))
    }

    fn metadata_path(&self, id: AttachmentId) -> PathBuf {
        self.attachment_dir(id).join(METADATA_FILE)
    }
}

/// Stable private content filename that retains a normal display extension.
/// Long or absent extensions fall back to `content`: they are unlikely to be
/// meaningful file associations and must not exceed platform filename limits.
fn content_file_name(display_name: &str) -> String {
    let extension = Path::new(display_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| {
            !extension.is_empty() && extension.len() <= MAX_CONTENT_EXTENSION_BYTES
        });
    match extension {
        Some(extension) => format!("{CONTENT_FILE_PREFIX}.{extension}"),
        None => CONTENT_FILE_PREFIX.to_string(),
    }
}

/// Uniquely named staging directory: never collides with `<uuid>/`
/// attachment directories or `trash/`.
fn staging_name() -> String {
    format!(".tmp-{}-{:016x}", std::process::id(), fastrand::u64(..))
}

/// Lowercase hex encoding used by content tokens.
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Replace an existing file with a same-directory staged file. Unix rename
/// atomically replaces the destination; Windows uses ReplaceFileW, the OS
/// primitive specifically intended to replace an existing file atomically.
fn atomic_replace(staged: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(not(windows))]
    {
        fs::rename(staged, destination)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_WRITE_THROUGH};

        let replacement = staged
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let replaced = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let replaced = unsafe {
            ReplaceFileW(
                replaced.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if replaced == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
/// Stream `reader` through SHA-256, refusing once more than `limit` bytes
/// would be read. Returns the byte count and the content token of those
/// bytes.
fn hash_stream(reader: &mut impl Read, limit: u64) -> Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; STREAM_BUFFER];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer).context("reading content")?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > limit {
            bail!("attachment exceeds the {limit} byte limit");
        }
        hasher.update(&buffer[..read]);
    }
    Ok((
        total,
        format!("{CONTENT_TOKEN_PREFIX}{}", hex_lower(&hasher.finalize())),
    ))
}

/// Validate a display name: it must be a non-empty bare file name with no
/// path separators, no control characters (including newlines and NUL), not
/// `.` or `..`, and at most [`MAX_DISPLAY_NAME_BYTES`] bytes.
pub fn validate_display_name(name: &str) -> Result<String> {
    if name.is_empty() {
        bail!("display name must not be empty");
    }
    if name == "." || name == ".." {
        bail!("display name must not be `.` or `..`");
    }
    if name.len() > MAX_DISPLAY_NAME_BYTES {
        bail!(
            "display name exceeds the {} byte limit",
            MAX_DISPLAY_NAME_BYTES
        );
    }
    for ch in name.chars() {
        if ch.is_control() {
            bail!("display name contains a control character");
        }
        if ch == '/' || ch == '\\' {
            bail!("display name must not contain path separators");
        }
    }
    Ok(name.to_string())
}

/// Escape a display name for use as a Markdown link label or image alt text:
/// backslashes, `[`, and `]` are backslash-escaped so a hostile name cannot
/// break out of the label into the destination.
pub fn escape_markdown_label(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            _ => out.push(ch),
        }
    }
    out
}

/// Best-effort media type from the display name's extension, used only when
/// content inspection is inconclusive.
fn infer_mime_from_extension(name: &str) -> Option<String> {
    let extension = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    let mime = match extension.as_str() {
        "md" | "markdown" | "mb" => "text/markdown",
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "epub" => "application/epub+zip",
        _ => return None,
    };
    Some(mime.to_string())
}

/// Detect a media type from content. Magic bytes win over everything; UTF-8
/// content is typed by its extension (defaulting to `text/plain`); binary
/// content that matches no magic falls back to its extension; otherwise
/// `None`.
pub fn detect_mime_type(content: &[u8], display_name: &str) -> Option<String> {
    let extension = Path::new(display_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());

    let magic = magic_mime(content);
    match magic {
        // Zip-family containers carry their real type in the extension
        // (Office documents, EPUB, JAR, ...); otherwise plain application/zip.
        Some("application/zip") => {
            let specific = match extension.as_deref() {
                Some("docx") => {
                    Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
                }
                Some("xlsx") => {
                    Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
                }
                Some("pptx") => Some(
                    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                ),
                Some("epub") => Some("application/epub+zip"),
                Some("jar") => Some("application/java-archive"),
                Some("apk") => Some("application/vnd.android.package-archive"),
                Some("odt") => Some("application/vnd.oasis.opendocument.text"),
                Some("ods") => Some("application/vnd.oasis.opendocument.spreadsheet"),
                Some("odp") => Some("application/vnd.oasis.opendocument.presentation"),
                _ => None,
            };
            Some(specific.unwrap_or("application/zip").to_string())
        }
        Some(mime) => Some(mime.to_string()),
        None => {
            if std::str::from_utf8(content).is_ok() {
                // UTF-8 text: extension distinguishes Markdown/JSON/plain.
                let text_mime = match extension.as_deref() {
                    Some("md" | "markdown" | "mb") => Some("text/markdown"),
                    Some("html" | "htm") => Some("text/html"),
                    Some("json") => Some("application/json"),
                    Some("csv") => Some("text/csv"),
                    Some("xml") => Some("application/xml"),
                    Some("svg") => Some("image/svg+xml"),
                    _ => None,
                };
                return Some(text_mime.unwrap_or("text/plain").to_string());
            }
            // Binary content with no magic: extension is the only hint.
            infer_mime_from_extension(display_name)
        }
    }
}

/// Content magic-byte detection. Text returns `None` here; the caller decides
/// text typing. Non-text binary types that lack a reliable magic (7z, DOC)
/// are handled by the extension fallback.
fn magic_mime(content: &[u8]) -> Option<&'static str> {
    let starts = |prefix: &[u8]| content.starts_with(prefix);
    let has_at = |offset: usize, needle: &[u8]| {
        content
            .get(offset..offset + needle.len())
            .is_some_and(|slice| slice == needle)
    };
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if starts(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some("image/gif");
    }
    if starts(b"RIFF") && has_at(8, b"WEBP") {
        return Some("image/webp");
    }
    if starts(b"BM") {
        return Some("image/bmp");
    }
    if starts(b"\x00\x00\x01\x00") {
        return Some("image/x-icon");
    }
    if starts(b"%PDF-") {
        return Some("application/pdf");
    }
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") || starts(b"PK\x07\x08") {
        return Some("application/zip");
    }
    if starts(b"\x1f\x8b") {
        return Some("application/gzip");
    }
    if has_at(257, b"ustar") {
        return Some("application/x-tar");
    }
    if starts(b"OggS") {
        return Some("audio/ogg");
    }
    if starts(b"ID3") || starts(&[0xff, 0xfb]) || starts(&[0xff, 0xf3]) || starts(&[0xff, 0xf2]) {
        return Some("audio/mpeg");
    }
    if starts(b"RIFF") && has_at(8, b"WAVE") {
        return Some("audio/wav");
    }
    if has_at(4, b"ftyp") {
        return if has_at(8, b"qt  ") {
            Some("video/quicktime")
        } else {
            Some("video/mp4")
        };
    }
    if starts(b"\x1a\x45\xdf\xa3") {
        return Some("video/webm");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;
    use std::time::Duration;

    fn fresh() -> (tempfile::TempDir, AttachmentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path().join("attachments"));
        store.ensure_layout().unwrap();
        (dir, store)
    }

    #[test]
    fn canonical_uri_parses_and_displays_verbatim() {
        let id = AttachmentId::new();
        let text = id.to_string();
        assert_eq!(text.len(), 36);
        assert_eq!(text.chars().filter(|ch| *ch == '-').count(), 4);
        assert!(text.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-'));
        let uri = AttachmentUri::from_id(id);
        assert_eq!(uri.to_string(), format!("{URI_SCHEME}{text}"));
        assert_eq!(uri.id(), id);
        assert_eq!(AttachmentUri::parse(&uri.to_string()).unwrap(), uri);
        let parsed: AttachmentUri = uri.to_string().parse().unwrap();
        assert_eq!(parsed, uri);
        assert_eq!(AttachmentId::parse(&text).unwrap(), id);
    }

    #[test]
    fn scheme_classification_is_prefix_based() {
        let id = AttachmentId::new();
        assert!(AttachmentUri::is_attachment_uri(&format!(
            "{URI_SCHEME}{id}"
        )));
        // Malformed after the scheme is still classified as an attachment URI.
        assert!(AttachmentUri::is_attachment_uri(
            format!("{URI_SCHEME}not-a-uuid").as_str()
        ));
        assert!(AttachmentUri::is_attachment_uri(URI_SCHEME));
        assert!(!AttachmentUri::is_attachment_uri("nole://attachments/x"));
        assert!(!AttachmentUri::is_attachment_uri(
            "nole-attachment://sha256/x"
        ));
        assert!(!AttachmentUri::is_attachment_uri(
            "https://nole/attachment/x"
        ));
        assert!(!AttachmentUri::is_attachment_uri(""));
    }

    #[test]
    fn uri_rejects_malformed_input() {
        let id = AttachmentId::new();
        let text = id.to_string();
        let cases = [
            format!("{URI_SCHEME}{}", text.to_uppercase()),
            format!("{URI_SCHEME}{}", text.replace('-', "")),
            format!("{URI_SCHEME}urn:uuid:{text}"),
            format!("{URI_SCHEME}{{{text}}}"),
            format!("{URI_SCHEME}{text}extra"),
            format!("{URI_SCHEME}{}&", &text[..35]),
            format!("{URI_SCHEME}{text}/extra"),
            format!("{URI_SCHEME}"),
            format!("{URI_SCHEME}{}", "z".repeat(36)),
            format!("attachment://{text}"),
            "".to_string(),
        ];
        for case in &cases {
            assert!(
                AttachmentUri::parse(case).is_err(),
                "expected rejection: {case}"
            );
        }
    }

    #[test]
    fn id_rejects_noncanonical_forms() {
        let id = AttachmentId::new();
        let text = id.to_string();
        assert!(AttachmentId::parse("").is_err());
        assert!(AttachmentId::parse(&text.to_uppercase()).is_err());
        assert!(AttachmentId::parse(&text.replace('-', "")).is_err());
        assert!(AttachmentId::parse(&format!("urn:uuid:{text}")).is_err());
        assert!(AttachmentId::parse(&format!("{{{text}}}")).is_err());
        assert!(AttachmentId::parse(&format!("{text}x")).is_err());
        let valid = AttachmentId::parse(&text).unwrap();
        assert_eq!(valid, id);
        assert!(AttachmentId::new() != AttachmentId::new());
    }

    #[test]
    fn duplicate_imports_are_distinct() {
        let (_dir, store) = fresh();
        let a = store
            .import_bytes(b"shared bytes", Some("first.txt"))
            .unwrap();
        let b = store
            .import_bytes(b"shared bytes", Some("second.txt"))
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(a.display_name, "first.txt");
        assert_eq!(b.display_name, "second.txt");
        assert_eq!(a.source, "first.txt");
        assert_eq!(a.size, 12);
        assert_eq!(b.size, 12);
        let page = store.list(&AttachmentQuery::default()).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(store.read_all(a.id).unwrap(), b"shared bytes");
        assert_eq!(store.read_all(b.id).unwrap(), b"shared bytes");
    }

    #[test]
    fn one_directory_per_attachment_with_content_and_metadata() {
        let (dir, store) = fresh();
        let source = dir.path().join("doc.pdf");
        fs::write(&source, b"%PDF-1.4 fake").unwrap();
        let imported = store.import_path(&source).unwrap();
        let attachment_dir = store.attachment_dir(imported.id);
        assert!(attachment_dir.is_dir());
        let entries: Vec<String> = fs::read_dir(&attachment_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&"content.pdf".to_string()));
        assert!(entries.contains(&"metadata.json".to_string()));
        // Directory name is the canonical id.
        assert_eq!(
            attachment_dir.file_name().unwrap().to_str().unwrap(),
            imported.id.to_string()
        );
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let (dir, store) = fresh();
        let source = dir.path().join("doc.pdf");
        fs::write(&source, b"%PDF-1.4 fake").unwrap();
        let imported = store.import_path(&source).unwrap();
        let json = fs::read_to_string(store.metadata_path(imported.id)).unwrap();
        let parsed: AttachmentMetadata = serde_json::from_str(&json).unwrap();
        // size is not persisted; it is filled from the live content file.
        assert_eq!(parsed.size, 0);
        assert_eq!(parsed.id, imported.id);
        assert_eq!(parsed.display_name, imported.display_name);
        assert_eq!(parsed.mime_type, imported.mime_type);
        assert_eq!(parsed.mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(store.metadata(imported.id).unwrap(), imported);
        assert_eq!(store.metadata(imported.id).unwrap().uri().id(), imported.id);
    }

    #[test]
    fn import_preserves_display_name_and_provenance() {
        let (dir, store) = fresh();
        let source = dir.path().join("report.md");
        fs::write(&source, b"# report").unwrap();
        let imported = store.import_path_as(&source, "custom name.md").unwrap();
        assert_eq!(imported.display_name, "custom name.md");
        assert_eq!(imported.source, source.display().to_string());
        assert_eq!(imported.mime_type.as_deref(), Some("text/markdown"));
        assert_eq!(imported.size, 8);
        // import_path derives the display name from the basename.
        let derived = store.import_path(&source).unwrap();
        assert_eq!(derived.display_name, "report.md");
    }

    #[test]
    fn symlink_sources_are_rejected() {
        use std::os::unix::fs::symlink;
        let (dir, store) = fresh();
        let target = dir.path().join("real.txt");
        fs::write(&target, b"content").unwrap();
        let link = dir.path().join("linked.txt");
        symlink(&target, &link).unwrap();
        let error = store.import_path(&link).unwrap_err();
        assert!(error.to_string().contains("symlink"));
        assert!(store.list(&AttachmentQuery::default()).unwrap().total == 0);
    }

    #[test]
    fn non_regular_and_missing_sources_are_rejected() {
        let (dir, store) = fresh();
        assert!(store.import_path(dir.path()).is_err());
        assert!(store.import_path(&dir.path().join("missing.txt")).is_err());
    }

    #[test]
    fn store_inconsistency_is_rejected() {
        use std::os::unix::fs::symlink;
        let (dir, store) = fresh();
        let import = |name: &str, content: &[u8]| -> AttachmentMetadata {
            let source = dir.path().join(name);
            fs::write(&source, content).unwrap();
            store.import_path(&source).unwrap()
        };
        // Content without metadata.
        let a = import("a.txt", b"data-a");
        fs::remove_file(store.content_path(&a)).unwrap();
        assert!(store.lookup(a.id).is_err());
        assert!(store.metadata(a.id).is_err());
        assert!(store.read_all(a.id).is_err());
        // Metadata without content.
        let b = import("b.txt", b"data-b");
        fs::remove_file(store.metadata_path(b.id)).unwrap();
        assert!(store.lookup(b.id).is_err());
        // Metadata naming a different id than its directory.
        let c = import("c.txt", b"data-c");
        let path = store.metadata_path(c.id);
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        json["id"] = serde_json::json!(AttachmentId::new().to_string());
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(store.lookup(c.id).is_err());
        // Symlinked content entry.
        let d = import("d.txt", b"data-d");
        let decoy = dir.path().join("decoy");
        fs::write(&decoy, b"data").unwrap();
        fs::remove_file(store.content_path(&d)).unwrap();
        symlink(&decoy, store.content_path(&d)).unwrap();
        assert!(store.lookup(d.id).is_err());
        // Unknown metadata field is rejected on read.
        let e = import("e.txt", b"data-e");
        let path = store.metadata_path(e.id);
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        json["bogus"] = serde_json::json!(1);
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(store.metadata(e.id).is_err());
    }

    #[test]
    fn content_is_mutable_in_place() {
        let (dir, store) = fresh();
        let source = dir.path().join("v.txt");
        fs::write(&source, b"original").unwrap();
        let imported = store.import_path(&source).unwrap();
        assert_eq!(store.read_all(imported.id).unwrap(), b"original");
        // External edits of the real content file update identity, size, and
        // content-derived media type without changing the URI.
        fs::write(store.content_path(&imported), b"\x89PNG\r\n\x1a\nrevised").unwrap();
        let metadata = store.metadata(imported.id).unwrap();
        assert_eq!(metadata.size, 15);
        assert_eq!(metadata.mime_type.as_deref(), Some("image/png"));
        assert_eq!(
            store.read_all(imported.id).unwrap(),
            b"\x89PNG\r\n\x1a\nrevised"
        );
        assert_eq!(metadata.id, imported.id, "identity survives content edits");
    }

    #[test]
    fn open_returns_the_real_content_path() {
        let (_dir, store) = fresh();
        let imported = store.import_bytes(b"open me", Some("open.txt")).unwrap();
        let opened = store.open(imported.id).unwrap();
        assert_eq!(opened, store.content_path(&imported));
        assert_eq!(
            opened.file_name().and_then(|name| name.to_str()),
            Some("content.txt")
        );
        assert_eq!(fs::read(&opened).unwrap(), b"open me");
        // Mutating the opened file is visible through the store.
        fs::write(&opened, b"edited").unwrap();
        assert_eq!(store.read_all(imported.id).unwrap(), b"edited");
    }

    #[test]
    fn remove_trashes_attachment_directory_atomically() {
        let (dir, store) = fresh();
        let source = dir.path().join("r.txt");
        fs::write(&source, b"remove me").unwrap();
        let imported = store.import_path(&source).unwrap();
        assert!(store.remove(imported.id).unwrap());
        assert!(store.lookup(imported.id).unwrap().is_none());
        assert!(!store.attachment_dir(imported.id).exists());
        let trash: Vec<String> = fs::read_dir(&store.trash_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        // Exactly one trashed entry: the whole attachment directory.
        assert_eq!(trash, vec![imported.id.to_string()]);
        assert!(!store.remove(imported.id).unwrap());
        assert!(store.list(&AttachmentQuery::default()).unwrap().total == 0);
    }

    #[test]
    fn list_defaults_to_imported_at_descending() {
        let (dir, store) = fresh();
        let source = dir.path().join("a.txt");
        for content in [&b"one"[..], &b"two"[..], &b"three"[..]] {
            fs::write(&source, content).unwrap();
            store.import_path(&source).unwrap();
            std::thread::sleep(Duration::from_millis(3));
        }
        let page = store.list(&AttachmentQuery::default()).unwrap();
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.total, 3);
        assert!(!page.has_more);
        assert!(
            page.items[0].imported_at >= page.items[1].imported_at,
            "newest first by default"
        );
        assert!(page.items[1].imported_at >= page.items[2].imported_at);
        // Every listed attachment is resolvable.
        assert_eq!(
            store.lookup(page.items[0].id).unwrap().unwrap(),
            page.items[0]
        );
    }

    #[test]
    fn list_filters_sorts_and_paginates() {
        let (dir, store) = fresh();
        let source = dir.path().join("f.txt");
        fs::write(&source, b"alpha").unwrap();
        store.import_path_as(&source, "alpha.txt").unwrap();
        std::thread::sleep(Duration::from_millis(3));
        fs::write(&source, b"beta").unwrap();
        store.import_path_as(&source, "beta.md").unwrap();
        std::thread::sleep(Duration::from_millis(3));
        fs::write(&source, b"gamma").unwrap();
        store.import_path_as(&source, "gamma.png").unwrap();

        // Query substring over display name (case-insensitive).
        let query = AttachmentQuery {
            query: "BETA".to_string(),
            ..AttachmentQuery::default()
        };
        let page = store.list(&query).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].display_name, "beta.md");

        // Name sort, ascending.
        let by_name = AttachmentQuery {
            sort_by: AttachmentSortBy::Name,
            order: AttachmentSortOrder::Asc,
            ..AttachmentQuery::default()
        };
        let page = store.list(&by_name).unwrap();
        let names: Vec<_> = page.items.iter().map(|m| m.display_name.as_str()).collect();
        assert_eq!(names, vec!["alpha.txt", "beta.md", "gamma.png"]);

        // Pagination: limit 2 then offset 2.
        let first = AttachmentQuery {
            limit: 2,
            ..AttachmentQuery::default()
        };
        let page = store.list(&first).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.total, 3);
        assert!(page.has_more);
        let second = AttachmentQuery {
            offset: 2,
            limit: 2,
            ..AttachmentQuery::default()
        };
        let page = store.list(&second).unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(!page.has_more);
    }

    #[test]
    fn lookup_missing_returns_none() {
        let (_dir, store) = fresh();
        let id = AttachmentId::new();
        assert!(store.lookup(id).unwrap().is_none());
        assert!(store.metadata(id).is_err());
        assert!(store.read_all(id).is_err());
        assert!(store.read_limited(id, 1024).is_err());
        assert!(store.open(id).is_err());
        assert!(!store.remove(id).unwrap());
    }

    #[test]
    fn empty_bytes_import_is_valid_and_not_digest_shaped() {
        let (_dir, store) = fresh();
        let imported = store.import_bytes(b"", Some("empty.bin")).unwrap();
        assert_eq!(imported.size, 0);
        assert_eq!(store.read_all(imported.id).unwrap(), b"");
        let id_text = imported.id.to_string();
        assert_eq!(id_text.len(), 36);
        assert!(id_text.contains('-'), "opaque UUID, not a digest");
    }

    #[test]
    fn import_bytes_records_optional_display_name() {
        let (_dir, store) = fresh();
        let imported = store.import_bytes(b"bytes", None).unwrap();
        assert_eq!(imported.display_name, "attachment");
        // UTF-8 content is at least text/plain.
        assert_eq!(imported.mime_type.as_deref(), Some("text/plain"));
        let other = store.import_bytes(b"other bytes", Some("note.md")).unwrap();
        assert_ne!(other.id, imported.id);
        assert_eq!(other.display_name, "note.md");
        assert_eq!(other.mime_type.as_deref(), Some("text/markdown"));
    }

    #[test]
    fn no_staging_files_left_behind() {
        let (dir, store) = fresh();
        let source = dir.path().join("s.txt");
        fs::write(&source, b"staging").unwrap();
        store.import_path(&source).unwrap();
        let leftovers: Vec<String> = fs::read_dir(&store.root)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                name.starts_with(".tmp-").then_some(name)
            })
            .collect();
        assert!(leftovers.is_empty(), "leftover staging: {leftovers:?}");
        assert!(fs::read_dir(&store.trash_dir).unwrap().next().is_none());
    }

    #[test]
    fn failed_import_leaves_nothing_behind() {
        let (_dir, store) = fresh();
        // A reader that overflows the (small, test-only) cap.
        let reader = Cursor::new(vec![b'x'; 100]);
        let error = store
            .import_reader_with_limit(reader, "big.bin", "big.bin", 64)
            .unwrap_err();
        assert!(error.to_string().contains("limit"));
        let leftovers: Vec<String> = fs::read_dir(&store.root)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                name.starts_with(".tmp-").then_some(name)
            })
            .collect();
        assert!(leftovers.is_empty(), "leftover staging: {leftovers:?}");
        assert!(store.list(&AttachmentQuery::default()).unwrap().total == 0);
    }

    #[test]
    fn read_limited_enforces_the_bound() {
        let (_dir, store) = fresh();
        let imported = store
            .import_bytes(b"0123456789", Some("digits.txt"))
            .unwrap();
        assert_eq!(store.read_limited(imported.id, 100).unwrap(), b"0123456789");
        assert!(store.read_limited(imported.id, 4).is_err());
    }

    #[test]
    fn copy_to_streams_content() {
        let (_dir, store) = fresh();
        let imported = store.import_bytes(b"stream me", Some("s.bin")).unwrap();
        let mut out = Vec::new();
        let copied = store.copy_to(imported.id, &mut out).unwrap();
        assert_eq!(copied, 9);
        assert_eq!(out, b"stream me");
        // A second copy is independent.
        let mut again = Vec::new();
        store.copy_to(imported.id, &mut again).unwrap();
        assert_eq!(again, out);
    }

    #[test]
    fn mime_detection_prefers_content_over_extension() {
        // PNG magic beats the .txt name.
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        assert_eq!(
            detect_mime_type(png, "notes.txt").as_deref(),
            Some("image/png")
        );
        // UTF-8 text named .png is text/plain, not an image.
        assert_eq!(
            detect_mime_type(b"hello world", "photo.png").as_deref(),
            Some("text/plain")
        );
        // PDF magic beats the .bin name.
        assert_eq!(
            detect_mime_type(b"%PDF-1.7\n%%EOF", "data.bin").as_deref(),
            Some("application/pdf")
        );
        // Garbage bytes with a known extension fall back to the extension.
        assert_eq!(
            detect_mime_type(&[0xff, 0x00, 0x01, 0xfe], "image.png").as_deref(),
            Some("image/png")
        );
        // Garbage bytes with an unknown extension are undetected.
        assert_eq!(
            detect_mime_type(&[0xff, 0x00, 0x01, 0xfe], "blob.bin"),
            None
        );
    }

    #[test]
    fn mime_detection_handles_text_zip_and_containers() {
        assert_eq!(
            detect_mime_type(br#"{"a": 1}"#, "data.json").as_deref(),
            Some("application/json")
        );
        assert_eq!(
            detect_mime_type(b"# title", "notes.md").as_deref(),
            Some("text/markdown")
        );
        let zip = b"PK\x03\x04\x00\x00\x00\x00";
        assert_eq!(
            detect_mime_type(zip, "archive.zip").as_deref(),
            Some("application/zip")
        );
        assert_eq!(
            detect_mime_type(zip, "report.docx").as_deref(),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        );
        assert_eq!(
            detect_mime_type(b"\x1f\x8b\x08\x00", "data.gz").as_deref(),
            Some("application/gzip")
        );
    }

    #[test]
    fn display_name_validation_rejects_hostile_names() {
        let valid = "report v2 (final).pdf";
        assert_eq!(validate_display_name(valid).unwrap(), valid);
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\nb",
            "a\r\nb",
            "a\x00b",
            "a\x7fb",
            "\u{000b}lead",
        ] {
            assert!(
                validate_display_name(bad).is_err(),
                "expected rejection: {bad:?}"
            );
        }
        assert!(validate_display_name(&"x".repeat(256)).is_err());
        assert!(validate_display_name(&"x".repeat(255)).is_ok());
        // Markdown punctuation is allowed and escaped when generating labels.
        assert!(validate_display_name("x](example).png").is_ok());
    }

    #[test]
    fn markdown_label_escaping_escapes_brackets_and_backslashes() {
        assert_eq!(
            escape_markdown_label("x](example).png"),
            "x\\](example).png"
        );
        assert_eq!(escape_markdown_label("a[b]c\\d"), "a\\[b\\]c\\\\d");
        assert_eq!(escape_markdown_label("plain name.png"), "plain name.png");
        // End-to-end embed shape stays well-formed.
        let id = AttachmentId::new();
        let uri = AttachmentUri::from_id(id).to_string();
        let label = escape_markdown_label("x](example).png");
        let embed = format!("![{label}]({uri})");
        assert_eq!(embed, format!("![x\\](example).png]({uri})"));
    }
}
