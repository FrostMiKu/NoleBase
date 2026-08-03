//! Attachment tools: import, list, info, copy-to-workspace, and delete.
//!
//! Attachments are mutable, application-managed files stored by
//! [`crate::attachment::AttachmentStore`] under `Storage.attachments_dir`.
//! Each attachment is one directory with a stable UUID identity and the
//! canonical URI `nole://attachment/<uuid>`; importing the same bytes twice
//! produces two distinct attachments. Attachment internals are private to the
//! store: every tool here exchanges ids, canonical URIs, metadata, and bounded
//! reads only, and tool results never print physical object paths. Deletion
//! funnels through the shared usage-checked service in
//! [`crate::attachment_usage`], never raw store removal, so an attachment
//! still referenced by a managed note can never be deleted.

use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::util::{display_path, required_string};
use super::workspace_quota::check_workspace_write;
use crate::agent::{canonical_root, ApprovalGate, ApprovalKind, ApprovalRequest, Tool};
use crate::attachment::{
    escape_markdown_label, validate_display_name, AttachmentId, AttachmentMetadata,
    AttachmentQuery, AttachmentStore, AttachmentUri,
};
use crate::attachment_usage::{AttachmentUsageHandle, TrashError, TrashResult};
use crate::storage::Storage;

const MAX_LIST_LIMIT: u64 = 200;

/// Build a store rooted at the given Nole root's attachments directory.
fn store_for(root: &Path) -> Result<AttachmentStore> {
    Ok(AttachmentStore::new(Storage::new(root)?.attachments_dir))
}

/// Resolve a user-supplied attachment reference: the canonical
/// `nole://attachment/<uuid>` URI or a bare lowercase hyphenated UUID. Every
/// accepted form is validated strictly, so malformed ids are refused before
/// any store access.
fn resolve_attachment_uri(reference: &str) -> Result<AttachmentUri> {
    if let Ok(uri) = AttachmentUri::parse(reference) {
        return Ok(uri);
    }
    let id = AttachmentId::parse(reference).with_context(|| {
        format!(
            "attachment reference must be nole://attachment/<uuid> or a bare \
             lowercase hyphenated UUID, got {reference:?}"
        )
    })?;
    Ok(AttachmentUri::from_id(id))
}

/// Resolve an import source: an existing regular file, absolute or relative
/// to the Nole root. Symlinks, non-regular entries, and attachment-store
/// internals are refused.
fn resolve_import_source(root: &Path, unresolved: &Path) -> Result<PathBuf> {
    let file_type = fs::symlink_metadata(unresolved)
        .with_context(|| format!("checking source {}", unresolved.display()))?
        .file_type();
    if file_type.is_symlink() || !file_type.is_file() {
        bail!("import source must be an existing regular file, not a symlink or directory");
    }
    let source = fs::canonicalize(unresolved)
        .with_context(|| format!("resolving source {}", unresolved.display()))?;
    if source.starts_with(Storage::new(root)?.attachments_dir) {
        bail!("import source must not be inside attachments/");
    }
    Ok(source)
}

/// Markdown for pasting into notes: an embed for images, a plain link
/// otherwise. The display name is escaped so hostile names cannot break the
/// Markdown; both forms reference only the canonical URI.
fn markdown_embed(metadata: &AttachmentMetadata) -> String {
    let uri = metadata.uri().to_string();
    let label = escape_markdown_label(&metadata.display_name);
    let is_image = metadata
        .mime_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with("image/"));
    if is_image {
        format!("![{label}]({uri})")
    } else {
        format!("[{label}]({uri})")
    }
}

/// Metadata shaped for Agent output. Never contains physical object paths.
fn attachment_metadata_json(metadata: &AttachmentMetadata) -> Value {
    json!({
        "id": metadata.id.to_string(),
        "uri": metadata.uri().to_string(),
        "display_name": metadata.display_name,
        "source": metadata.source,
        "size": metadata.size,
        "mime_type": metadata.mime_type,
        "imported_at": metadata.imported_at.to_rfc3339(),
    })
}

/// Resolve a copy destination under `workspace/main`, creating missing
/// intermediate directories without ever following a symlink or escaping the
/// sandbox. The final file must not already exist.
fn workspace_destination(root: &Path, input: &str) -> Result<PathBuf> {
    let workspace_main = Storage::new(root)?.agent_workspace_dir();
    fs::create_dir_all(&workspace_main)
        .with_context(|| format!("creating {}", workspace_main.display()))?;
    let workspace_canonical = fs::canonicalize(&workspace_main)
        .with_context(|| format!("resolving {}", workspace_main.display()))?;
    let relative = Path::new(input);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("destination must stay within workspace/main");
    }
    let file_name = relative
        .file_name()
        .context("destination must name a file")?;
    let mut current = workspace_canonical.clone();
    for component in relative
        .parent()
        .map(Path::components)
        .into_iter()
        .flatten()
    {
        let candidate = current.join(component);
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                    bail!("destination parent must be a real directory, not a symlink");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&candidate)
                    .with_context(|| format!("creating directory {}", candidate.display()))?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", candidate.display()));
            }
        }
        current = fs::canonicalize(&candidate)
            .with_context(|| format!("resolving {}", candidate.display()))?;
        if !current.starts_with(&workspace_canonical) {
            bail!("destination escapes workspace/main");
        }
    }
    let destination = current.join(file_name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => bail!("destination already exists: {input}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(error).with_context(|| format!("checking destination {input}")),
    }
}

pub struct ImportAttachment {
    root: PathBuf,
    store: AttachmentStore,
}

impl ImportAttachment {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            store: store_for(&root)?,
            root,
        })
    }
}

#[async_trait::async_trait]
impl Tool for ImportAttachment {
    fn name(&self) -> &'static str {
        "import_attachment"
    }

    fn description(&self) -> &'static str {
        "Import an existing regular file (absolute or Nole-relative path) into the attachment store. The source is copied and never modified. Every import creates a NEW mutable attachment with its own stable UUID identity, so importing the same content twice yields two distinct attachments. Returns the canonical nole://attachment/<uuid> URI plus a Markdown embed (images) or link (other files) for notes."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Absolute or Nole-relative path of an existing regular file to import"
                },
                "display_name": {
                    "type": "string",
                    "description": "Optional bare file name stored as the attachment's display name and media-type hint"
                },
                "media_type": {
                    "type": "string",
                    "description": "Optional media type, accepted only when it matches the type detected from the content"
                }
            },
            "required": ["source"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let source_text = required_string(input, "source")?;
        let unresolved = if Path::new(source_text).is_absolute() {
            PathBuf::from(source_text)
        } else {
            self.root.join(source_text)
        };
        let source = resolve_import_source(&self.root, &unresolved)?;
        let default_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .context("source file name must be valid UTF-8")?;
        let display_name = match input.get("display_name").and_then(Value::as_str) {
            Some(name) => validate_display_name(name)?,
            None => validate_display_name(default_name)?,
        };
        let metadata = self.store.import_path_as(&source, &display_name)?;
        if let Some(requested) = input.get("media_type").and_then(Value::as_str) {
            let stored = metadata.mime_type.as_deref().unwrap_or("none");
            if stored != requested {
                bail!(
                    "media type {requested:?} does not match the type detected from the content; detected media type is {stored}"
                );
            }
        }
        let mut result = attachment_metadata_json(&metadata);
        result["markdown"] = json!(markdown_embed(&metadata));
        serde_json::to_string_pretty(&result).context("encoding attachment metadata")
    }
}

pub struct ListAttachments {
    store: AttachmentStore,
}

impl ListAttachments {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            store: store_for(&root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for ListAttachments {
    fn name(&self) -> &'static str {
        "list_attachments"
    }

    fn description(&self) -> &'static str {
        "List stored attachments with metadata (canonical URI, display name, size, media type, import time), paginated, filtered, and sorted. Defaults to the 50 most recently imported. Attachment object paths are never exposed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional case-insensitive substring filter on display name and import source"
                },
                "offset": {
                    "type": "integer", "minimum": 0, "default": 0,
                    "description": "Number of matching attachments to skip"
                },
                "limit": {
                    "type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT, "default": 50,
                    "description": "Maximum number of matching attachments to return"
                },
                "sort_by": {
                    "type": "string",
                    "enum": ["imported_at", "name", "size", "type"],
                    "default": "imported_at",
                    "description": "Sort key"
                },
                "order": {
                    "type": "string",
                    "enum": ["asc", "desc"],
                    "default": "desc",
                    "description": "Sort direction"
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let query: AttachmentQuery =
            serde_json::from_value(input.clone()).context("invalid list_attachments parameters")?;
        if query.limit == 0 || query.limit > MAX_LIST_LIMIT {
            bail!("limit must be between 1 and {MAX_LIST_LIMIT}");
        }
        let page = self.store.list(&query)?;
        serde_json::to_string_pretty(&json!({
            "count": page.items.len(),
            "total": page.total,
            "offset": page.offset,
            "limit": page.limit,
            "has_more": page.has_more,
            "items": page.items.iter().map(attachment_metadata_json).collect::<Vec<_>>(),
        }))
        .context("encoding attachment list")
    }
}

pub struct AttachmentInfo {
    store: AttachmentStore,
}

impl AttachmentInfo {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            store: store_for(&root)?,
        })
    }
}

#[async_trait::async_trait]
impl Tool for AttachmentInfo {
    fn name(&self) -> &'static str {
        "attachment_info"
    }

    fn description(&self) -> &'static str {
        "Show metadata for one attachment (canonical URI, display name, size, media type, import time). Attachment object paths are never exposed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "Canonical attachment URI (nole://attachment/<uuid>) or bare lowercase hyphenated UUID"
                }
            },
            "required": ["uri"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let reference = required_string(input, "uri")?;
        let uri = resolve_attachment_uri(reference)?;
        let metadata = self.store.metadata(uri.id())?;
        serde_json::to_string_pretty(&attachment_metadata_json(&metadata))
            .context("encoding attachment metadata")
    }
}

pub struct CopyAttachmentToWorkspace {
    root: PathBuf,
    store: AttachmentStore,
}

impl CopyAttachmentToWorkspace {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            store: store_for(&root)?,
            root,
        })
    }
}

#[async_trait::async_trait]
impl Tool for CopyAttachmentToWorkspace {
    fn name(&self) -> &'static str {
        "copy_attachment_to_workspace"
    }

    fn description(&self) -> &'static str {
        "Copy an attachment's bytes into a NEW file under workspace/main (the Agent workspace sandbox), where the generic file tools may edit it. The copy is a separate file: editing or deleting it later never changes the original attachment, and importing the edited copy creates a new attachment. The destination is relative to workspace/main, must not already exist, and must respect the workspace size limits."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "Canonical attachment URI (nole://attachment/<uuid>) or bare lowercase hyphenated UUID"
                },
                "destination": {
                    "type": "string",
                    "description": "New file path relative to workspace/main"
                }
            },
            "required": ["uri", "destination"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let reference = required_string(input, "uri")?;
        let destination_text = required_string(input, "destination")?;
        let uri = resolve_attachment_uri(reference)?;
        let metadata = self.store.metadata(uri.id())?;
        let destination = workspace_destination(&self.root, destination_text)?;
        check_workspace_write(&self.root, &destination, metadata.size)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .with_context(|| format!("creating destination {}", destination.display()))?;
        let copied = match self.store.copy_to(uri.id(), &mut output) {
            Ok(bytes) => bytes,
            Err(error) => {
                drop(output);
                let _ = fs::remove_file(&destination);
                return Err(error)
                    .with_context(|| format!("writing destination {}", destination.display()));
            }
        };
        serde_json::to_string_pretty(&json!({
            "path": display_path(&self.root, &destination),
            "bytes": copied,
            "uri": uri.to_string(),
        }))
        .context("encoding copy result")
    }
}

/// Map a shared deletion-service refusal onto a tool error message.
fn trash_error_message(uri: &AttachmentUri, error: TrashError) -> anyhow::Error {
    match error {
        TrashError::Referenced { locations } => anyhow::anyhow!(
            "cannot delete attachment {uri}: still referenced by {} note(s); \
             remove those references first ({})",
            locations.len(),
            locations
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TrashError::NotReady => anyhow::anyhow!(
            "attachment reference index is not ready yet; retry deletion in a moment"
        ),
        TrashError::Stale { current_revision } => anyhow::anyhow!(
            "attachment references changed since the request (index at revision \
             {current_revision}); review the current state and retry"
        ),
        TrashError::Store(message) => anyhow::anyhow!("{message}"),
    }
}

pub struct DeleteAttachment {
    root: PathBuf,
    store: AttachmentStore,
    gate: ApprovalGate,
    usage: AttachmentUsageHandle,
}

impl DeleteAttachment {
    pub fn new(root: &Path, gate: ApprovalGate, usage: AttachmentUsageHandle) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            store: store_for(&root)?,
            root,
            gate,
            usage,
        })
    }
}

#[async_trait::async_trait]
impl Tool for DeleteAttachment {
    fn name(&self) -> &'static str {
        "delete_attachment"
    }

    fn description(&self) -> &'static str {
        "Move an attachment to trash, refusing while any managed note still references it."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "Canonical attachment URI (nole://attachment/<uuid>) or bare lowercase hyphenated UUID"
                }
            },
            "required": ["uri"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let reference = required_string(input, "uri")?;
        let uri = resolve_attachment_uri(reference)?;
        let metadata = self.store.metadata(uri.id())?;
        let usage_snapshot = self.usage.snapshot();
        if !usage_snapshot.ready {
            bail!("{}", TrashError::NotReady);
        }
        let expected_revision = usage_snapshot.revision;
        self.gate
            .request(ApprovalRequest {
                title: "Delete attachment".to_string(),
                message: format!(
                    "Delete attachment {uri} ({}), {} bytes? It is moved to trash and \
                     refused while any note still references it.",
                    metadata.display_name, metadata.size
                ),
                kind: ApprovalKind::Confirm,
            })
            .await?;
        let storage = Storage::new(&self.root)?;
        let result = self
            .usage
            .trash(&self.store, &storage, uri.id(), expected_revision)
            .map_err(|error| trash_error_message(&uri, error))?;
        match result {
            TrashResult::Trashed => Ok(format!("deleted attachment {uri} (moved to trash)")),
            TrashResult::NotFound => bail!("attachment no longer exists"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use super::*;
    use crate::agent::test_support::{
        bypass_gate, event_channel, test_runtime, TestFutureResultExt,
    };
    use crate::agent::{AgentEvent, ApprovalDecision};
    use crate::attachment_index::AttachmentReferenceIndex;

    /// A valid 1x1 transparent PNG so content-based media detection reports
    /// image/png.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// A well-formed v4-format UUID that is not stored, for unknown-attachment
    /// and malformed-reference cases that must reach the store.
    const UNKNOWN_UUID: &str = "00000000-0000-4000-8000-000000000000";

    fn fresh_root() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        (directory, storage.root)
    }

    fn outside_file(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join(name);
        fs::write(&path, bytes).unwrap();
        (outside, path)
    }

    fn import_source(import: &ImportAttachment, source: &Path) -> String {
        let result: Value =
            serde_json::from_str(&import.execute(&json!({"source": source})).unwrap()).unwrap();
        result["uri"].as_str().unwrap().to_string()
    }

    /// A usage handle whose snapshot is ready at revision 0 against the
    /// current managed notes, so `delete_attachment` reaches the trash path.
    fn ready_usage(root: &Path) -> AttachmentUsageHandle {
        let storage = Storage::new(root).unwrap();
        let usage = AttachmentUsageHandle::new();
        usage.publish_snapshot(0, AttachmentReferenceIndex::build(&storage));
        usage
    }

    #[test]
    fn import_preserves_source_and_creates_distinct_identities() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("photo.png", TINY_PNG);
        let import = ImportAttachment::new(&root).unwrap();

        let first: Value =
            serde_json::from_str(&import.execute(&json!({"source": source})).unwrap()).unwrap();
        assert!(source.exists(), "import must preserve the source file");
        let uri = first["uri"].as_str().unwrap().to_string();
        assert!(uri.starts_with("nole://attachment/"));
        assert_eq!(uri.len(), "nole://attachment/".len() + 36);
        assert_eq!(first["display_name"], "photo.png");
        assert_eq!(first["mime_type"], "image/png");
        assert_eq!(first["size"], TINY_PNG.len() as u64);
        assert!(first["markdown"].as_str().unwrap().contains(&uri));
        assert!(first["markdown"]
            .as_str()
            .unwrap()
            .starts_with("![photo.png]("));

        // Duplicate bytes import to a NEW attachment with its own identity.
        let second: Value =
            serde_json::from_str(&import.execute(&json!({"source": source})).unwrap()).unwrap();
        assert_ne!(
            second["uri"], first["uri"],
            "duplicate import must create a distinct attachment"
        );
        assert!(source.exists());

        let list = ListAttachments::new(&root).unwrap();
        let listed: Value = serde_json::from_str(&list.execute(&json!({})).unwrap()).unwrap();
        assert_eq!(listed["count"], 2, "each import stores its own attachment");
        assert_eq!(listed["total"], 2);
    }

    #[test]
    fn import_accepts_relative_sources_and_validated_metadata() {
        let (_directory, root) = fresh_root();
        fs::write(root.join("data/note.md"), "# imported\n").unwrap();
        let import = ImportAttachment::new(&root).unwrap();

        let result: Value =
            serde_json::from_str(&import.execute(&json!({"source": "data/note.md"})).unwrap())
                .unwrap();
        assert_eq!(result["display_name"], "note.md");
        assert_eq!(result["mime_type"], "text/markdown");
        assert!(result["markdown"]
            .as_str()
            .unwrap()
            .starts_with("[note.md]("));

        // Re-importing identical bytes with a new name produces a distinct
        // attachment carrying that name (display name belongs to the import).
        let renamed: Value = serde_json::from_str(
            &import
                .execute(&json!({
                    "source": "data/note.md",
                    "display_name": "renamed.png"
                }))
                .unwrap(),
        )
        .unwrap();
        assert_ne!(renamed["uri"], result["uri"]);
        assert_eq!(renamed["display_name"], "renamed.png");
        assert_eq!(
            renamed["mime_type"], "text/markdown",
            "content detection still sees markdown text despite the .png name"
        );

        // Explicit display name and matching media type are accepted when the
        // bytes really are an image.
        fs::write(root.join("data/art.png"), TINY_PNG).unwrap();
        let custom: Value = serde_json::from_str(
            &import
                .execute(&json!({
                    "source": "data/art.png",
                    "display_name": "renamed.png",
                    "media_type": "image/png"
                }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(custom["display_name"], "renamed.png");
        assert_eq!(custom["mime_type"], "image/png");
        assert!(custom["markdown"]
            .as_str()
            .unwrap()
            .starts_with("![renamed.png]("));

        // Unsafe display names and non-matching media types are refused.
        assert!(import
            .execute(&json!({"source": "data/note.md", "display_name": "a/b.png"}))
            .returns_err());
        assert!(import
            .execute(&json!({"source": "data/note.md", "display_name": ".."}))
            .returns_err());
        assert!(import
            .execute(&json!({"source": "data/note.md", "media_type": "image/png"}))
            .returns_err());
        assert!(import
            .execute(&json!({"source": "data/note.md", "display_name": "x.md", "media_type": "image/png"}))
            .returns_err());
        assert!(import
            .execute(&json!({"source": "data/note.md", "media_type": "image/../../etc"}))
            .returns_err());
    }

    #[test]
    fn import_refuses_symlinks_directories_and_attachment_internals() {
        let (_directory, root) = fresh_root();
        let import = ImportAttachment::new(&root).unwrap();
        let (_outside, source) = outside_file("plain.txt", b"bytes");
        fs::remove_file(&source).unwrap();
        fs::create_dir(&source).unwrap();
        assert!(import.execute(&json!({"source": source})).returns_err());
        fs::remove_dir(&source).unwrap();
        fs::write(&source, b"bytes").unwrap();
        #[cfg(unix)]
        {
            let link = source.with_extension("lnk");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert!(import.execute(&json!({"source": link})).returns_err());
        }
        let internal = root.join("attachments");
        fs::write(internal.join("probe.txt"), b"bytes").unwrap();
        assert!(import
            .execute(&json!({"source": internal.join("probe.txt")}))
            .returns_err());
        assert!(import
            .execute(&json!({"source": "attachments/probe.txt"}))
            .returns_err());
    }

    #[test]
    fn list_and_info_never_leak_object_paths() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("report.pdf", b"%PDF-1.4 fake");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);

        let root_text = root.to_string_lossy().to_string();
        let info = AttachmentInfo::new(&root).unwrap();
        let info_text = info.execute(&json!({"uri": uri})).unwrap();
        assert!(info_text.contains("\"display_name\": \"report.pdf\""));
        assert!(!info_text.contains(&root_text));
        assert!(!info_text.contains("objects"));
        assert!(!info_text.contains("metadata"));
        assert!(!info_text.contains("attachments"));

        let list = ListAttachments::new(&root).unwrap();
        let list_text = list.execute(&json!({})).unwrap();
        assert!(!list_text.contains(&root_text));
        assert!(!list_text.contains("objects"));
        assert!(!list_text.contains("metadata"));
        assert!(!list_text.contains("attachments"));
        assert!(list_text.contains(&uri));

        // Bare lowercase hyphenated UUIDs are accepted as references.
        let bare = uri.trim_start_matches("nole://attachment/");
        info.execute(&json!({"uri": bare})).unwrap();
        assert!(info
            .execute(&json!({"uri": "nole://attachment/not-a-uuid"}))
            .returns_err());
        assert!(info
            .execute(&json!({"uri": "nole://attachment/AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA"}))
            .returns_err());
        assert!(info
            .execute(&json!({"uri": "nole://attachment/00000000000000000000000000000000"}))
            .returns_err());
        // A well-formed but unknown UUID reaches the store and reports absence.
        assert!(info
            .execute(&json!({"uri": format!("nole://attachment/{UNKNOWN_UUID}")}))
            .returns_err());
    }

    #[test]
    fn copy_to_workspace_stays_inside_workspace_main() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("plan.pdf", b"%PDF-1.4 plan bytes");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);
        let copy = CopyAttachmentToWorkspace::new(&root).unwrap();

        let result: Value = serde_json::from_str(
            &copy
                .execute(&json!({"uri": uri, "destination": "docs/plan.pdf"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["path"], "workspace/main/docs/plan.pdf");
        assert_eq!(result["bytes"], 19);
        let written = root.join("workspace/main/docs/plan.pdf");
        assert_eq!(fs::read(&written).unwrap(), b"%PDF-1.4 plan bytes");

        // Existing destinations, traversal, absolute paths, and symlinked
        // parents are all refused.
        assert!(copy
            .execute(&json!({"uri": uri, "destination": "docs/plan.pdf"}))
            .returns_err());
        assert!(copy
            .execute(&json!({"uri": uri, "destination": "../escape.pdf"}))
            .returns_err());
        assert!(copy
            .execute(&json!({"uri": uri, "destination": "docs/../../escape.pdf"}))
            .returns_err());
        assert!(copy
            .execute(&json!({"uri": uri, "destination": "/tmp/escape.pdf"}))
            .returns_err());
        assert!(copy
            .execute(&json!({"uri": uri, "destination": "."}))
            .returns_err());
        assert!(copy
            .execute(&json!({"uri": uri, "destination": "docs/"}))
            .returns_err());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("workspace/main/linked")).unwrap();
            assert!(copy
                .execute(&json!({"uri": uri, "destination": "linked/x.pdf"}))
                .returns_err());
        }
        // A different new destination still works after the first copy.
        let again: Value = serde_json::from_str(
            &copy
                .execute(&json!({"uri": uri, "destination": "plan-copy.pdf"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(again["path"], "workspace/main/plan-copy.pdf");
        assert_eq!(
            fs::read(root.join("workspace/main/plan-copy.pdf")).unwrap(),
            b"%PDF-1.4 plan bytes"
        );

        // Unknown attachments are refused before writing anything.
        let unknown = format!("nole://attachment/{UNKNOWN_UUID}");
        assert!(copy
            .execute(&json!({"uri": unknown, "destination": "x.pdf"}))
            .returns_err());
        assert!(!root.join("workspace/main/x.pdf").exists());
    }

    #[test]
    fn list_attachments_paginates_filters_and_sorts() {
        let (_directory, root) = fresh_root();
        let import = ImportAttachment::new(&root).unwrap();
        for (name, bytes) in [
            ("alpha.md", &b"# alpha"[..]),
            ("beta.md", &b"# beta"[..]),
            ("gamma.md", &b"# gamma"[..]),
        ] {
            let (_outside, source) = outside_file(name, bytes);
            import.execute(&json!({"source": source})).unwrap();
        }
        let list = ListAttachments::new(&root).unwrap();

        // Defaults: limit 50, imported_at desc.
        let all: Value = serde_json::from_str(&list.execute(&json!({})).unwrap()).unwrap();
        assert_eq!(all["count"], 3);
        assert_eq!(all["total"], 3);
        assert_eq!(all["limit"], 50);
        assert_eq!(all["has_more"], false);

        // Case-insensitive substring filter on display name.
        let filtered: Value =
            serde_json::from_str(&list.execute(&json!({"query": "ALP"})).unwrap()).unwrap();
        assert_eq!(filtered["count"], 1);
        assert_eq!(filtered["items"][0]["display_name"], "alpha.md");

        // Limit and offset paginate; the first page reports has_more.
        let first_page: Value =
            serde_json::from_str(&list.execute(&json!({"limit": 2})).unwrap()).unwrap();
        assert_eq!(first_page["count"], 2);
        assert_eq!(first_page["total"], 3);
        assert_eq!(first_page["has_more"], true);
        let second_page: Value =
            serde_json::from_str(&list.execute(&json!({"limit": 2, "offset": 2})).unwrap())
                .unwrap();
        assert_eq!(second_page["count"], 1);
        assert_eq!(second_page["has_more"], false);

        // Sort by name ascending.
        let sorted: Value = serde_json::from_str(
            &list
                .execute(&json!({"sort_by": "name", "order": "asc"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(sorted["items"][0]["display_name"], "alpha.md");
        assert_eq!(sorted["items"][2]["display_name"], "gamma.md");

        // Invalid sort keys are refused.
        assert!(list.execute(&json!({"sort_by": "bogus"})).returns_err());
        assert!(list.execute(&json!({"order": "sideways"})).returns_err());
        assert!(list.execute(&json!({"limit": 0})).returns_err());
        assert!(list
            .execute(&json!({"limit": MAX_LIST_LIMIT + 1}))
            .returns_err());
    }

    #[test]
    fn delete_attachment_waits_for_approval_and_denial_preserves() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("keep.txt", b"keep me");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);

        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let delete = DeleteAttachment::new(&root, gate, ready_usage(&root)).unwrap();

        // Malformed references are refused without any approval request.
        assert!(delete
            .execute(&json!({"uri": "nole://attachment/XYZ"}))
            .returns_err());
        assert!(
            event_receiver.try_recv().is_err(),
            "malformed reference must not request approval"
        );

        // A denial preserves the attachment.
        let worker_uri = uri.clone();
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(delete.execute(&json!({"uri": worker_uri})))
        });
        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert_eq!(request.kind, ApprovalKind::Confirm);
        assert_eq!(request.title, "Delete attachment");
        assert!(request.message.contains(&uri));
        decision_sender.send(ApprovalDecision::Deny).unwrap();
        let error = worker.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("denied"));

        let info = AttachmentInfo::new(&root).unwrap();
        info.execute(&json!({"uri": uri})).unwrap();
    }

    #[test]
    fn delete_attachment_moves_to_trash_after_approval() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("gone.txt", b"gone");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);

        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let delete = DeleteAttachment::new(&root, gate, ready_usage(&root)).unwrap();
        let worker_uri = uri.clone();
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(delete.execute(&json!({"uri": worker_uri})))
        });
        let AgentEvent::Approval(_) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        let message = worker.join().unwrap().unwrap();
        assert!(message.contains("moved to trash"));

        let list = ListAttachments::new(&root).unwrap();
        let listed: Value = serde_json::from_str(&list.execute(&json!({})).unwrap()).unwrap();
        assert_eq!(
            listed["count"], 0,
            "deleted attachment must leave the store"
        );
        let trash = root.join("attachments/trash");
        assert!(
            fs::read_dir(&trash).unwrap().next().is_some(),
            "deleted attachment must land in trash"
        );
        // Deleting again reports the attachment as gone without another approval.
        let delete_again = DeleteAttachment::new(&root, bypass_gate(), ready_usage(&root)).unwrap();
        assert!(delete_again.execute(&json!({"uri": uri})).returns_err());
    }

    #[test]
    fn delete_attachment_refuses_while_referenced() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("cited.txt", b"cited");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);
        fs::write(root.join("data/Note.md"), format!("[cited]({uri})\n")).unwrap();

        let delete = DeleteAttachment::new(&root, bypass_gate(), ready_usage(&root)).unwrap();
        let error = delete.execute(&json!({"uri": uri})).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("referenced"), "got: {message}");
        assert!(message.contains("1 note"), "got: {message}");

        // The attachment is untouched and still listed.
        let list = ListAttachments::new(&root).unwrap();
        let listed: Value = serde_json::from_str(&list.execute(&json!({})).unwrap()).unwrap();
        assert_eq!(listed["count"], 1);
    }

    #[test]
    fn bypassed_approval_gate_allows_immediate_delete() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("temp.txt", b"temp");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);
        let delete = DeleteAttachment::new(&root, bypass_gate(), ready_usage(&root)).unwrap();
        let message = delete.execute(&json!({"uri": uri})).unwrap();
        assert!(message.contains("moved to trash"));
        let list = ListAttachments::new(&root).unwrap();
        let listed: Value = serde_json::from_str(&list.execute(&json!({})).unwrap()).unwrap();
        assert_eq!(listed["count"], 0);
    }
}
