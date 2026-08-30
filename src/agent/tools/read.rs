//! Read-only filesystem, note search, and unified read tools.
//!
//! The `read` tool is a parser registry: a target (file path, directory path,
//! http(s) URL, attachment URI, or skill URI) is resolved once, then each
//! registered [`ReadParser`] is asked in order whether it handles that target.
//! Registering a new format (for example a PDF parser) requires no change to
//! the dispatch logic — only a new parser registered before the generic
//! text-file parser. Attachment reads are read-only: they never register an
//! edit snapshot.
//!
//! The registry, target resolution, and shared filesystem helpers live here;
//! ranges and pagination live in [`paging`], each parser family has its own
//! submodule ([`text`], [`documents`], [`attachments`], [`directory`],
//! [`web`], [`skill`]), and the note search tools live in [`notes`].

mod attachments;
mod directory;
mod document;
mod documents;
mod notes;
mod paging;
mod result;
mod skill;
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

use super::util::{portable_path, required_string};
use crate::agent::{canonical_root, SnapshotStore, Tool, ToolExecutionPolicy, ToolOutput};
use crate::attachment::{AttachmentStore, AttachmentUri};
use crate::provider::ImageBlock;
use crate::storage::ATTACHMENTS_DIR;

use self::attachments::AttachmentParser;
use self::directory::DirectoryParser;
use self::document::DocumentCache;
use self::documents::DocumentFileParser;
use self::paging::{line_range, LineRange};
use self::result::ResultParser;
use self::text::TextFileParser;
use self::web::WebParser;

pub use self::notes::{Notes, SearchNotes};
pub use self::skill::SkillParser;
pub(crate) use self::web::fetch_web_response;

const DEFAULT_READ_LINES: usize = 200;
const MAX_READ_LINES: usize = 2_000;
const MAX_READ_RESPONSE_CHARACTERS: usize = 8_192;
const MAX_READ_LINE_BYTES: usize = 256 * 1024;
const READ_RESPONSE_OVERHEAD_CHARACTERS: usize = 2 * 1024;
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
    Skill {
        name: String,
    },
    Result {
        uri: String,
        path: PathBuf,
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
            Target::Skill { .. } => "skill",
            Target::Result { .. } => "result",
        }
    }

    /// Root-relative form for local paths, the URL for web targets, the
    /// canonical URI for attachments, and `skill://<name>` for skills.
    pub(crate) fn display(&self, root: &Path) -> String {
        match self {
            Target::File { path, .. } | Target::Directory { path } => listed_path(root, path),
            Target::Web { url, .. } => url.clone(),
            Target::Attachment { uri, .. } => uri.to_string(),
            Target::Skill { name } => format!("{}{name}", skill::SKILL_URI_SCHEME),
            Target::Result { uri, .. } => uri.clone(),
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
    Image(ImageBlock),
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
            Box::new(ResultParser),
            Box::new(TextFileParser),
        ];
        Ok(Self { ctx, parsers })
    }

    /// Registers a parser ahead of built-in file parsers so callers can
    /// override format handling before the generic fallbacks.
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
        "Read local files, directories, URLs, office documents, PDFs, attachment URIs, session result URIs, and skills. A nole://result/<id> URI pages an oversized text result from the current Agent session. A skill URI (`skill://<name>`) returns the full body of one skill from the catalog listed in the system prompt. For http(s) URLs this returns reader-mode content: HTML is converted to Markdown, PDFs and office documents are extracted to text, and JSON/plain text is returned unchanged. Image files and image URLs are returned as images the model can see natively. The inclusive `range` selects lines from text and extracted documents or entries from directories; editable text returns tagged source lines. To fetch the raw unprocessed response body instead, use `http`."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Local file or document path, http(s) URL, attachment URI, session result URI (nole://result/<id>), skill URI (skill://<name>), or directory path"
                },
                "range": {
                    "type": "string",
                    "pattern": "^[1-9][0-9]*-[1-9][0-9]*$",
                    "description": "Inclusive one-based line or directory-entry range. Defaults to 1-200 for line-based targets and 1-50 for directories; may select at most 2000 positions."
                },
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
        self.read_output(input).await?.into_inline_text()
    }

    async fn execute_output(&self, input: &Value) -> Result<ToolOutput> {
        self.read_output(input).await
    }
}

impl Read {
    /// Dispatch a target to its parser and return the full tool output: text
    /// for text/document targets, or a native image block plus its summary.
    pub(crate) async fn read_output(&self, input: &Value) -> Result<ToolOutput> {
        let target = resolve_target(&self.ctx, input).await?;
        for parser in &self.parsers {
            if parser.matches(&target) {
                let payload = parser.parse(&self.ctx, &target, input).await?;
                return match payload {
                    ReadPayload::Text(text) => Ok(ToolOutput::text(text)),
                    ReadPayload::Image(block) => {
                        let target_display = target.display(&self.ctx.root);
                        let byte_count = block.bytes.as_ref().map_or(0, |bytes| bytes.len());
                        let summary = format!(
                            "Read image {target_display} ({}x{}, {}, {byte_count} bytes).",
                            block.width,
                            block.height,
                            block.media_type.mime(),
                        );
                        Ok(ToolOutput {
                            content: crate::agent::ToolOutputContent::Text(summary),
                            images: vec![block],
                        })
                    }
                    ReadPayload::Structured(Value::Object(mut payload)) => {
                        payload.insert("kind".into(), json!(target.kind()));
                        payload.insert("target".into(), json!(target.display(&self.ctx.root)));
                        let text = serde_json::to_string_pretty(&Value::Object(payload))
                            .context("encoding read result")?;
                        Ok(ToolOutput::text(text))
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
    let requested = required_string(input, "path")?;
    if let Some(id) = crate::agent::parse_result_id(requested)? {
        let root = ctx.root.clone();
        let path =
            tokio::task::spawn_blocking(move || crate::agent::resolve_result_path(&root, id))
                .await
                .context("joining session result resolution")??;
        return Ok(Target::Result {
            uri: requested.to_string(),
            path,
            range: line_range(input)?,
        });
    }
    if requested.starts_with("https://") || requested.starts_with("http://") {
        return Ok(Target::Web {
            url: requested.to_string(),
            range: line_range(input)?,
        });
    }
    if let Some(name) = requested.strip_prefix(skill::SKILL_URI_SCHEME) {
        if name.is_empty() {
            bail!("skill URI must name a skill");
        }
        return Ok(Target::Skill {
            name: name.to_string(),
        });
    }
    if AttachmentUri::is_attachment_uri(requested) {
        let uri = AttachmentUri::parse(requested)?;
        return Ok(Target::Attachment {
            uri,
            range: line_range(input)?,
        });
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
        Ok(Target::File {
            path,
            range: line_range(input)?,
        })
    } else if metadata.is_dir() {
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
    async fn session_result_uris_survive_store_recreation_and_page() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::agent::SessionResultStore::new(directory.path()).unwrap();
        let uri = store.store("one\ntwo\nthree\nfour\n").unwrap();
        drop(store);

        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read
            .execute(&json!({"path": uri, "range": "2-3"}))
            .await
            .unwrap();
        assert!(output.contains("2:two\n3:three"));
        assert!(output.contains("[Showing lines 2-3 of 4]"));

        let restored = crate::agent::SessionResultStore::new(directory.path()).unwrap();
        assert_eq!(restored.store("next").unwrap(), "nole://result/2");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_result_pages_fit_the_inline_character_limit() {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::agent::SessionResultStore::new(directory.path()).unwrap();
        let content = (0..200)
            .map(|index| format!("{index}:{}", "界".repeat(100)))
            .collect::<Vec<_>>()
            .join("\n");
        let uri = store.store(&content).unwrap();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let output = read.execute(&json!({"path": uri})).await.unwrap();
        assert!(output.chars().count() <= MAX_READ_RESPONSE_CHARACTERS);
        assert!(output.contains("[Showing lines 1-"));
        assert!(!output.contains("[Showing lines 1-200"));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn session_result_uri_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let results = directory.path().join("agent-session/results");
        fs::create_dir_all(&results).unwrap();
        let private = directory.path().join("config/ai.toml");
        fs::create_dir_all(private.parent().unwrap()).unwrap();
        fs::write(&private, "secret").unwrap();
        symlink(&private, results.join("1")).unwrap();

        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();
        let error = read
            .execute(&json!({"path": "nole://result/1"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not a regular stored result"));
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

    #[tokio::test(flavor = "current_thread")]
    async fn local_and_attachment_images_return_native_tool_output() {
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let image = image::DynamicImage::new_rgb8(8, 4);
        let mut buf = std::io::Cursor::new(Vec::new());
        image.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let png = buf.into_inner();
        let path = directory.path().join("diagram.png");
        fs::write(&path, &png).unwrap();
        let uri = store
            .import_bytes(&png, Some("diagram.png"))
            .unwrap()
            .uri()
            .to_string();
        let read = Read::new(
            directory.path(),
            Arc::new(SnapshotStore::default()),
            reqwest::Client::new(),
        )
        .unwrap();

        // Absolute local path.
        let output = read
            .execute_output(&json!({ "path": path.to_str().unwrap() }))
            .await
            .unwrap();
        assert!(output
            .clone()
            .into_inline_text()
            .unwrap()
            .starts_with("Read image diagram.png (8x4, image/png, "));
        assert_eq!(output.images.len(), 1);
        assert!(output.images[0].bytes.is_some());

        // Relative local path resolves against the nole root.
        let output = read
            .execute_output(&json!({ "path": "diagram.png" }))
            .await
            .unwrap();
        assert_eq!(output.images.len(), 1);
        assert!(matches!(
            &output.images[0].source,
            crate::provider::ImageSource::LocalFile { .. }
        ));

        // Attachment URI.
        let output = read.execute_output(&json!({ "path": uri })).await.unwrap();
        assert_eq!(output.images.len(), 1);
        assert!(matches!(
            &output.images[0].source,
            crate::provider::ImageSource::Attachment { .. }
        ));

        // Line ranges are rejected for image targets.
        let error = read
            .execute_output(&json!({ "path": uri, "range": "1-2" }))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "range is not supported for image targets"
        );

        // Corrupted image bytes fail explicitly, not as a UTF-8 error.
        let bad = directory.path().join("broken.png");
        fs::write(&bad, b"\x89PNG\r\n\x1a\nnot-a-real-png").unwrap();
        let error = read
            .execute_output(&json!({ "path": bad.to_str().unwrap() }))
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("not valid UTF-8"));
    }
}
