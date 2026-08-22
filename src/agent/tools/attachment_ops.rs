//! Attachment tools: import, list, info, checkout, update, and delete.
//!
//! Attachments are mutable, application-managed files stored by
//! [`crate::attachment::AttachmentStore`] under `Storage.attachments_dir`.
//! Each attachment is one directory with a stable UUID identity and the
//! canonical URI `nole://attachment/<uuid>`; importing the same bytes twice
//! produces two distinct attachments. Attachment internals are private to the
//! store: every tool here exchanges ids, canonical URIs, metadata, bounded
//! reads, and deterministic content tokens only; tool results expose metadata
//! rather than physical object paths. Editing an existing attachment is checkout
//! (materialize a separate workspace copy plus its `sha256:<hex>` content
//! token) followed by update (publish the edited copy back to the same
//! identity after approval, with a current-content match). Deletion funnels
//! through the shared usage-checked service in [`crate::attachment_usage`],
//! which protects managed-note references before moving content to trash.

use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::util::{display_path, range_schema, required_string, RangeSelector};
use super::workspace_quota::{
    check_workspace_write, workspace_destination, MAX_WORKSPACE_FILE_BYTES,
};
use crate::agent::{canonical_root, ApprovalGate, ApprovalKind, ApprovalRequest, Tool};
use crate::attachment::{
    markdown_embed, validate_display_name, AttachmentId, AttachmentMetadata, AttachmentQuery,
    AttachmentStore, AttachmentUri, MAX_ATTACHMENT_SIZE,
};
use crate::attachment_usage::{AttachmentUsageHandle, TrashError, TrashResult};
use crate::storage::Storage;

const MAX_LIST_LIMIT: usize = 200;

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

/// Metadata shaped for Agent output. It contains stable identifiers and
/// presentation metadata while keeping storage locations private.
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

/// Resolve an update source under `workspace/main`: an existing regular file
/// whose path follows real filesystem entries and remains inside the sandbox.
/// Every component exists and is a real directory (the final one a regular
/// file); each step is canonicalized and checked against the workspace root so
/// the resolved read stays inside the sandbox.
fn workspace_source(root: &Path, input: &str) -> Result<PathBuf> {
    let workspace_main = Storage::new(root)?.agent_workspace_dir();
    match fs::symlink_metadata(&workspace_main) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
        Ok(_) => bail!("workspace/main must be a real directory, not a symlink"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("workspace/main does not exist; check the attachment out first")
        }
        Err(error) => {
            return Err(error).with_context(|| format!("checking {}", workspace_main.display()));
        }
    }
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
        bail!("source must stay within workspace/main");
    }
    let mut current = workspace_canonical.clone();
    for component in relative.components() {
        let candidate = current.join(component);
        let metadata = fs::symlink_metadata(&candidate)
            .with_context(|| format!("checking {}", candidate.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("source must not contain symlinks: {input}");
        }
        current = fs::canonicalize(&candidate)
            .with_context(|| format!("resolving {}", candidate.display()))?;
        if !current.starts_with(&workspace_canonical) {
            bail!("source escapes workspace/main: {input}");
        }
    }
    let metadata = fs::symlink_metadata(&current)
        .with_context(|| format!("checking {}", current.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("source must be an existing regular file, not a symlink or directory");
    }
    Ok(current)
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
        "Create a new attachment by copying an existing regular file; returns its canonical URI and a Markdown embed or link."
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
        "List attachment metadata with optional filtering, sorting, and pagination."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Optional case-insensitive substring filter on display name and import source"
                },
                "range": range_schema(MAX_LIST_LIMIT),
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
        let selector = RangeSelector::from_input(input, MAX_LIST_LIMIT)?;
        let mut query_input = input.clone();
        let object = query_input
            .as_object_mut()
            .context("list_attachments input must be an object")?;
        object.remove("range");
        object.insert("offset".to_string(), json!(selector.start - 1));
        object.insert(
            "limit".to_string(),
            json!(selector.end - selector.start + 1),
        );
        let query: AttachmentQuery =
            serde_json::from_value(query_input).context("invalid list_attachments parameters")?;
        let store_page = self.store.list(&query)?;
        let page = selector.window(usize::try_from(store_page.total).unwrap_or(usize::MAX));
        let mut result = json!({
            "range": selector.as_string(),
            "returned": store_page.items.len(),
            "total": store_page.total,
            "has_more": store_page.has_more,
            "items": store_page.items.iter().map(attachment_metadata_json).collect::<Vec<_>>(),
        });
        if let Some(next) = page.next() {
            result["next"] = json!(next);
        }
        serde_json::to_string_pretty(&result).context("encoding attachment list")
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
        "Show metadata for one attachment using stable identifiers and presentation fields."
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

pub struct CheckoutAttachment {
    root: PathBuf,
    store: AttachmentStore,
}

impl CheckoutAttachment {
    pub fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            store: store_for(&root)?,
            root,
        })
    }
}

#[async_trait::async_trait]
impl Tool for CheckoutAttachment {
    fn name(&self) -> &'static str {
        "checkout_attachment"
    }

    fn description(&self) -> &'static str {
        "Materialize an attachment as a new editable file in workspace/main; returns the content token required to publish changes to the same attachment."
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
        let (copied, token) = match self.store.copy_to_with_token_limited(
            uri.id(),
            &mut output,
            MAX_WORKSPACE_FILE_BYTES,
        ) {
            Ok(result) => result,
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
            "content_token": token,
        }))
        .context("encoding checkout result")
    }
}

pub struct UpdateAttachment {
    root: PathBuf,
    store: AttachmentStore,
    gate: ApprovalGate,
}

impl UpdateAttachment {
    pub fn new(root: &Path, gate: ApprovalGate) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            store: store_for(&root)?,
            root,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for UpdateAttachment {
    fn name(&self) -> &'static str {
        "update_attachment"
    }

    fn description(&self) -> &'static str {
        "Publish a workspace/main file over the same attachment using its checkout content token; atomically refuses stale content."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "Canonical attachment URI (nole://attachment/<uuid>) or bare lowercase hyphenated UUID"
                },
                "source": {
                    "type": "string",
                    "description": "Existing file path relative to workspace/main containing the new content"
                },
                "expected_content_token": {
                    "type": "string",
                "description": "The sha256:<hex> content token returned by checkout_attachment; the current attachment content must match this token"
                }
            },
            "required": ["uri", "source", "expected_content_token"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let reference = required_string(input, "uri")?;
        let source_text = required_string(input, "source")?;
        let expected_token = required_string(input, "expected_content_token")?;
        let uri = resolve_attachment_uri(reference)?;
        // Source safety and workspace/attachment limits are validated before
        // any approval request; a refusal never asks the user.
        let source = workspace_source(&self.root, source_text)?;
        let source_len = fs::metadata(&source)
            .with_context(|| format!("reading metadata for {}", source.display()))?
            .len();
        if source_len > MAX_WORKSPACE_FILE_BYTES {
            bail!(
                "source {} exceeds the 64 MiB workspace per-file limit",
                source.display()
            );
        }
        if source_len > MAX_ATTACHMENT_SIZE {
            bail!(
                "source {} exceeds the attachment {} byte limit",
                source.display(),
                MAX_ATTACHMENT_SIZE
            );
        }
        let metadata = self.store.metadata(uri.id())?;
        self.gate
            .request(ApprovalRequest {
                title: "Update attachment content".to_string(),
                message: format!(
                    "Replace the content of attachment {uri} ({}), {} bytes, with the {} bytes \
                     from workspace/main/{source_text}? The attachment keeps its URI and identity, \
                     so every existing note reference observes the updated content.",
                    metadata.display_name, metadata.size, source_len
                ),
                kind: ApprovalKind::Confirm,
            })
            .await?;
        let reader =
            File::open(&source).with_context(|| format!("opening source {}", source.display()))?;
        let (updated, token) = self.store.replace_content(
            uri.id(),
            expected_token,
            reader,
            MAX_WORKSPACE_FILE_BYTES,
        )?;
        let mut result = attachment_metadata_json(&updated);
        result["content_token"] = json!(token);
        serde_json::to_string_pretty(&result).context("encoding attachment metadata")
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
        "Move an attachment to trash after confirming that managed notes have released it."
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
                kind: ApprovalKind::DestructiveConfirm,
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
    use super::*;
    use crate::agent::test_support::{
        bypass_gate, event_channel, gate, test_runtime, TestFutureResultExt,
    };
    use crate::agent::{AgentEvent, ApprovalDecision, PermissionMode};
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
        assert_eq!(
            listed["returned"], 2,
            "each import stores its own attachment"
        );
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
        let copy = CheckoutAttachment::new(&root).unwrap();

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
    fn checkout_edit_and_update_preserve_identity_and_refuse_stale_content() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("draft.txt", b"original");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);
        let id = AttachmentUri::parse(&uri).unwrap().id();
        let store = store_for(&root).unwrap();
        let original = store.metadata(id).unwrap();

        let checkout = CheckoutAttachment::new(&root).unwrap();
        let checked_out: Value = serde_json::from_str(
            &checkout
                .execute(&json!({"uri": uri, "destination": "draft.txt"}))
                .unwrap(),
        )
        .unwrap();
        let token = checked_out["content_token"].as_str().unwrap();
        assert_eq!(
            token,
            "sha256:0682c5f2076f099c34cfdd15a9e063849ed437a49677e6fcc5b4198c76575be5"
        );
        fs::write(root.join("workspace/main/draft.txt"), b"agent edit").unwrap();

        let update = UpdateAttachment::new(&root, bypass_gate()).unwrap();
        let updated: Value = serde_json::from_str(
            &update
                .execute(&json!({
                    "uri": uri,
                    "source": "draft.txt",
                    "expected_content_token": token,
                }))
                .unwrap(),
        )
        .unwrap();
        let after = store.metadata(id).unwrap();
        assert_eq!(updated["uri"], uri);
        assert_eq!(after.id, original.id);
        assert_eq!(after.display_name, original.display_name);
        assert_eq!(after.source, original.source);
        assert_eq!(after.imported_at, original.imported_at);
        assert_eq!(fs::read(store.open(id).unwrap()).unwrap(), b"agent edit");
        assert_ne!(updated["content_token"], token);

        let current_token = updated["content_token"].as_str().unwrap();
        fs::write(root.join("workspace/main/draft.txt"), b"stale overwrite").unwrap();
        fs::write(store.open(id).unwrap(), b"concurrent user edit").unwrap();
        let error = update
            .execute(&json!({
                "uri": uri,
                "source": "draft.txt",
                "expected_content_token": current_token,
            }))
            .unwrap_err();
        assert!(error.to_string().contains("changed since checkout"));
        assert_eq!(
            fs::read(store.open(id).unwrap()).unwrap(),
            b"concurrent user edit"
        );
    }

    #[test]
    fn update_attachment_waits_for_approval_and_denial_preserves_content() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("keep.txt", b"original");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);
        let id = AttachmentUri::parse(&uri).unwrap().id();
        let store = store_for(&root).unwrap();
        let token = store.content_token(id).unwrap();
        fs::write(root.join("workspace/main/keep.txt"), b"unapproved edit").unwrap();

        let unsafe_update = UpdateAttachment::new(&root, bypass_gate()).unwrap();
        assert!(unsafe_update
            .execute(&json!({
                "uri": uri.clone(),
                "source": "../keep.txt",
                "expected_content_token": token.clone(),
            }))
            .returns_err());
        assert_eq!(fs::read(store.open(id).unwrap()).unwrap(), b"original");

        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = gate(
            PermissionMode::Approve,
            &root,
            event_sender,
            decision_receiver,
        );
        let update = UpdateAttachment::new(&root, gate).unwrap();
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(update.execute(&json!({
                "uri": uri,
                "source": "keep.txt",
                "expected_content_token": token,
            })))
        });
        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert_eq!(request.title, "Update attachment content");
        assert!(request.message.contains("every existing note reference"));
        decision_sender.send(ApprovalDecision::Deny).unwrap();
        assert!(worker
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("denied"));
        assert_eq!(fs::read(store.open(id).unwrap()).unwrap(), b"original");
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

        // Defaults: range 1-50, imported_at desc.
        let all: Value = serde_json::from_str(&list.execute(&json!({})).unwrap()).unwrap();
        assert_eq!(all["range"], "1-50");
        assert_eq!(all["returned"], 3);
        assert_eq!(all["total"], 3);
        assert_eq!(all["has_more"], false);

        // Case-insensitive substring filter on display name.
        let filtered: Value =
            serde_json::from_str(&list.execute(&json!({"query": "ALP"})).unwrap()).unwrap();
        assert_eq!(filtered["returned"], 1);
        assert_eq!(filtered["items"][0]["display_name"], "alpha.md");

        // Inclusive ranges paginate and return the next selector.
        let first_page: Value =
            serde_json::from_str(&list.execute(&json!({"range": "1-2"})).unwrap()).unwrap();
        assert_eq!(first_page["returned"], 2);
        assert_eq!(first_page["total"], 3);
        assert_eq!(first_page["has_more"], true);
        assert_eq!(first_page["next"], "3-4");
        let second_page: Value =
            serde_json::from_str(&list.execute(&json!({"range": "3-4"})).unwrap()).unwrap();
        assert_eq!(second_page["returned"], 1);
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
        assert!(list.execute(&json!({"range": "0-1"})).returns_err());
        assert!(list.execute(&json!({"range": "1-201"})).returns_err());
    }

    #[test]
    fn delete_attachment_waits_for_approval_and_denial_preserves() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("keep.txt", b"keep me");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);

        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = gate(
            PermissionMode::Approve,
            &root,
            event_sender,
            decision_receiver,
        );
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
        assert_eq!(request.kind, ApprovalKind::DestructiveConfirm);
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
        let gate = gate(
            PermissionMode::Approve,
            &root,
            event_sender,
            decision_receiver,
        );
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
            listed["returned"], 0,
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
        assert_eq!(listed["returned"], 1);
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
        assert_eq!(listed["returned"], 0);
    }
}
