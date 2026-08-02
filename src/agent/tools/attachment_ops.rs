//! Attachment tools: import, list, info, copy-to-workspace, and delete.
//!
//! Attachments are immutable, content-addressed files stored by
//! [`crate::attachment::AttachmentStore`] under `Storage.attachments_dir`.
//! Object paths are private to the store: every tool here exchanges ids,
//! canonical URIs, metadata, and raw bytes only, and the tool results never
//! print physical object paths.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::util::{display_path, required_string};
use crate::agent::{canonical_root, ApprovalGate, ApprovalKind, ApprovalRequest, Tool};
use crate::attachment::{AttachmentId, AttachmentMetadata, AttachmentStore, AttachmentUri};
use crate::storage::Storage;

/// Build a store rooted at the given Nole root's attachments directory.
fn store_for(root: &Path) -> Result<AttachmentStore> {
    Ok(AttachmentStore::new(Storage::new(root)?.attachments_dir))
}

/// Resolve a user-supplied attachment reference: the canonical URI or a bare
/// 64-hex content address. Every accepted form is validated strictly, so
/// malformed ids are refused before any store access.
fn resolve_attachment_uri(reference: &str) -> Result<AttachmentUri> {
    if let Ok(uri) = AttachmentUri::parse(reference) {
        return Ok(uri);
    }
    let id = AttachmentId::from_hex(reference).with_context(|| {
        format!(
            "attachment reference must be nole-attachment://sha256/<64 lowercase hex> or a bare 64-hex id, got {reference:?}"
        )
    })?;
    Ok(AttachmentUri::from_id(id))
}

/// Validate an optional display name: it must be a bare file name with no
/// path separators, NUL, or `.`/`..` and no empty form.
fn validate_display_name(name: &str) -> Result<String> {
    if name.is_empty() {
        bail!("display name must not be empty");
    }
    if name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
        bail!("display name must be a bare file name without path separators");
    }
    Ok(name.to_string())
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
/// otherwise. Both reference only the canonical URI.
fn markdown_embed(metadata: &AttachmentMetadata) -> String {
    let uri = metadata.uri().to_string();
    let is_image = metadata
        .mime_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with("image/"));
    if is_image {
        format!("![{}]({uri})", metadata.source)
    } else {
        format!("[{}]({uri})", metadata.source)
    }
}

/// Metadata shaped for Agent output. Never contains physical object paths.
fn attachment_metadata_json(metadata: &AttachmentMetadata) -> Value {
    json!({
        "id": metadata.id.to_hex(),
        "uri": metadata.uri().to_string(),
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
        "Import an existing regular file (absolute or Nole-relative path) into the immutable attachment store. The source is copied and never modified; identical content re-imports to the same canonical URI. Returns the canonical URI plus a Markdown embed (images) or link (other files) for notes."
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
                    "description": "Optional media type, accepted only when it matches the type derivable from the display name"
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
            None => default_name.to_string(),
        };
        let metadata = self.store.import_path_as(&source, &display_name)?;
        if let Some(requested) = input.get("media_type").and_then(Value::as_str) {
            let stored = metadata.mime_type.as_deref().unwrap_or("none");
            if stored != requested {
                bail!(
                    "media type {requested:?} is not safely derivable from {display_name:?}; stored media type is {stored}"
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
        "List stored attachments with metadata (canonical URI, display name, size, media type, import time). Attachment object paths are never exposed."
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    async fn execute(&self, _input: &Value) -> Result<String> {
        let items = self
            .store
            .list()?
            .iter()
            .map(attachment_metadata_json)
            .collect::<Vec<_>>();
        serde_json::to_string_pretty(&json!({ "count": items.len(), "items": items }))
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
                    "description": "Canonical attachment URI (nole-attachment://sha256/<64 hex>) or bare 64-hex id"
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
        "Copy an attachment's bytes into a NEW file under workspace/main (the Agent workspace sandbox), where the generic file tools may edit it. The destination is relative to workspace/main and must not already exist."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "Canonical attachment URI (nole-attachment://sha256/<64 hex>) or bare 64-hex id"
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
        let bytes = self.store.read_object(uri.id())?;
        let destination = workspace_destination(&self.root, destination_text)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .with_context(|| format!("creating destination {}", destination.display()))?;
        if let Err(error) = output.write_all(&bytes) {
            drop(output);
            let _ = fs::remove_file(&destination);
            return Err(error)
                .with_context(|| format!("writing destination {}", destination.display()));
        }
        serde_json::to_string_pretty(&json!({
            "path": display_path(&self.root, &destination),
            "bytes": bytes.len(),
            "uri": uri.to_string(),
        }))
        .context("encoding copy result")
    }
}

pub struct DeleteAttachment {
    store: AttachmentStore,
    gate: ApprovalGate,
}

impl DeleteAttachment {
    pub fn new(root: &Path, gate: ApprovalGate) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            store: store_for(&root)?,
            gate,
        })
    }
}

#[async_trait::async_trait]
impl Tool for DeleteAttachment {
    fn name(&self) -> &'static str {
        "delete_attachment"
    }

    fn description(&self) -> &'static str {
        "Delete an attachment, moving it to the attachment trash. The attachment must not change between the request and the deletion."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "uri": {
                    "type": "string",
                    "description": "Canonical attachment URI (nole-attachment://sha256/<64 hex>) or bare 64-hex id"
                }
            },
            "required": ["uri"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let reference = required_string(input, "uri")?;
        let uri = resolve_attachment_uri(reference)?;
        let before = self.store.metadata(uri.id())?;
        self.gate
            .request(ApprovalRequest {
                title: "Delete attachment".to_string(),
                message: format!(
                    "Delete attachment {uri} ({}), {} bytes?",
                    before.source, before.size
                ),
                kind: ApprovalKind::Confirm,
            })
            .await?;
        let after = self.store.metadata(uri.id())?;
        if after != before {
            bail!("attachment changed before deletion; inspect it again and retry");
        }
        if !self.store.verify(uri.id())? {
            bail!("attachment content changed before deletion; inspect it again and retry");
        }
        if !self.store.remove(uri.id())? {
            bail!("attachment no longer exists");
        }
        Ok(format!("deleted attachment {uri} (moved to trash)"))
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

    #[test]
    fn import_preserves_source_and_dedups() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("photo.png", b"fake-png-bytes");
        let import = ImportAttachment::new(&root).unwrap();

        let first: Value =
            serde_json::from_str(&import.execute(&json!({"source": source})).unwrap()).unwrap();
        assert!(source.exists(), "import must preserve the source file");
        let uri = first["uri"].as_str().unwrap().to_string();
        assert!(uri.starts_with("nole-attachment://sha256/"));
        assert_eq!(uri.len(), "nole-attachment://sha256/".len() + 64);
        assert_eq!(first["source"], "photo.png");
        assert_eq!(first["mime_type"], "image/png");
        assert_eq!(first["size"], 14);
        assert!(first["markdown"].as_str().unwrap().contains(&uri));
        assert!(first["markdown"]
            .as_str()
            .unwrap()
            .starts_with("![photo.png]("));

        let second: Value =
            serde_json::from_str(&import.execute(&json!({"source": source})).unwrap()).unwrap();
        assert_eq!(
            second["uri"], first["uri"],
            "duplicate import must dedup to the same URI"
        );
        assert!(source.exists());

        let list = ListAttachments::new(&root).unwrap();
        let listed: Value = serde_json::from_str(&list.execute(&json!({})).unwrap()).unwrap();
        assert_eq!(listed["count"], 1, "duplicate import must not store twice");
        assert_eq!(listed["items"][0]["uri"], first["uri"]);
    }

    #[test]
    fn import_accepts_relative_sources_and_validated_metadata() {
        let (_directory, root) = fresh_root();
        fs::write(root.join("data/note.md"), "# imported\n").unwrap();
        let import = ImportAttachment::new(&root).unwrap();

        let result: Value =
            serde_json::from_str(&import.execute(&json!({"source": "data/note.md"})).unwrap())
                .unwrap();
        assert_eq!(result["source"], "note.md");
        assert_eq!(result["mime_type"], "text/markdown");
        assert!(result["markdown"]
            .as_str()
            .unwrap()
            .starts_with("[note.md]("));

        // Re-importing identical bytes with a new name still returns the first
        // import's metadata (first import wins).
        let deduped: Value = serde_json::from_str(
            &import
                .execute(&json!({
                    "source": "data/note.md",
                    "display_name": "renamed.png"
                }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(deduped["source"], "note.md");
        assert_eq!(deduped["mime_type"], "text/markdown");

        // Explicit safe display name and matching media type are accepted for
        // fresh content.
        fs::write(root.join("data/art.txt"), "unique bytes\n").unwrap();
        let custom: Value = serde_json::from_str(
            &import
                .execute(&json!({
                    "source": "data/art.txt",
                    "display_name": "renamed.png",
                    "media_type": "image/png"
                }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(custom["source"], "renamed.png");
        assert_eq!(custom["mime_type"], "image/png");
        assert!(custom["markdown"]
            .as_str()
            .unwrap()
            .starts_with("![renamed.png]("));

        // Unsafe display names and non-derivable media types are refused.
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
        assert!(info_text.contains("\"source\": \"report.pdf\""));
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

        // Bare 64-hex ids are accepted as references.
        let bare = uri.trim_start_matches("nole-attachment://sha256/");
        info.execute(&json!({"uri": bare})).unwrap();
        assert!(info
            .execute(&json!({"uri": "nole-attachment://sha256/not-hex"}))
            .returns_err());
        assert!(info
            .execute(&json!({"uri": "nole-attachment://sha256/abcdef"}))
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
        let unknown = "nole-attachment://sha256/".to_string() + &"0".repeat(64);
        assert!(copy
            .execute(&json!({"uri": unknown, "destination": "x.pdf"}))
            .returns_err());
        assert!(!root.join("workspace/main/x.pdf").exists());
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
        let delete = DeleteAttachment::new(&root, gate).unwrap();

        // Malformed references are refused without any approval request.
        assert!(delete
            .execute(&json!({"uri": "nole-attachment://sha256/XYZ"}))
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
        let delete = DeleteAttachment::new(&root, gate).unwrap();
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
        let delete_again = DeleteAttachment::new(&root, bypass_gate()).unwrap();
        assert!(delete_again.execute(&json!({"uri": uri})).returns_err());
    }

    #[test]
    fn bypassed_approval_gate_allows_immediate_delete() {
        let (_directory, root) = fresh_root();
        let (_outside, source) = outside_file("temp.txt", b"temp");
        let import = ImportAttachment::new(&root).unwrap();
        let uri = import_source(&import, &source);
        let delete = DeleteAttachment::new(&root, bypass_gate()).unwrap();
        let message = delete.execute(&json!({"uri": uri})).unwrap();
        assert!(message.contains("moved to trash"));
        let list = ListAttachments::new(&root).unwrap();
        let listed: Value = serde_json::from_str(&list.execute(&json!({})).unwrap()).unwrap();
        assert_eq!(listed["count"], 0);
    }
}
