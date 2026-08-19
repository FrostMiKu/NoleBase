//! Read-only filesystem, note search, and unified read tools.
//!
//! The `read` tool is a parser registry: a target (file path, directory path,
//! http(s) URL, or attachment URI) is resolved once, then each registered
//! [`ReadParser`] is asked in order whether it handles that target. Registering
//! a new format (for example a PDF parser) requires no change to the dispatch
//! logic — only a new parser registered before the generic text-file parser.
//! Attachment reads are read-only: they never register an edit snapshot.
//!
//! The registry, target resolution, and shared filesystem helpers live here;
//! selectors and pagination live in [`paging`], each parser family has its own
//! submodule ([`text`], [`documents`], [`attachments`], [`directory`], [`web`]),
//! and the note search tools live in [`notes`].

mod attachments;
mod directory;
mod document;
mod documents;
mod notes;
mod paging;
mod text;
mod web;

#[cfg(test)]
pub(crate) mod test_support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::fs as async_fs;
use tokio::io::AsyncReadExt as _;

use super::util::{portable_path, range_schema, required_string};
use crate::agent::{canonical_root, SnapshotStore, Tool, ToolExecutionPolicy};
use crate::attachment::{AttachmentStore, AttachmentUri};
use crate::storage::ATTACHMENTS_DIR;

use self::attachments::AttachmentParser;
use self::directory::DirectoryParser;
use self::document::DocumentCache;
use self::documents::DocumentFileParser;
use self::paging::{split_line_range, LineRange};
use self::text::TextFileParser;
use self::web::WebParser;

pub use self::notes::{ListNotes, SearchFiles};

const DEFAULT_READ_LINES: usize = 200;
const MAX_READ_LINES: usize = 2_000;
const MAX_READ_RESPONSE_BYTES: usize = 1_000_000;
const MAX_READ_LINE_BYTES: usize = 256 * 1024;
const READ_RESPONSE_OVERHEAD: usize = 8 * 1024;
const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXTRACTED_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_DIRECTORY_RESULTS: usize = 2_000;
const MAX_DIRECTORY_DEPTH: usize = 16;

/// A resolved target for the unified `read` tool.
#[derive(Clone, Debug)]
pub(crate) enum Target {
    File {
        path: PathBuf,
        range: Option<LineRange>,
    },
    Directory {
        path: PathBuf,
    },
    Web {
        url: String,
        range: Option<LineRange>,
    },
    Attachment {
        uri: AttachmentUri,
        range: Option<LineRange>,
    },
}

impl Target {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Target::File { .. } => "file",
            Target::Directory { .. } => "directory",
            Target::Web { .. } => "web",
            Target::Attachment { .. } => "attachment",
        }
    }

    /// Root-relative form for local paths, the URL for web targets, and the
    /// canonical URI for attachments.
    pub(crate) fn display(&self, root: &Path) -> String {
        match self {
            Target::File { path, .. } | Target::Directory { path } => listed_path(root, path),
            Target::Web { url, .. } => url.clone(),
            Target::Attachment { uri, .. } => uri.to_string(),
        }
    }
}

/// Shared dependencies handed to every [`ReadParser`].
pub(crate) struct ParseContext {
    pub(crate) root: PathBuf,
    pub(crate) reads: Arc<SnapshotStore>,
    pub(crate) client: reqwest::Client,
    pub(crate) attachments: AttachmentStore,
    documents: DocumentCache,
}

pub(crate) enum ReadPayload {
    Text(String),
    Structured(Value),
}

/// A reader for one kind of target. Implementations are tried in registration
/// order; specific parsers must be registered before the generic text-file one.
#[async_trait::async_trait]
pub(crate) trait ReadParser: Send + Sync {
    fn name(&self) -> &'static str;

    fn matches(&self, target: &Target) -> bool;

    async fn parse(
        &self,
        ctx: &ParseContext,
        target: &Target,
        input: &Value,
    ) -> Result<ReadPayload>;
}

pub struct Read {
    ctx: ParseContext,
    parsers: Vec<Box<dyn ReadParser>>,
}

impl Read {
    pub fn new(root: &Path, reads: Arc<SnapshotStore>, client: reqwest::Client) -> Result<Self> {
        let root = canonical_root(root)?;
        let attachments = AttachmentStore::new(root.join(ATTACHMENTS_DIR));
        let ctx = ParseContext {
            root,
            reads,
            client,
            attachments,
            documents: DocumentCache::default(),
        };
        // Order matters: the generic text-file parser must be tried last so a
        // more specific file parser (for example PDF) can claim a target first.
        let parsers: Vec<Box<dyn ReadParser>> = vec![
            Box::new(WebParser),
            Box::new(DirectoryParser),
            Box::new(DocumentFileParser),
            Box::new(AttachmentParser),
            Box::new(TextFileParser),
        ];
        Ok(Self { ctx, parsers })
    }

    /// Registers a parser ahead of built-in file parsers so callers can
    /// override format handling before the generic fallbacks.
    #[allow(dead_code)]
    pub fn register(&mut self, parser: impl ReadParser + 'static) {
        let file_index = self
            .parsers
            .iter()
            .position(|parser| parser.name() == "document_file")
            .unwrap_or(self.parsers.len());
        self.parsers.insert(file_index, Box::new(parser));
    }
}

#[async_trait::async_trait]
impl Tool for Read {
    fn name(&self) -> &'static str {
        "read"
    }
    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    fn description(&self) -> &'static str {
        "Read local files, directories, URLs, office documents, PDFs, and attachment URIs. Text and extracted documents accept an inclusive `:start-end` line selector; editable text returns tagged source lines, while directories use range, depth, and sort options."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Local file or document path, http(s) URL, or attachment URI, optionally suffixed with inclusive lines `:start-end`; or a directory path"
                },
                "range": range_schema(MAX_DIRECTORY_RESULTS),
                "depth": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_DIRECTORY_DEPTH, "default": 1,
                    "description": "Directory recursion depth (directory targets only)"
                },
                "sort_by": {
                    "type": "string",
                    "enum": ["name", "type", "depth", "line_count", "created_at", "modified_at", "size"],
                    "default": "name",
                    "description": "Directory entry sort key (directory targets only)"
                },
                "order": {
                    "type": "string", "enum": ["asc", "desc"], "default": "asc",
                    "description": "Directory entry sort order (directory targets only)"
                }
            },
            "required": ["path"], "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let target = resolve_target(&self.ctx, input).await?;
        for parser in &self.parsers {
            if parser.matches(&target) {
                return match parser.parse(&self.ctx, &target, input).await? {
                    ReadPayload::Text(text) => Ok(text),
                    ReadPayload::Structured(Value::Object(mut payload)) => {
                        payload.insert("kind".into(), json!(target.kind()));
                        payload.insert("target".into(), json!(target.display(&self.ctx.root)));
                        serde_json::to_string_pretty(&Value::Object(payload))
                            .context("encoding read result")
                    }
                    ReadPayload::Structured(_) => {
                        bail!("parser {} returned non-object payload", parser.name())
                    }
                };
            }
        }
        bail!(
            "no parser registered for {} target {}",
            target.kind(),
            target.display(&self.ctx.root)
        )
    }
}

/// Resolves the `path` argument into a concrete target, rejecting the private
/// AI configuration and attachment internals before any parser sees them.
async fn resolve_target(ctx: &ParseContext, input: &Value) -> Result<Target> {
    let requested_input = required_string(input, "path")?;
    let (requested, range) = split_line_range(requested_input)?;
    if requested.starts_with("https://") || requested.starts_with("http://") {
        return Ok(Target::Web {
            url: requested.to_string(),
            range,
        });
    }
    if AttachmentUri::is_attachment_uri(requested) {
        let uri = AttachmentUri::parse(requested)?;
        return Ok(Target::Attachment { uri, range });
    }
    let path = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        ctx.root.join(requested)
    };
    let path = async_fs::canonicalize(&path)
        .await
        .with_context(|| format!("resolving {}", path.display()))?;
    if path.starts_with(ctx.root.join(ATTACHMENTS_DIR)) {
        bail!("generic tools cannot read attachment internals");
    }
    let private_config = ctx.root.join("config/ai.toml");
    if path == private_config {
        bail!("AI configuration is private");
    }
    let metadata = async_fs::metadata(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    if metadata.is_file() {
        Ok(Target::File { path, range })
    } else if metadata.is_dir() {
        if range.is_some() {
            bail!("line selectors can only be used with files and text attachments");
        }
        Ok(Target::Directory { path })
    } else {
        bail!(
            "path is not a regular file or directory: {}",
            path.display()
        )
    }
}

/// The root-relative display form used for local file and directory targets.
fn listed_path(root: &Path, path: &Path) -> String {
    portable_path(path.strip_prefix(root).unwrap_or(path))
}

/// Counts newline-terminated lines in a file without loading it into memory,
/// treating a final unterminated line as one more line.
async fn count_file_lines(path: &Path) -> Result<u64> {
    let mut file = async_fs::File::open(path).await?;
    let mut buffer = [0u8; 64 * 1024];
    let mut newlines = 0u64;
    let mut saw_bytes = false;
    let mut ends_with_newline = false;
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        saw_bytes = true;
        ends_with_newline = buffer[read - 1] == b'\n';
        newlines = newlines
            .saturating_add(buffer[..read].iter().filter(|byte| **byte == b'\n').count() as u64);
    }
    Ok(newlines + u64::from(saw_bytes && !ends_with_newline))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;

    use serde_json::json;

    use super::test_support::{attachment_ctx, FakeParser};
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn read_dispatches_to_registered_parsers_before_text_file() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("sample.pdf"), b"%PDF-1.4").unwrap();
        let mut read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        read.register(FakeParser);

        let output = read
            .execute(&json!({ "path": "sample.pdf" }))
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["kind"], "file");
        assert_eq!(parsed["parsed_by"], "fake");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attachment_uris_dispatch_before_http_and_local_paths() {
        let directory = tempfile::tempdir().unwrap();
        let ctx = attachment_ctx(directory.path());
        let uri = "nole://attachment/00000000-0000-4000-8000-000000000000".to_string();

        let target = resolve_target(&ctx, &json!({ "path": uri })).await.unwrap();
        assert!(
            matches!(&target, Target::Attachment { uri: resolved, .. } if resolved.to_string() == uri)
        );
        assert_eq!(target.kind(), "attachment");
        assert_eq!(target.display(&ctx.root), uri);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_attachment_uris_are_rejected_at_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let ctx = attachment_ctx(directory.path());
        for malformed in [
            "nole://attachment/not-a-uuid".to_string(),
            "nole://attachment/AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA".to_string(),
            "nole://attachment/00000000000000000000000000000000".to_string(),
            "nole://attachment/".to_string(),
            "nole://attachment/00000000-0000-4000-8000-000000000000-extra".to_string(),
        ] {
            assert!(
                resolve_target(&ctx, &json!({ "path": malformed }))
                    .await
                    .is_err(),
                "expected rejection for {malformed}"
            );
        }
        // A non-attachment URL still dispatches to the web target.
        let target = resolve_target(&ctx, &json!({ "path": "https://example.test" }))
            .await
            .unwrap();
        assert!(matches!(target, Target::Web { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_reads_of_attachment_internals_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let metadata = store.import_bytes(b"payload", Some("secret.txt")).unwrap();
        let object = directory
            .path()
            .join(ATTACHMENTS_DIR)
            .join(metadata.id.to_string())
            .join("content.txt");
        assert!(object.exists());
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        let error = read
            .execute(&json!({ "path": object.to_str().unwrap() }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("attachment internals"));
    }
}
