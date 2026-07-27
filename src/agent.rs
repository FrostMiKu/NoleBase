//! Small Anthropic Messages API agent with a registry of local tools.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use similar::TextDiff;

use crate::storage::Storage;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_AGENT_ROUNDS: usize = 12;
const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_FETCH_BYTES: u64 = 1_000_000;
const MAX_DIRECTORY_DEPTH: u64 = 10;
const MAX_DIRECTORY_ENTRIES: usize = 2_000;
const MAX_DIFF_BYTES: usize = 200_000;
const DEFAULT_READ_LINES: usize = 200;
const MAX_READ_LINES: usize = 2_000;
const DEFAULT_SEARCH_RESULTS: usize = 50;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_SEARCH_OFFSET: usize = 10_000;
const MAX_SEARCH_SNIPPET_CHARS: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    Approve,
    Bypass,
}

impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::Bypass => "BYPASS",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Approve => Self::Bypass,
            Self::Bypass => Self::Approve,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub title: String,
    pub diff: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    Deny,
}

#[derive(Debug)]
pub enum AgentEvent {
    Progress(String),
    Notification(String),
    Approval(ApprovalRequest),
    AskUser(AskUserRequest),
    Finished(Result<String, String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskUserRequest {
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AskUserResponse {
    Answer(String),
    Cancelled,
}

#[derive(Clone)]
struct ApprovalGate {
    bypass: Arc<AtomicBool>,
    events: Sender<AgentEvent>,
    decisions: Arc<Mutex<Receiver<ApprovalDecision>>>,
}

#[derive(Default)]
struct ReadTracker {
    files: Mutex<HashMap<PathBuf, FileReadState>>,
    messages: Mutex<HashMap<String, String>>,
}

struct FileReadState {
    snapshot: String,
    ranges: Vec<(usize, usize)>,
    total_lines: usize,
}

impl ReadTracker {
    fn mark_file(
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

    fn file_snapshot(&self, path: &Path) -> Result<Option<String>> {
        let files = self
            .files
            .lock()
            .map_err(|_| anyhow::anyhow!("file read tracker lock poisoned"))?;
        Ok(files.get(path).and_then(|state| {
            let complete = state.total_lines == 0
                || state
                    .ranges
                    .first()
                    .is_some_and(|range| range.0 == 0 && range.1 >= state.total_lines);
            complete.then(|| state.snapshot.clone())
        }))
    }

    fn consume_file(&self, path: &Path) -> Result<()> {
        self.files
            .lock()
            .map_err(|_| anyhow::anyhow!("file read tracker lock poisoned"))?
            .remove(path);
        Ok(())
    }

    fn mark_message(&self, id: String, body: String) -> Result<()> {
        self.messages
            .lock()
            .map_err(|_| anyhow::anyhow!("message read tracker lock poisoned"))?
            .insert(id, body);
        Ok(())
    }

    fn message_snapshot(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .messages
            .lock()
            .map_err(|_| anyhow::anyhow!("message read tracker lock poisoned"))?
            .get(id)
            .cloned())
    }

    fn consume_message(&self, id: &str) -> Result<()> {
        self.messages
            .lock()
            .map_err(|_| anyhow::anyhow!("message read tracker lock poisoned"))?
            .remove(id);
        Ok(())
    }
}

impl ApprovalGate {
    fn request(&self, request: ApprovalRequest) -> Result<()> {
        if self.bypass.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.events
            .send(AgentEvent::Approval(request))
            .context("sending approval request")?;
        let decision = self
            .decisions
            .lock()
            .map_err(|_| anyhow::anyhow!("approval channel lock poisoned"))?
            .recv()
            .context("waiting for approval decision")?;
        match decision {
            ApprovalDecision::Approve => Ok(()),
            ApprovalDecision::Deny => bail!("change denied by user"),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentConfig {
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_base_url() -> String {
    "https://api.anthropic.com".to_string()
}

const fn default_max_tokens() -> u32 {
    4096
}

impl AgentConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading AI config {}", path.display()))?;
        let config: Self = toml::from_str(&text)
            .with_context(|| format!("parsing AI config {}", path.display()))?;
        if config.api_key.trim().is_empty() {
            bail!("set api_key in {}", path.display());
        }
        if config.model.trim().is_empty() {
            bail!("model is empty in {}", path.display());
        }
        if config.max_tokens == 0 {
            bail!("max_tokens must be greater than zero");
        }
        Ok(config)
    }
}

/// The minimal interface needed to expose a new tool to the model.
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn input_schema(&self) -> Value;
    fn execute(&self, input: &Value) -> Result<String>;

    fn definition(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "input_schema": self.input_schema(),
        })
    }
}

pub struct Agent {
    config: AgentConfig,
    client: Client,
    tools: HashMap<String, Box<dyn Tool>>,
    system: String,
    events: Sender<AgentEvent>,
}

impl Agent {
    pub fn from_config(
        config_path: &Path,
        nole_root: &Path,
        events: Sender<AgentEvent>,
        decisions: Receiver<ApprovalDecision>,
        user_responses: Receiver<AskUserResponse>,
        bypass: Arc<AtomicBool>,
    ) -> Result<Self> {
        let config = AgentConfig::load(config_path)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(90))
            .user_agent(concat!("nole/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        let mut agent = Self {
            config,
            client: client.clone(),
            tools: HashMap::new(),
            system: system_prompt(nole_root),
            events: events.clone(),
        };
        let gate = ApprovalGate {
            bypass,
            events,
            decisions: Arc::new(Mutex::new(decisions)),
        };
        let reads = Arc::new(ReadTracker::default());
        agent.register(ReadFile::new(nole_root, reads.clone())?);
        agent.register(ListDirectory::new(nole_root)?);
        agent.register(SearchContent::new(nole_root)?);
        agent.register(SearchFiles::new(nole_root)?);
        agent.register(WriteFile::new(nole_root)?);
        agent.register(UpdateFile::new(nole_root, gate.clone(), reads.clone())?);
        agent.register(ReadMessage::new(nole_root, reads.clone())?);
        agent.register(UpdateMessage::new(nole_root, gate, reads)?);
        agent.register(AddMessage::new(nole_root)?);
        agent.register(Notify {
            events: agent.events.clone(),
        });
        agent.register(AskUser {
            events: agent.events.clone(),
            responses: Arc::new(Mutex::new(user_responses)),
        });
        agent.register(WebFetch { client });
        Ok(agent)
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
    }

    pub fn run(&self, prompt: &str) -> Result<String> {
        let mut messages = vec![json!({ "role": "user", "content": prompt })];
        let definitions: Vec<Value> = self.tools.values().map(|tool| tool.definition()).collect();

        for _ in 0..MAX_AGENT_ROUNDS {
            let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));
            let response = self
                .client
                .post(url)
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .json(&json!({
                    "model": self.config.model,
                    "max_tokens": self.config.max_tokens,
                    "system": self.system,
                    "messages": messages,
                    "tools": definitions,
                }))
                .send()
                .context("calling Anthropic Messages API")?;
            let status = response.status();
            let body = response.text().context("reading Anthropic response")?;
            if !status.is_success() {
                let message = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|value| value.pointer("/error/message")?.as_str().map(str::to_owned))
                    .unwrap_or(body);
                bail!("Anthropic API returned {status}: {message}");
            }
            let value: Value =
                serde_json::from_str(&body).context("decoding Anthropic response")?;
            let content = value
                .get("content")
                .and_then(Value::as_array)
                .context("Anthropic response has no content array")?;
            let tool_uses: Vec<&Value> = content
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                .collect();
            if tool_uses.is_empty() {
                let output = content
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if output.trim().is_empty() {
                    bail!("Anthropic returned no text");
                }
                return Ok(output);
            }

            messages.push(json!({ "role": "assistant", "content": content }));
            let results: Vec<Value> = tool_uses
                .into_iter()
                .map(|call| self.execute_tool_call(call))
                .collect();
            messages.push(json!({ "role": "user", "content": results }));
        }
        bail!("agent exceeded {MAX_AGENT_ROUNDS} tool-call rounds")
    }

    fn execute_tool_call(&self, call: &Value) -> Value {
        let id = call.get("id").and_then(Value::as_str).unwrap_or("");
        let name = call.get("name").and_then(Value::as_str).unwrap_or("");
        let input = call.get("input").unwrap_or(&Value::Null);
        let _ = self
            .events
            .send(AgentEvent::Progress(format!("Using {name}")));
        let result = self
            .tools
            .get(name)
            .context("unknown tool")
            .and_then(|tool| tool.execute(input));
        match result {
            Ok(content) => {
                let _ = self
                    .events
                    .send(AgentEvent::Progress(format!("Completed {name}")));
                json!({
                "type": "tool_result", "tool_use_id": id, "content": content
                })
            }
            Err(error) => {
                let _ = self
                    .events
                    .send(AgentEvent::Progress(format!("Failed {name}: {error}")));
                json!({
                "type": "tool_result", "tool_use_id": id,
                "content": error.to_string(), "is_error": true
                })
            }
        }
    }
}

fn system_prompt(root: &Path) -> String {
    format!(
        r#"You are the AI assistant inside Nole, a chat-style terminal note app.
Answer the user's card directly. Your final response becomes a new Chat card, so return only useful content, without discussing tool mechanics.

Nole renders MBDown. MBDown supports CommonMark headings, emphasis, strong text, strikethrough, links, lists, task lists, fenced code, block quotes, and tables. It also supports restricted BBCode:
- inline: [b], [i], [u], [s], [dim], named colors such as [red], [color=196], [color=#12abef], [bg=blue], and [link=https://example.com]label[/link]
- layout: [center]...[/center], [right]...[/right], [indent first=4]...[/indent]
- boxes: [box title="Info" width=full border=single border-color=#12abef bg=17 px=1 py=0]...[/box]
- responsive columns: [columns gap=2][column width=1fr]...[/column][column width=2fr]...[/column][/columns]
Use ordinary Markdown unless richer MBDown layout materially improves the answer. Close every BBCode tag and never emit raw terminal escape sequences.

The Nole root is {root}. Special paths are:
- CHAT.md: persisted chat cards; each is wrapped in hidden nole-msg comments
- TODO.md: Markdown task list
- ARCHIVE.md: archived cards
- config/ai.toml: Anthropic API configuration; never read or expose secrets from it
- data/: flat user note storage; notes use .md or .mb
Relative file paths use this root. read_file and list_directory also accept absolute paths, but write_file and update_file are restricted to this root. read_file is line-paginated; use offset and limit to inspect only relevant portions. Generic file tools must never operate on CHAT.md or config/ai.toml. Use search_content for full-text search across Chat and notes, and search_files for fuzzy note-name search. Use read_message and update_message for existing chat cards by nole-msg id; use add_message to append a new card. add_message is not approval-gated, while updates may pause for user approval. write_file only creates new files and update_file only changes existing files. Before update_file you MUST successfully read every line of the exact file in this agent run, possibly across multiple read_file calls; before update_message you MUST successfully read_message the exact id. Updates are automatically rejected without that read, even in bypass mode. Prefer shallow directory listings first, then inspect relevant subdirectories or request bounded recursion. Use ask_user when a missing decision or ambiguity materially affects the result; include concise options when useful, while allowing free-text answers. Use notify to surface a short, time-sensitive message in the user's TUI. Do not assume your final response is added to Chat: call add_message when content belongs in Chat. Your final text is shown in the Agent output panel. Use tools only when the request requires local context or changes."#,
        root = root.display()
    )
}

fn canonical_root(root: &Path) -> Result<PathBuf> {
    fs::canonicalize(root).with_context(|| format!("resolving {}", root.display()))
}

fn safe_relative(root: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("path must stay within the Nole root");
    }
    let path = root.join(relative);
    let parent = path.parent().context("path has no parent")?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("resolving parent directory {}", parent.display()))?;
    if !canonical_parent.starts_with(root) {
        bail!("path escapes the Nole root");
    }
    Ok(path)
}

struct ReadFile {
    root: PathBuf,
    private_config: PathBuf,
    chat_path: PathBuf,
    reads: Arc<ReadTracker>,
}

impl ReadFile {
    fn new(root: &Path, reads: Arc<ReadTracker>) -> Result<Self> {
        let root = canonical_root(root)?;
        let private_config = fs::canonicalize(root.join("config/ai.toml"))
            .unwrap_or_else(|_| root.join("config/ai.toml"));
        Ok(Self {
            private_config,
            chat_path: root.join("CHAT.md"),
            reads,
            root,
        })
    }
}

impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a line range from any UTF-8 text file by absolute path, or by a path relative to the Nole root (maximum 1 MB). offset is zero-based. Reads default to 200 lines and return pagination metadata."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": {
                "path": { "type": "string" },
                "offset": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_READ_LINES, "default": DEFAULT_READ_LINES
                }
            },
            "required": ["path"], "additionalProperties": false
        })
    }
    fn execute(&self, input: &Value) -> Result<String> {
        let path = required_string(input, "path")?;
        let path = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.root.join(path)
        };
        let path =
            fs::canonicalize(&path).with_context(|| format!("resolving {}", path.display()))?;
        if path == self.private_config || path == self.chat_path {
            bail!("use message tools for CHAT.md; AI configuration is private");
        }
        let metadata =
            fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            bail!("file must be a regular UTF-8 file no larger than 1 MB");
        }
        let content =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let offset = optional_usize(input, "offset", 0, usize::MAX)?;
        let limit = optional_usize(input, "limit", DEFAULT_READ_LINES, MAX_READ_LINES)?;
        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let total_lines = lines.len();
        let start = offset.min(total_lines);
        let end = start.saturating_add(limit).min(total_lines);
        let selected = lines[start..end].concat();
        self.reads
            .mark_file(path.clone(), content, start, end, total_lines)?;
        serde_json::to_string_pretty(&json!({
            "path": path.to_string_lossy(),
            "offset": start,
            "returned_lines": end - start,
            "total_lines": total_lines,
            "has_more": end < total_lines,
            "content": selected,
        }))
        .context("encoding file read")
    }
}

struct ListDirectory {
    root: PathBuf,
}

impl ListDirectory {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
        })
    }
}

impl Tool for ListDirectory {
    fn name(&self) -> &'static str {
        "list_directory"
    }

    fn description(&self) -> &'static str {
        "List a directory by absolute path or a path relative to the Nole root. Supports bounded recursion; symlinks are listed but never followed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "recursive": { "type": "boolean", "default": false },
                "max_depth": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_DIRECTORY_DEPTH, "default": 3
                }
            },
            "required": ["path"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let requested = required_string(input, "path")?;
        let path = if Path::new(requested).is_absolute() {
            PathBuf::from(requested)
        } else {
            self.root.join(requested)
        };
        let path = fs::canonicalize(&path)
            .with_context(|| format!("resolving directory {}", path.display()))?;
        if !path.is_dir() {
            bail!("path is not a directory: {}", path.display());
        }

        let recursive = input
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_depth = if recursive {
            input.get("max_depth").and_then(Value::as_u64).unwrap_or(3)
        } else {
            1
        };
        if !(1..=MAX_DIRECTORY_DEPTH).contains(&max_depth) {
            bail!("max_depth must be between 1 and {MAX_DIRECTORY_DEPTH}");
        }

        let mut entries = Vec::new();
        let mut pending = vec![(path.clone(), PathBuf::new(), 1u64)];
        let mut truncated = false;
        while let Some((directory, relative_parent, depth)) = pending.pop() {
            let mut children = fs::read_dir(&directory)
                .with_context(|| format!("listing {}", directory.display()))?
                .collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(|entry| entry.file_name());

            let mut nested = Vec::new();
            for entry in children {
                if entries.len() == MAX_DIRECTORY_ENTRIES {
                    truncated = true;
                    break;
                }
                let file_type = entry.file_type()?;
                let relative = relative_parent.join(entry.file_name());
                let kind = if file_type.is_symlink() {
                    "symlink"
                } else if file_type.is_dir() {
                    "directory"
                } else if file_type.is_file() {
                    "file"
                } else {
                    "other"
                };
                let mut item = json!({
                    "path": relative.to_string_lossy(),
                    "type": kind,
                });
                if file_type.is_file() {
                    item["size"] = json!(entry.metadata()?.len());
                }
                entries.push(item);
                if recursive && file_type.is_dir() && depth < max_depth {
                    nested.push((entry.path(), relative, depth + 1));
                }
            }
            if truncated {
                break;
            }
            // The stack is LIFO, so reverse to retain sorted traversal order.
            nested.reverse();
            pending.extend(nested);
        }

        serde_json::to_string_pretty(&json!({
            "root": path.to_string_lossy(),
            "entries": entries,
            "truncated": truncated,
        }))
        .context("encoding directory listing")
    }
}

struct SearchContent {
    storage: Storage,
    root: PathBuf,
}

impl SearchContent {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            root: canonical_root(root)?,
        })
    }
}

impl Tool for SearchContent {
    fn name(&self) -> &'static str {
        "search_content"
    }

    fn description(&self) -> &'static str {
        "Case-insensitive full-text search across Chat cards and managed note files. Returns message ids or note paths with matching snippets and supports result pagination."
    }

    fn input_schema(&self) -> Value {
        search_schema("Text to find in Chat cards and note contents")
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let query = required_string(input, "query")?.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        let offset = optional_usize(input, "offset", 0, MAX_SEARCH_OFFSET)?;
        let limit = optional_usize(input, "limit", DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS)?;
        let query_lower = query.to_lowercase();
        let mut matches = Vec::new();
        for message in self.storage.load_messages()? {
            if message.body.to_lowercase().contains(&query_lower) {
                matches.push(json!({
                    "type": "message",
                    "id": message.id,
                    "snippet": matching_line(&message.body, &query_lower),
                }));
            }
        }
        for hit in self.storage.search_file_lines(query) {
            if let crate::model::SearchHit::FileLine {
                path,
                line_no,
                text,
            } = hit
            {
                matches.push(json!({
                    "type": "file",
                    "path": display_path(&self.root, &path),
                    "line": line_no,
                    "snippet": truncate_chars(&text, MAX_SEARCH_SNIPPET_CHARS),
                }));
            }
        }
        paginated_search_result(query, offset, limit, matches)
    }
}

struct SearchFiles {
    storage: Storage,
    root: PathBuf,
}

impl SearchFiles {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            root: canonical_root(root)?,
        })
    }
}

impl Tool for SearchFiles {
    fn name(&self) -> &'static str {
        "search_files"
    }

    fn description(&self) -> &'static str {
        "Fuzzy, case-insensitive filename search across managed .md and .mb notes, using the same matching as the Files sidebar. Supports result pagination."
    }

    fn input_schema(&self) -> Value {
        search_schema("Fuzzy filename query; the extension is not required")
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let query = required_string(input, "query")?.trim();
        if query.is_empty() {
            bail!("query must not be empty");
        }
        let offset = optional_usize(input, "offset", 0, MAX_SEARCH_OFFSET)?;
        let limit = optional_usize(input, "limit", DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS)?;
        let matches = self
            .storage
            .list_note_files()?
            .into_iter()
            .filter(|file| {
                file.path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| fuzzy_match(name, query))
            })
            .map(|file| {
                json!({
                    "path": display_path(&self.root, &file.path),
                    "name": file.path.file_name().unwrap_or_default().to_string_lossy(),
                })
            })
            .collect();
        paginated_search_result(query, offset, limit, matches)
    }
}

struct UpdateFile {
    root: PathBuf,
    private_config: PathBuf,
    chat_path: PathBuf,
    gate: ApprovalGate,
    reads: Arc<ReadTracker>,
}

impl UpdateFile {
    fn new(root: &Path, gate: ApprovalGate, reads: Arc<ReadTracker>) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            private_config: root.join("config/ai.toml"),
            chat_path: root.join("CHAT.md"),
            root,
            gate,
            reads,
        })
    }
}

impl Tool for UpdateFile {
    fn name(&self) -> &'static str {
        "update_file"
    }

    fn description(&self) -> &'static str {
        "Replace an existing UTF-8 file under the Nole root after every line has been covered by read_file in this run and, unless bypassed, user diff approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let content = required_string(input, "content")?;
        if content.len() as u64 > MAX_FILE_BYTES {
            bail!("content exceeds 1 MB");
        }
        let unresolved = safe_relative(&self.root, relative)?;
        if fs::symlink_metadata(&unresolved)?.file_type().is_symlink() {
            bail!("refusing to update through a symlink");
        }
        let path = fs::canonicalize(&unresolved)
            .with_context(|| format!("resolving existing file {}", unresolved.display()))?;
        if path == self.private_config || path == self.chat_path {
            bail!("generic file tools cannot operate on this special file");
        }
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            bail!("target must be a regular UTF-8 file no larger than 1 MB");
        }
        let old = fs::read_to_string(&path)
            .with_context(|| format!("reading current file {}", path.display()))?;
        let snapshot = self
            .reads
            .file_snapshot(&path)?
            .context("update_file requires read_file on the same path first")?;
        if snapshot != old {
            self.reads.consume_file(&path)?;
            bail!("file changed since read_file; read it again before updating");
        }
        if old == content {
            return Ok(format!("no changes needed for {relative}"));
        }
        self.gate.request(ApprovalRequest {
            title: format!("Update {relative}"),
            diff: limited_diff(&old, content, relative, relative),
        })?;
        let current =
            fs::read_to_string(&path).with_context(|| format!("rechecking {}", path.display()))?;
        if current != old {
            self.reads.consume_file(&path)?;
            bail!("file changed while awaiting approval; read it again before updating");
        }
        fs::write(&path, content).with_context(|| format!("updating {}", path.display()))?;
        self.reads.consume_file(&path)?;
        Ok(format!("updated {relative}"))
    }
}

struct ReadMessage {
    storage: Storage,
    reads: Arc<ReadTracker>,
}

impl ReadMessage {
    fn new(root: &Path, reads: Arc<ReadTracker>) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            reads,
        })
    }
}

impl Tool for ReadMessage {
    fn name(&self) -> &'static str {
        "read_message"
    }

    fn description(&self) -> &'static str {
        "Read one Chat card by its nole-msg id. Required before update_message for that id."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": { "id": { "type": "string" } },
            "required": ["id"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let id = required_string(input, "id")?;
        let message = self
            .storage
            .load_messages()?
            .into_iter()
            .find(|message| message.id == id)
            .with_context(|| format!("message not found: {id}"))?;
        self.reads
            .mark_message(id.to_string(), message.body.clone())?;
        serde_json::to_string_pretty(&json!({
            "id": message.id,
            "created_at": message.created_at.to_rfc3339(),
            "body": message.body,
        }))
        .context("encoding message")
    }
}

struct UpdateMessage {
    storage: Storage,
    gate: ApprovalGate,
    reads: Arc<ReadTracker>,
}

impl UpdateMessage {
    fn new(root: &Path, gate: ApprovalGate, reads: Arc<ReadTracker>) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            gate,
            reads,
        })
    }
}

impl Tool for UpdateMessage {
    fn name(&self) -> &'static str {
        "update_message"
    }

    fn description(&self) -> &'static str {
        "Replace an existing Chat card body by nole-msg id after read_message and, unless bypassed, user diff approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "body": { "type": "string" }
            },
            "required": ["id", "body"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let id = required_string(input, "id")?;
        let body = required_string(input, "body")?;
        if body.len() as u64 > MAX_FILE_BYTES {
            bail!("message body exceeds 1 MB");
        }
        let mut message = self
            .storage
            .load_messages()?
            .into_iter()
            .find(|message| message.id == id)
            .with_context(|| format!("message not found: {id}"))?;
        let old = message.body.clone();
        let snapshot = self
            .reads
            .message_snapshot(id)?
            .context("update_message requires read_message for the same id first")?;
        if snapshot != old {
            self.reads.consume_message(id)?;
            bail!("message changed since read_message; read it again before updating");
        }
        if old == body {
            return Ok(format!("no changes needed for message {id}"));
        }
        let label = format!("message/{id}");
        self.gate.request(ApprovalRequest {
            title: format!("Update message {id}"),
            diff: limited_diff(&old, body, &label, &label),
        })?;
        let current = self
            .storage
            .load_messages()?
            .into_iter()
            .find(|candidate| candidate.id == id)
            .with_context(|| format!("message disappeared while awaiting approval: {id}"))?;
        if current.body != old {
            self.reads.consume_message(id)?;
            bail!("message changed while awaiting approval; read it again before updating");
        }
        message.body = body.to_string();
        if !self.storage.replace_message(&message)? {
            bail!("message not found: {id}");
        }
        self.reads.consume_message(id)?;
        Ok(format!("updated message {id}"))
    }
}

struct AddMessage {
    storage: Storage,
}

impl AddMessage {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
        })
    }
}

impl Tool for AddMessage {
    fn name(&self) -> &'static str {
        "add_message"
    }

    fn description(&self) -> &'static str {
        "Append a new Chat card. This operation does not require approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": { "body": { "type": "string" } },
            "required": ["body"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let body = required_string(input, "body")?;
        if body.len() as u64 > MAX_FILE_BYTES {
            bail!("message body exceeds 1 MB");
        }
        let message = self.storage.append_chat_message(body)?;
        serde_json::to_string(&json!({ "id": message.id })).context("encoding new message result")
    }
}

struct Notify {
    events: Sender<AgentEvent>,
}

impl Tool for Notify {
    fn name(&self) -> &'static str {
        "notify"
    }

    fn description(&self) -> &'static str {
        "Show a short, temporary notification in the top-right of the user's TUI."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": {
                "message": { "type": "string", "maxLength": 500 }
            },
            "required": ["message"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let message = required_string(input, "message")?;
        if message.trim().is_empty() {
            bail!("notification message is empty");
        }
        if message.chars().count() > 500 {
            bail!("notification message exceeds 500 characters");
        }
        self.events
            .send(AgentEvent::Notification(message.to_string()))
            .context("sending notification")?;
        Ok("notification shown".to_string())
    }
}

struct AskUser {
    events: Sender<AgentEvent>,
    responses: Arc<Mutex<Receiver<AskUserResponse>>>,
}

impl Tool for AskUser {
    fn name(&self) -> &'static str {
        "ask_user"
    }

    fn description(&self) -> &'static str {
        "Ask the user a blocking clarification question in the TUI. Optional choices may be provided, and the user can always enter a different free-text answer."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "minLength": 1, "maxLength": 2000 },
                "options": {
                    "type": "array", "maxItems": 10,
                    "items": { "type": "string", "minLength": 1, "maxLength": 200 }
                }
            },
            "required": ["question"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let question = required_string(input, "question")?.trim();
        if question.is_empty() {
            bail!("question must not be empty");
        }
        if question.chars().count() > 2_000 {
            bail!("question exceeds 2000 characters");
        }
        let options = input
            .get("options")
            .map(|value| {
                value
                    .as_array()
                    .context("field options must be an array")?
                    .iter()
                    .map(|option| {
                        let option = option
                            .as_str()
                            .context("each option must be a string")?
                            .trim();
                        if option.is_empty() {
                            bail!("options must not be empty");
                        }
                        if option.chars().count() > 200 {
                            bail!("option exceeds 200 characters");
                        }
                        Ok(option.to_string())
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        if options.len() > 10 {
            bail!("at most 10 options are allowed");
        }
        self.events
            .send(AgentEvent::AskUser(AskUserRequest {
                question: question.to_string(),
                options,
            }))
            .context("sending question to user")?;
        match self
            .responses
            .lock()
            .map_err(|_| anyhow::anyhow!("user response channel lock poisoned"))?
            .recv()
            .context("waiting for user response")?
        {
            AskUserResponse::Answer(answer) => Ok(answer),
            AskUserResponse::Cancelled => bail!("user cancelled the question"),
        }
    }
}

fn limited_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let diff = TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(old_label, new_label)
        .to_string();
    if diff.len() <= MAX_DIFF_BYTES {
        return diff;
    }
    let mut end = MAX_DIFF_BYTES;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... diff truncated ...\n", &diff[..end])
}

struct WriteFile {
    root: PathBuf,
    private_config: PathBuf,
    chat_path: PathBuf,
}

impl WriteFile {
    fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            private_config: root.join("config/ai.toml"),
            chat_path: root.join("CHAT.md"),
            root,
        })
    }
}

impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Create a new UTF-8 text file under the Nole root. Fails if the path already exists."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"], "additionalProperties": false
        })
    }
    fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let content = required_string(input, "content")?;
        if content.len() as u64 > MAX_FILE_BYTES {
            bail!("content exceeds 1 MB");
        }
        let path = safe_relative(&self.root, relative)?;
        if path == self.private_config || path == self.chat_path {
            bail!("generic file tools cannot operate on this special file");
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("creating new file {}", path.display()))?;
        file.write_all(content.as_bytes())?;
        Ok(format!("wrote {} bytes to {relative}", content.len()))
    }
}

struct WebFetch {
    client: Client,
}

impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn description(&self) -> &'static str {
        "Fetch the text content of an HTTP or HTTPS URL (maximum 1 MB)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": { "url": { "type": "string" } },
            "required": ["url"], "additionalProperties": false
        })
    }
    fn execute(&self, input: &Value) -> Result<String> {
        let url = required_string(input, "url")?;
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            bail!("URL must use http or https");
        }
        let response = self
            .client
            .get(url)
            .send()
            .with_context(|| format!("fetching {url}"))?;
        if !response.status().is_success() {
            bail!("fetch returned HTTP {}", response.status());
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_FETCH_BYTES)
        {
            bail!("response exceeds 1 MB");
        }
        let mut bytes = Vec::new();
        response.take(MAX_FETCH_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FETCH_BYTES {
            bail!("response exceeds 1 MB");
        }
        String::from_utf8(bytes).context("response is not UTF-8 text")
    }
}

fn search_schema(query_description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": query_description },
            "offset": {
                "type": "integer", "minimum": 0,
                "maximum": MAX_SEARCH_OFFSET, "default": 0
            },
            "limit": {
                "type": "integer", "minimum": 1,
                "maximum": MAX_SEARCH_RESULTS, "default": DEFAULT_SEARCH_RESULTS
            }
        },
        "required": ["query"], "additionalProperties": false
    })
}

fn paginated_search_result(
    query: &str,
    offset: usize,
    limit: usize,
    matches: Vec<Value>,
) -> Result<String> {
    let total = matches.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    serde_json::to_string_pretty(&json!({
        "query": query,
        "offset": start,
        "returned": end - start,
        "total_matches": total,
        "has_more": end < total,
        "matches": &matches[start..end],
    }))
    .context("encoding search results")
}

fn display_path(root: &Path, path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let mut offset = 0;
    for wanted in needle.to_lowercase().chars() {
        let Some(found) = hay[offset..]
            .iter()
            .position(|candidate| *candidate == wanted)
        else {
            return false;
        };
        offset += found + 1;
    }
    true
}

fn matching_line(body: &str, query_lower: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.to_lowercase().contains(query_lower))
        .or_else(|| body.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("");
    truncate_chars(line, MAX_SEARCH_SNIPPET_CHARS)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn optional_usize(input: &Value, key: &str, default: usize, maximum: usize) -> Result<usize> {
    let Some(value) = input.get(key) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .with_context(|| format!("field {key} must be a non-negative integer"))?;
    let value = usize::try_from(value).with_context(|| format!("field {key} is too large"))?;
    if value > maximum || (key == "limit" && value == 0) {
        bail!(
            "field {key} must be between {} and {maximum}",
            usize::from(key == "limit")
        );
    }
    Ok(value)
}

fn required_string<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string field {key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bypass_gate() -> ApprovalGate {
        let (event_sender, _event_receiver) = std::sync::mpsc::channel();
        let (_decision_sender, decision_receiver) = std::sync::mpsc::channel();
        ApprovalGate {
            bypass: Arc::new(AtomicBool::new(true)),
            events: event_sender,
            decisions: Arc::new(Mutex::new(decision_receiver)),
        }
    }

    #[test]
    fn file_tools_stay_inside_root() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        let write = WriteFile::new(directory.path()).unwrap();
        write
            .execute(&json!({"path": "data/test.md", "content": "hello"}))
            .unwrap();
        let read = ReadFile::new(directory.path(), Arc::new(ReadTracker::default())).unwrap();
        let result: Value =
            serde_json::from_str(&read.execute(&json!({"path": "data/test.md"})).unwrap()).unwrap();
        assert_eq!(result["content"], "hello");
        assert_eq!(result["total_lines"], 1);
        assert_eq!(result["has_more"], false);
        let outside_directory = tempfile::tempdir().unwrap();
        let outside = outside_directory.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        let result: Value =
            serde_json::from_str(&read.execute(&json!({"path": outside})).unwrap()).unwrap();
        assert_eq!(result["content"], "outside");
    }

    #[test]
    fn paginated_file_reads_require_complete_coverage_before_update() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::write(directory.path().join("CHAT.md"), "").unwrap();
        let content = (0..450)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(directory.path().join("data/large.md"), &content).unwrap();
        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let update = UpdateFile::new(directory.path(), bypass_gate(), reads).unwrap();

        let first: Value = serde_json::from_str(
            &read
                .execute(&json!({"path": "data/large.md", "offset": 0, "limit": 200}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first["returned_lines"], 200);
        assert_eq!(first["total_lines"], 450);
        assert_eq!(first["has_more"], true);
        assert!(update
            .execute(&json!({"path": "data/large.md", "content": content}))
            .is_err());

        read.execute(&json!({"path": "data/large.md", "offset": 200, "limit": 200}))
            .unwrap();
        read.execute(&json!({"path": "data/large.md", "offset": 400, "limit": 100}))
            .unwrap();
        update
            .execute(&json!({"path": "data/large.md", "content": format!("{content}done\n")}))
            .unwrap();
    }

    #[test]
    fn content_and_filename_searches_are_structured_and_paginated() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage.append_chat_message("Rust in Chat").unwrap();
        fs::write(
            directory.path().join("data/RustProject.md"),
            "# Project\nRust in a note\n",
        )
        .unwrap();
        fs::write(directory.path().join("data/Research.md"), "unrelated\n").unwrap();

        let content = SearchContent::new(directory.path()).unwrap();
        let result: Value = serde_json::from_str(
            &content
                .execute(&json!({"query": "rust", "limit": 1}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["returned"], 1);
        assert_eq!(result["total_matches"], 2);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["matches"][0]["type"], "message");

        let files = SearchFiles::new(directory.path()).unwrap();
        let result: Value = serde_json::from_str(
            &files
                .execute(&json!({"query": "rsprj", "offset": 0, "limit": 10}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total_matches"], 1);
        assert_eq!(result["matches"][0]["path"], "data/RustProject.md");
    }

    #[test]
    fn private_config_is_not_available_to_tools() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::write(directory.path().join("config/ai.toml"), "api_key='secret'").unwrap();
        let read = ReadFile::new(directory.path(), Arc::new(ReadTracker::default())).unwrap();
        assert!(read.execute(&json!({"path": "config/ai.toml"})).is_err());
    }

    #[test]
    fn directory_listing_is_shallow_by_default_and_recurses_to_a_limit() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("data/nested/deep")).unwrap();
        fs::write(directory.path().join("data/root.md"), "root").unwrap();
        fs::write(directory.path().join("data/nested/child.md"), "child").unwrap();
        fs::write(directory.path().join("data/nested/deep/leaf.md"), "leaf").unwrap();
        let list = ListDirectory::new(directory.path()).unwrap();

        let shallow: Value =
            serde_json::from_str(&list.execute(&json!({"path": "data"})).unwrap()).unwrap();
        let shallow_paths = shallow["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(shallow_paths, ["nested", "root.md"]);

        let recursive: Value = serde_json::from_str(
            &list
                .execute(&json!({"path": "data", "recursive": true, "max_depth": 2}))
                .unwrap(),
        )
        .unwrap();
        let recursive_paths = recursive["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            recursive_paths,
            ["nested", "root.md", "nested/child.md", "nested/deep"]
        );
    }

    #[test]
    fn file_update_requires_read_and_write_file_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::write(directory.path().join("CHAT.md"), "").unwrap();
        fs::write(directory.path().join("data/note.md"), "old\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let update = UpdateFile::new(directory.path(), bypass_gate(), reads.clone()).unwrap();
        let input = json!({"path": "data/note.md", "content": "new\n"});
        assert!(update.execute(&input).is_err());

        let read = ReadFile::new(directory.path(), reads).unwrap();
        read.execute(&json!({"path": "data/note.md"})).unwrap();
        update.execute(&input).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("data/note.md")).unwrap(),
            "new\n"
        );
        assert!(update
            .execute(&json!({"path": "data/note.md", "content": "again\n"}))
            .is_err());

        let write = WriteFile::new(directory.path()).unwrap();
        assert!(write
            .execute(&json!({"path": "data/note.md", "content": "overwrite"}))
            .is_err());
    }

    #[test]
    fn message_update_requires_read_but_add_does_not_require_approval() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let message = storage.append_chat_message("old").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let update = UpdateMessage::new(directory.path(), bypass_gate(), reads.clone()).unwrap();
        let input = json!({"id": message.id.clone(), "body": "new"});
        assert!(update.execute(&input).is_err());

        let read = ReadMessage::new(directory.path(), reads).unwrap();
        read.execute(&json!({"id": message.id})).unwrap();
        update.execute(&input).unwrap();
        assert_eq!(storage.load_messages().unwrap()[0].body, "new");
        assert!(update
            .execute(&json!({"id": message.id, "body": "again"}))
            .is_err());

        let add = AddMessage::new(directory.path()).unwrap();
        add.execute(&json!({"body": "added"})).unwrap();
        assert_eq!(storage.load_messages().unwrap().len(), 2);
    }

    #[test]
    fn update_waits_for_diff_approval() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::write(directory.path().join("CHAT.md"), "").unwrap();
        fs::write(directory.path().join("data/note.md"), "old\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "data/note.md"}))
            .unwrap();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (decision_sender, decision_receiver) = std::sync::mpsc::channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(Mutex::new(decision_receiver)),
        };
        let update = UpdateFile::new(directory.path(), gate, reads).unwrap();
        let worker = std::thread::spawn(move || {
            update.execute(&json!({"path": "data/note.md", "content": "new\n"}))
        });

        let AgentEvent::Approval(request) = event_receiver.recv().unwrap() else {
            panic!("expected approval request");
        };
        assert!(request.diff.contains("-old"));
        assert!(request.diff.contains("+new"));
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("data/note.md")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn notify_tool_emits_a_tui_notification_event() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let tool = Notify { events: sender };
        tool.execute(&json!({"message": "Work complete"})).unwrap();
        let AgentEvent::Notification(message) = receiver.recv().unwrap() else {
            panic!("expected notification event");
        };
        assert_eq!(message, "Work complete");
    }

    #[test]
    fn ask_user_waits_for_and_returns_the_tui_answer() {
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (response_sender, response_receiver) = std::sync::mpsc::channel();
        let tool = AskUser {
            events: event_sender,
            responses: Arc::new(Mutex::new(response_receiver)),
        };
        let worker = std::thread::spawn(move || {
            tool.execute(&json!({
                "question": "Which format?",
                "options": ["Markdown", "MBDown"]
            }))
        });

        let AgentEvent::AskUser(request) = event_receiver.recv().unwrap() else {
            panic!("expected user question");
        };
        assert_eq!(request.question, "Which format?");
        assert_eq!(request.options, ["Markdown", "MBDown"]);
        response_sender
            .send(AskUserResponse::Answer("MBDown".to_string()))
            .unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), "MBDown");
    }
}
