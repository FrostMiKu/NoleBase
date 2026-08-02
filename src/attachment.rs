//! Content-addressed attachment storage for Nole.
//!
//! Attachments are immutable objects identified by the SHA-256 digest of their
//! bytes. The canonical URI is `nole-attachment://sha256/<64 lowercase hex>`.
//!
//! Physical layout is private to [`AttachmentStore`] and rooted at
//! `Storage.attachments_dir`:
//!
//! - `objects/<hex>`         raw object bytes
//! - `metadata/<hex>.json`   JSON metadata for the object
//! - `trash/`                objects and metadata moved by `remove`
//!
//! Imports stream the source through SHA-256 into a uniquely named staging
//! file that is atomically renamed into place; metadata is published the same
//! way and is first-import-wins, so re-importing identical bytes never churns
//! provenance. Malformed ids, symlinks, non-regular sources, and out-of-sync
//! object/metadata pairs are rejected rather than silently repaired. Object
//! paths are never exposed: callers receive bytes, ids, and metadata only.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Content address of an attachment: the lowercase-hex SHA-256 digest of its
/// object bytes. Only strictly canonical digests (exactly 64 lowercase hex
/// characters) can be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttachmentId([u8; 32]);

impl AttachmentId {
    /// Parse a strictly canonical digest: exactly 64 lowercase hex characters.
    pub fn from_hex(hex: &str) -> Result<Self> {
        if hex.len() != 64 {
            bail!(
                "attachment id must be exactly 64 hex characters, got {}",
                hex.len()
            );
        }
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("attachment id contains non-hex characters");
        }
        if hex.bytes().any(|byte| byte.is_ascii_uppercase()) {
            bail!("attachment id must be lowercase hex");
        }
        let mut bytes = [0u8; 32];
        for (index, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                .expect("hex validated above");
        }
        Ok(Self(bytes))
    }

    /// Build an id from a raw SHA-256 digest.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The canonical 64-character lowercase hex form.
    pub fn to_hex(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for AttachmentId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for AttachmentId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        Self::from_hex(&hex).map_err(serde::de::Error::custom)
    }
}

/// Canonical URI of an attachment: `nole-attachment://sha256/<64 lowercase hex>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttachmentUri(AttachmentId);

const URI_SCHEME: &str = "nole-attachment://";

impl AttachmentUri {
    /// Strictly parse a canonical attachment URI. Anything other than
    /// `nole-attachment://sha256/<64 lowercase hex>` is rejected.
    pub fn parse(input: &str) -> Result<Self> {
        let rest = input
            .strip_prefix(URI_SCHEME)
            .with_context(|| format!("attachment URI must start with {URI_SCHEME}"))?;
        let (algorithm, digest) = rest
            .split_once('/')
            .context("attachment URI must be nole-attachment://sha256/<digest>")?;
        if algorithm != "sha256" {
            bail!("unsupported attachment URI algorithm `{algorithm}`, expected sha256");
        }
        Ok(Self(AttachmentId::from_hex(digest)?))
    }

    /// The content address this URI points at.
    pub fn id(self) -> AttachmentId {
        self.0
    }

    /// Canonical URI for a content address.
    pub fn from_id(id: AttachmentId) -> Self {
        Self(id)
    }

    /// True when `input` starts with the attachment URI scheme, regardless of
    /// whether the rest parses. Used to classify link targets before strict
    /// activation: a malformed `nole-attachment://…` link is still an
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
        write!(f, "{URI_SCHEME}sha256/{}", self.0)
    }
}

/// Immutable provenance for one attachment object, persisted as JSON beside
/// the object. `id` must equal the metadata file name and `size` the object's
/// byte length; violations are reported as store inconsistency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentMetadata {
    /// Content address; must equal the digest of the object bytes.
    pub id: AttachmentId,
    /// Byte length of the object.
    pub size: u64,
    /// Original source path or name as provided at import time.
    pub source: String,
    /// Media type inferred from the source name, when recognizable.
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

/// Private on-disk layout for attachments, rooted at `Storage.attachments_dir`.
///
/// All layout knowledge lives here; callers only ever handle ids, uris,
/// metadata, and raw bytes.
#[derive(Debug, Clone)]
pub struct AttachmentStore {
    objects_dir: PathBuf,
    metadata_dir: PathBuf,
    trash_dir: PathBuf,
}

const OBJECTS_DIR: &str = "objects";
const METADATA_DIR: &str = "metadata";
const TRASH_DIR: &str = "trash";
const STREAM_BUFFER: usize = 64 * 1024;

impl AttachmentStore {
    /// Build a store rooted at `attachments_dir` (typically
    /// `Storage.attachments_dir`). Does not touch the filesystem.
    pub fn new(attachments_dir: impl Into<PathBuf>) -> Self {
        let root = attachments_dir.into();
        Self {
            objects_dir: root.join(OBJECTS_DIR),
            metadata_dir: root.join(METADATA_DIR),
            trash_dir: root.join(TRASH_DIR),
        }
    }

    /// Create `objects/`, `metadata/`, and `trash/` under the root.
    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(&self.objects_dir)
            .with_context(|| format!("creating {}", self.objects_dir.display()))?;
        fs::create_dir_all(&self.metadata_dir)
            .with_context(|| format!("creating {}", self.metadata_dir.display()))?;
        fs::create_dir_all(&self.trash_dir)
            .with_context(|| format!("creating {}", self.trash_dir.display()))?;
        Ok(())
    }

    /// Import a regular file by streaming it through SHA-256. Symlinks and
    /// non-regular sources are rejected. Importing bytes that are already
    /// stored returns the existing metadata unchanged (first import wins).
    #[cfg(test)]
    pub fn import_path(&self, source: &Path) -> Result<AttachmentMetadata> {
        self.import_path_as(source, &source.display().to_string())
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
        self.import_reader(reader, display_name)
    }

    /// Import raw bytes, optionally with a source name for provenance.
    #[cfg(test)]
    pub fn import_bytes(&self, bytes: &[u8], source: Option<&str>) -> Result<AttachmentMetadata> {
        self.import_reader(bytes, source.unwrap_or_default())
    }

    /// Stream `reader` through SHA-256 into a uniquely named staging file
    /// under `objects/`, returning the digest, byte count, and staging path.
    fn stage_object(&self, reader: &mut impl Read) -> Result<(AttachmentId, u64, PathBuf)> {
        self.ensure_layout()?;
        let staged = self.objects_dir.join(staging_name());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .with_context(|| format!("creating staging file {}", staged.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; STREAM_BUFFER];
        let mut size = 0u64;
        loop {
            let read = reader.read(&mut buffer).context("reading import source")?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .with_context(|| format!("writing staging file {}", staged.display()))?;
            size += read as u64;
        }
        file.sync_all()
            .with_context(|| format!("syncing staging file {}", staged.display()))?;
        let digest = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest);
        Ok((AttachmentId::from_bytes(bytes), size, staged))
    }

    fn import_reader(&self, mut reader: impl Read, source: &str) -> Result<AttachmentMetadata> {
        let (id, size, staged) = self.stage_object(&mut reader)?;
        self.publish(id, size, staged, source)
    }

    /// Atomically publish a staged object plus its metadata, or return the
    /// existing metadata when the object is already stored (duplicate import).
    fn publish(
        &self,
        id: AttachmentId,
        size: u64,
        staged: PathBuf,
        source: &str,
    ) -> Result<AttachmentMetadata> {
        let object_path = self.object_path(id);
        if object_path.exists() {
            fs::remove_file(&staged).ok();
        } else {
            fs::rename(&staged, &object_path)
                .with_context(|| format!("publishing object {}", object_path.display()))?;
        }
        if let Some(existing) = self.metadata_if_consistent(id)? {
            return Ok(existing);
        }
        let metadata = AttachmentMetadata {
            id,
            size,
            source: source.to_string(),
            mime_type: infer_mime_type(source),
            imported_at: Utc::now(),
        };
        let json =
            serde_json::to_vec_pretty(&metadata).context("serializing attachment metadata")?;
        let staged_metadata = self.metadata_dir.join(staging_name());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_metadata)
            .with_context(|| format!("creating staging file {}", staged_metadata.display()))?;
        file.write_all(&json)
            .with_context(|| format!("writing staging file {}", staged_metadata.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing staging file {}", staged_metadata.display()))?;
        // Last write wins if two imports of identical bytes race; both copies
        // are valid metadata for the same content.
        fs::rename(&staged_metadata, &self.metadata_path(id))
            .with_context(|| format!("publishing metadata {}", self.metadata_path(id).display()))?;
        Ok(metadata)
    }

    /// Look up stored metadata for `id`. `Ok(None)` only when the attachment
    /// is entirely absent; any half-present or inconsistent state is an error.
    pub fn lookup(&self, id: AttachmentId) -> Result<Option<AttachmentMetadata>> {
        let object = self.object_path(id);
        let metadata_path = self.metadata_path(id);
        let object_present = match fs::symlink_metadata(&object) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", object.display()));
            }
        };
        let metadata_present = match fs::symlink_metadata(&metadata_path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", metadata_path.display()));
            }
        };
        match (object_present, metadata_present) {
            (false, false) => Ok(None),
            (true, true) => Ok(Some(
                self.metadata_if_consistent(id)?.expect("metadata exists"),
            )),
            _ => bail!("attachment store inconsistent: object and metadata out of sync for {id}"),
        }
    }

    /// Stored metadata for `id`, erroring when the attachment is absent.
    pub fn metadata(&self, id: AttachmentId) -> Result<AttachmentMetadata> {
        self.lookup(id)?
            .with_context(|| format!("no such attachment: {id}"))
    }

    /// Read the object bytes for `id`. Object paths are never exposed.
    pub fn read_object(&self, id: AttachmentId) -> Result<Vec<u8>> {
        let metadata = self.metadata(id)?;
        let path = self.object_path(id);
        let bytes =
            fs::read(&path).with_context(|| format!("reading object {}", path.display()))?;
        if bytes.len() as u64 != metadata.size {
            bail!("attachment store inconsistent: object size changed for {id}");
        }
        Ok(bytes)
    }

    /// Re-hash the stored object and report whether its bytes still match
    /// `id`. Errors when the attachment is absent or the store is inconsistent.
    pub fn verify(&self, id: AttachmentId) -> Result<bool> {
        self.metadata(id)?;
        let path = self.object_path(id);
        let mut file =
            File::open(&path).with_context(|| format!("opening object {}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; STREAM_BUFFER];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let digest = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest);
        Ok(AttachmentId::from_bytes(bytes) == id)
    }

    /// Metadata for every stored attachment, sorted by content address.
    pub fn list(&self) -> Result<Vec<AttachmentMetadata>> {
        let entries = match fs::read_dir(&self.metadata_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("listing {}", self.metadata_dir.display()));
            }
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                bail!(
                    "refusing symlink in attachment store: {}",
                    entry.path().display()
                );
            }
            if !file_type.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            let Some(hex) = file_name
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
            else {
                continue;
            };
            let Ok(id) = AttachmentId::from_hex(hex) else {
                continue;
            };
            out.push(self.lookup(id)?.expect("listed metadata exists"));
        }
        out.sort_by_key(|metadata| metadata.id);
        Ok(out)
    }

    /// Move the object and metadata for `id` into `trash/`. Returns `false`
    /// when the attachment does not exist. Inconsistent state is an error.
    pub fn remove(&self, id: AttachmentId) -> Result<bool> {
        if self.lookup(id)?.is_none() {
            return Ok(false);
        }
        self.ensure_layout()?;
        self.trash(&self.object_path(id))?;
        self.trash(&self.metadata_path(id))?;
        Ok(true)
    }

    fn trash(&self, path: &Path) -> Result<()> {
        let name = path.file_name().context("path has no file name")?;
        let mut destination = self.trash_dir.join(name);
        let mut index = 1;
        while destination.exists() {
            destination = self
                .trash_dir
                .join(format!("{}-{}", name.to_string_lossy(), index));
            index += 1;
        }
        fs::rename(path, &destination)
            .with_context(|| format!("moving {} to trash", path.display()))?;
        Ok(())
    }

    /// Read and validate the metadata file for `id`, returning `None` when it
    /// does not exist. Errors on any inconsistent or hostile state.
    fn metadata_if_consistent(&self, id: AttachmentId) -> Result<Option<AttachmentMetadata>> {
        let metadata_path = self.metadata_path(id);
        let meta = match fs::symlink_metadata(&metadata_path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", metadata_path.display()));
            }
        };
        if meta.file_type().is_symlink() {
            bail!(
                "refusing symlink in attachment store: {}",
                metadata_path.display()
            );
        }
        if !meta.file_type().is_file() {
            bail!(
                "non-regular entry in attachment store: {}",
                metadata_path.display()
            );
        }
        let metadata = self.load_metadata(id)?;
        let object = self.object_path(id);
        let object_meta = fs::symlink_metadata(&object)
            .with_context(|| format!("checking object {}", object.display()))?;
        if object_meta.file_type().is_symlink() || !object_meta.file_type().is_file() {
            bail!(
                "non-regular object entry in attachment store: {}",
                object.display()
            );
        }
        if metadata.size != object_meta.len() {
            bail!(
                "attachment store inconsistent: metadata size {} does not match object size {} for {id}",
                metadata.size,
                object_meta.len()
            );
        }
        Ok(Some(metadata))
    }

    fn load_metadata(&self, id: AttachmentId) -> Result<AttachmentMetadata> {
        let path = self.metadata_path(id);
        let json =
            fs::read(&path).with_context(|| format!("reading metadata {}", path.display()))?;
        let metadata: AttachmentMetadata = serde_json::from_slice(&json)
            .with_context(|| format!("parsing metadata {}", path.display()))?;
        if metadata.id != id {
            bail!(
                "attachment store inconsistent: metadata at {} names id {} instead of {}",
                path.display(),
                metadata.id,
                id
            );
        }
        Ok(metadata)
    }

    /// Integrity-safe path derivation: `id` is validated to exactly 64
    /// lowercase hex characters before it is ever joined into a path.
    fn object_path(&self, id: AttachmentId) -> PathBuf {
        self.objects_dir.join(id.to_hex())
    }

    fn metadata_path(&self, id: AttachmentId) -> PathBuf {
        self.metadata_dir.join(format!("{}.json", id.to_hex()))
    }
}

/// Uniquely named staging file: never collides with `objects/` or `metadata/`
/// entries and never matches the strict `<64hex>.json` metadata pattern.
fn staging_name() -> String {
    format!(".tmp-{}-{:016x}", std::process::id(), fastrand::u64(..))
}

/// Best-effort media type from a source name's extension.
fn infer_mime_type(source: &str) -> Option<String> {
    let extension = Path::new(source)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, AttachmentStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(dir.path().join("attachments"));
        store.ensure_layout().unwrap();
        (dir, store)
    }

    fn valid_hex() -> String {
        "0123456789abcdef".repeat(4)
    }

    #[test]
    fn canonical_uri_parses_and_displays_verbatim() {
        let hex = valid_hex();
        let uri = AttachmentUri::parse(&format!("nole-attachment://sha256/{hex}")).unwrap();
        assert_eq!(uri.to_string(), format!("nole-attachment://sha256/{hex}"));
        assert_eq!(uri.id().to_hex(), hex);
        assert_eq!(AttachmentUri::from_id(uri.id()), uri);
        let parsed: AttachmentUri = format!("nole-attachment://sha256/{hex}").parse().unwrap();
        assert_eq!(parsed, uri);
    }

    #[test]
    fn scheme_classification_is_prefix_based() {
        let hex = valid_hex();
        assert!(AttachmentUri::is_attachment_uri(&format!(
            "nole-attachment://sha256/{hex}"
        )));
        // Malformed after the scheme is still classified as an attachment URI.
        assert!(AttachmentUri::is_attachment_uri(
            "nole-attachment://md5/bad"
        ));
        assert!(AttachmentUri::is_attachment_uri("nole-attachment://"));
        assert!(!AttachmentUri::is_attachment_uri("attachment://sha256/…"));
        assert!(!AttachmentUri::is_attachment_uri(
            "https://nole-attachment/…"
        ));
        assert!(!AttachmentUri::is_attachment_uri(""));
    }

    #[test]
    fn uri_rejects_malformed_input() {
        let hex = valid_hex();
        let cases = [
            format!("nole-attachment://sha256/{}", hex.to_uppercase()),
            format!("nole-attachment://sha256/{hex}extra"),
            format!("nole-attachment://sha256/{hex}/extra"),
            format!("nole-attachment://sha256/{}", &hex[..63]),
            format!("nole-attachment://sha256/{}", hex.replacen('a', "z", 1)),
            "nole-attachment://sha256/".to_string(),
            "nole-attachment://md5/abcdef".to_string(),
            "attachment://sha256/abcdef".to_string(),
            "nole-attachment://sha256/<64hex>".to_string(),
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
    fn id_rejects_noncanonical_hex() {
        assert!(AttachmentId::from_hex("abcd").is_err());
        assert!(AttachmentId::from_hex(&"A".repeat(64)).is_err());
        assert!(AttachmentId::from_hex(&"g".repeat(64)).is_err());
        assert!(AttachmentId::from_hex(&"0".repeat(63)).is_err());
        assert!(AttachmentId::from_hex(&"0".repeat(65)).is_err());
        let id = AttachmentId::from_hex(&"0".repeat(64)).unwrap();
        assert_eq!(id.to_hex(), "0".repeat(64));
    }

    #[test]
    fn duplicate_imports_dedupe_and_preserve_first_source() {
        let (dir, store) = fresh();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        fs::write(&first, b"shared bytes").unwrap();
        fs::write(&second, b"shared bytes").unwrap();
        let a = store.import_path(&first).unwrap();
        let b = store.import_path(&second).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.size, 12);
        assert_eq!(a.source, first.display().to_string());
        assert_eq!(b.source, a.source, "first import wins");
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.read_object(a.id).unwrap(), b"shared bytes");
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let (dir, store) = fresh();
        let source = dir.path().join("doc.pdf");
        fs::write(&source, b"%PDF-1.4 fake").unwrap();
        let imported = store.import_path(&source).unwrap();
        let json = fs::read_to_string(store.metadata_path(imported.id)).unwrap();
        let parsed: AttachmentMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, imported);
        assert_eq!(parsed.mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(store.metadata(imported.id).unwrap(), imported);
        assert_eq!(store.metadata(imported.id).unwrap().uri().id(), imported.id);
    }

    #[test]
    fn import_preserves_source_provenance() {
        let (dir, store) = fresh();
        let source = dir.path().join("report.md");
        fs::write(&source, b"# report").unwrap();
        let imported = store.import_path(&source).unwrap();
        assert_eq!(imported.source, source.display().to_string());
        assert_eq!(imported.mime_type.as_deref(), Some("text/markdown"));
        assert_eq!(imported.size, 8);
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
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn non_regular_and_missing_sources_are_rejected() {
        let (dir, store) = fresh();
        assert!(store.import_path(dir.path()).is_err());
        assert!(store.import_path(&dir.path().join("missing.txt")).is_err());
    }

    #[test]
    fn object_metadata_inconsistency_is_rejected() {
        use std::os::unix::fs::symlink;
        let (dir, store) = fresh();
        let import = |name: &str, content: &[u8]| -> AttachmentMetadata {
            let source = dir.path().join(name);
            fs::write(&source, content).unwrap();
            store.import_path(&source).unwrap()
        };
        // Metadata without object.
        let a = import("a.txt", b"data-a");
        fs::remove_file(store.object_path(a.id)).unwrap();
        assert!(store.lookup(a.id).is_err());
        assert!(store.metadata(a.id).is_err());
        assert!(store.read_object(a.id).is_err());
        // Object without metadata.
        let b = import("b.txt", b"data-b");
        fs::remove_file(store.metadata_path(b.id)).unwrap();
        assert!(store.lookup(b.id).is_err());
        // Tampered metadata size.
        let c = import("c.txt", b"data-c");
        let path = store.metadata_path(c.id);
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        json["size"] = serde_json::json!(999);
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(store.lookup(c.id).is_err());
        // Symlinked object entry.
        let d = import("d.txt", b"data-d");
        let decoy = dir.path().join("decoy");
        fs::write(&decoy, b"data").unwrap();
        fs::remove_file(store.object_path(d.id)).unwrap();
        symlink(&decoy, store.object_path(d.id)).unwrap();
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
    fn verify_detects_tampering() {
        let (dir, store) = fresh();
        let source = dir.path().join("v.txt");
        fs::write(&source, b"original").unwrap();
        let imported = store.import_path(&source).unwrap();
        assert!(store.verify(imported.id).unwrap());
        fs::write(store.object_path(imported.id), b"tampered").unwrap();
        assert!(!store.verify(imported.id).unwrap());
        fs::write(store.object_path(imported.id), b"different length").unwrap();
        assert!(store.lookup(imported.id).is_err());
    }

    #[test]
    fn remove_moves_objects_and_metadata_to_trash() {
        let (dir, store) = fresh();
        let source = dir.path().join("r.txt");
        fs::write(&source, b"remove me").unwrap();
        let imported = store.import_path(&source).unwrap();
        assert!(store.remove(imported.id).unwrap());
        assert!(store.lookup(imported.id).unwrap().is_none());
        assert!(!store.object_path(imported.id).exists());
        assert!(!store.metadata_path(imported.id).exists());
        let trash: Vec<String> = fs::read_dir(&store.trash_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(trash.len(), 2);
        assert!(trash.iter().any(|name| *name == imported.id.to_hex()));
        assert!(trash
            .iter()
            .any(|name| *name == format!("{}.json", imported.id.to_hex())));
        assert!(!store.remove(imported.id).unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn list_returns_all_imports_sorted() {
        let (dir, store) = fresh();
        let source = dir.path().join("a.txt");
        for content in [&b"one"[..], &b"two"[..], &b"three"[..]] {
            fs::write(&source, content).unwrap();
            store.import_path(&source).unwrap();
        }
        let all = store.list().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(store.lookup(all[0].id).unwrap().unwrap(), all[0]);
    }

    #[test]
    fn lookup_missing_returns_none() {
        let (_dir, store) = fresh();
        let id = AttachmentId::from_hex(&"0".repeat(64)).unwrap();
        assert!(store.lookup(id).unwrap().is_none());
        assert!(store.metadata(id).is_err());
        assert!(store.read_object(id).is_err());
        assert!(store.verify(id).is_err());
        assert!(!store.remove(id).unwrap());
    }

    #[test]
    fn empty_bytes_import_matches_sha256_of_empty_input() {
        let (_dir, store) = fresh();
        let imported = store.import_bytes(b"", Some("empty.bin")).unwrap();
        assert_eq!(imported.size, 0);
        assert_eq!(store.read_object(imported.id).unwrap(), b"");
        assert_eq!(
            imported.id.to_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn import_bytes_records_optional_provenance() {
        let (_dir, store) = fresh();
        let imported = store.import_bytes(b"bytes", None).unwrap();
        assert_eq!(imported.source, "");
        assert!(imported.mime_type.is_none());
        let other = store.import_bytes(b"other bytes", Some("note.md")).unwrap();
        assert_ne!(other.id, imported.id);
        assert_eq!(other.source, "note.md");
        assert_eq!(other.mime_type.as_deref(), Some("text/markdown"));
    }

    #[test]
    fn no_staging_files_left_behind() {
        let (dir, store) = fresh();
        let source = dir.path().join("s.txt");
        fs::write(&source, b"staging").unwrap();
        store.import_path(&source).unwrap();
        for directory in [&store.objects_dir, &store.metadata_dir] {
            let leftovers: Vec<String> = fs::read_dir(directory)
                .unwrap()
                .filter_map(|entry| {
                    let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                    name.starts_with(".tmp-").then_some(name)
                })
                .collect();
            assert!(
                leftovers.is_empty(),
                "leftover staging files: {leftovers:?}"
            );
        }
    }
}
