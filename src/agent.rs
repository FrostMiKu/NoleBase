//! Small Anthropic Messages API agent with a registry of local tools.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local, NaiveDate};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use similar::TextDiff;

use crate::storage::Storage;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_AGENT_ROUNDS: usize = 12;
const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_FETCH_BYTES: u64 = 1_000_000;
const MAX_NOTE_RESULTS: usize = 2_000;
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
    FileMoved { from: PathBuf, to: PathBuf },
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

#[derive(Clone)]
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

    fn file_state(&self, path: &Path) -> Result<Option<FileReadState>> {
        let files = self
            .files
            .lock()
            .map_err(|_| anyhow::anyhow!("file read tracker lock poisoned"))?;
        Ok(files.get(path).cloned())
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

impl FileReadState {
    fn covers(&self, start: usize, end: usize) -> bool {
        start == end
            || self
                .ranges
                .iter()
                .any(|range| range.0 <= start && range.1 >= end)
    }

    fn ensure_edit_read(&self, start_line: usize, end_line: usize) -> Result<()> {
        if start_line < end_line {
            if !self.covers(start_line, end_line) {
                bail!(
                    "update_file must read changed zero-based lines {start_line}..{end_line} first"
                );
            }
        } else if self.total_lines > 0 {
            let anchor_start = start_line.saturating_sub(1);
            let anchor_end = (start_line + 1).min(self.total_lines);
            if !self.covers(anchor_start, anchor_end) {
                bail!(
                    "update_file must read insertion anchor lines {anchor_start}..{anchor_end} first"
                );
            }
        }
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
    cancelled: Arc<AtomicBool>,
}

impl Agent {
    pub fn from_config(
        config_path: &Path,
        nole_root: &Path,
        events: Sender<AgentEvent>,
        decisions: Receiver<ApprovalDecision>,
        user_responses: Receiver<AskUserResponse>,
        bypass: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
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
            cancelled,
        };
        let gate = ApprovalGate {
            bypass,
            events,
            decisions: Arc::new(Mutex::new(decisions)),
        };
        let reads = Arc::new(ReadTracker::default());
        agent.register(ReadFile::new(nole_root, reads.clone())?);
        agent.register(ListNotes::new(nole_root)?);
        agent.register(SearchContent::new(nole_root)?);
        agent.register(SearchFiles::new(nole_root)?);
        agent.register(WriteFile::new(nole_root)?);
        agent.register(CopyFile::new(nole_root)?);
        let file_events = agent.events.clone();
        agent.register(MoveFile::new(nole_root, file_events.clone())?);
        agent.register(MoveFiles::new(nole_root, file_events.clone())?);
        agent.register(RenameFile::new(nole_root, file_events)?);
        agent.register(DeleteFile::new(nole_root, gate.clone())?);
        agent.register(UpdateFile::new(nole_root, gate.clone(), reads.clone())?);
        agent.register(ReadDaily::new(nole_root, reads.clone())?);
        agent.register(UpdateDaily::new(nole_root, gate, reads)?);
        agent.register(AppendDaily::new(nole_root)?);
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
        let prompt = prompt_with_datetime(prompt, Local::now());
        let mut messages = vec![json!({ "role": "user", "content": prompt })];
        let definitions: Vec<Value> = self.tools.values().map(|tool| tool.definition()).collect();

        for _ in 0..MAX_AGENT_ROUNDS {
            self.ensure_active()?;
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
            self.ensure_active()?;
            let status = response.status();
            let body = response.text().context("reading Anthropic response")?;
            self.ensure_active()?;
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
            self.ensure_active()?;
            let results: Vec<Value> = tool_uses
                .into_iter()
                .map(|call| self.execute_tool_call(call))
                .collect();
            messages.push(json!({ "role": "user", "content": results }));
        }
        bail!("agent exceeded {MAX_AGENT_ROUNDS} tool-call rounds")
    }

    fn ensure_active(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            bail!("agent task cancelled");
        }
        Ok(())
    }

    fn execute_tool_call(&self, call: &Value) -> Value {
        let id = call.get("id").and_then(Value::as_str).unwrap_or("");
        let name = call.get("name").and_then(Value::as_str).unwrap_or("");
        let input = call.get("input").unwrap_or(&Value::Null);
        if let Err(error) = self.ensure_active() {
            return json!({
                "type": "tool_result", "tool_use_id": id,
                "content": error.to_string(), "is_error": true
            });
        }
        let _ = self
            .events
            .send(AgentEvent::Progress(tool_start_activity(name)));
        let result = self
            .tools
            .get(name)
            .context("unknown tool")
            .and_then(|tool| tool.execute(input));
        match result {
            Ok(content) => {
                let _ = self.events.send(AgentEvent::Progress(format!(
                    "Completed {}.",
                    tool_display_name(name)
                )));
                json!({
                "type": "tool_result", "tool_use_id": id, "content": content
                })
            }
            Err(error) => {
                let _ = self.events.send(AgentEvent::Progress(format!(
                    "Failed {}: {error}",
                    tool_display_name(name)
                )));
                json!({
                "type": "tool_result", "tool_use_id": id,
                "content": error.to_string(), "is_error": true
                })
            }
        }
    }
}

fn tool_start_activity(name: &str) -> String {
    if name == "web_fetch" {
        "Fetching Web...".to_string()
    } else {
        format!("Calling {}...", tool_display_name(name))
    }
}

fn tool_display_name(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(characters).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn system_prompt(root: &Path) -> String {
    format!(
        r#"You are the AI assistant inside Nole, a chat-style terminal note app.
This is a single-turn task. After your final response, the user cannot reply in the same conversation and you will not see a follow-up. If you need clarification, confirmation, a choice, or any other user input to complete the task, you MUST call ask_user before returning your final response. Never put a question that requires an answer in the final response.
Answer the user's request directly. Your final response is shown in the Agent panel, so return only useful content, without discussing tool mechanics.

Nole renders MBDown. MBDown supports CommonMark headings, emphasis, strong text, strikethrough, links, lists, task lists, fenced code, block quotes, and tables. It also supports #tag at word boundaries and [[wikilink]] for references between notes. Tag names may use Unicode letters and numbers plus _, -, and /. Keep both forms literal inside code. Clicking a wikilink searches data/ and archives/; if multiple MD/MB files match, the user chooses in a popup, and if none match Nole creates a new data note. MBDown also supports restricted BBCode:
- inline: [b], [i], [u], [s], [dim], named colors such as [red], [color=196], [color=#12abef], [bg=blue], and [link=https://example.com]label[/link]
- layout: [center]...[/center], [right]...[/right], [indent first=4]...[/indent]
- boxes: [box title="Info" width=full border=single border-color=#12abef bg=17 px=1 py=0]...[/box]
- responsive columns: [columns gap=2][column width=1fr]...[/column][column width=2fr]...[/column][/columns]
Use ordinary Markdown unless richer MBDown layout materially improves the answer. Close every BBCode tag and never emit raw terminal escape sequences.

The Nole root is {root}. Special paths are:
- daily/: chat cards stored as YYYY-MM-DD.md, one card per day
- archives/: archived daily files, retaining the same YYYY-MM-DD.md name
- config/ai.toml: Anthropic API configuration; never read or expose secrets from it
- data/: flat user note storage; notes use .md or .mb
Relative file paths use this root. read_file accepts absolute paths, but write_file, update_file, and all destinations are restricted to this root. read_file is line-paginated; use offset and limit to inspect only relevant portions. Generic file tools must never operate inside daily/ or on config/ai.toml. Use list_notes to inspect managed notes and sort them by name, line count, creation time, modification time, or file size. Use search_content for full-text search across daily cards and notes, and search_files for fuzzy note-name search. copy_file and move_file accept a source anywhere on the filesystem, but only create a non-existing destination under the Nole root and do not require approval. Use move_files to move up to 200 files into one existing Nole directory while preserving basenames. Use rename_file for a same-directory rename under Nole instead of expressing renames as moves. delete_file only deletes a regular file under the Nole root and requires approval unless permission checks are bypassed. read_daily requires an inclusive start_date and end_date and returns all existing cards in that range; use the same date for both bounds to read one card. Use update_daily to replace an existing daily card and append_daily to append content for a date. append_daily is not approval-gated, while updates may pause for user approval. write_file only creates new files. update_file applies zero-based line edits and preserves all content outside each [start_line, end_line) range; replacement content is exact, so include required newlines. Before update_file you MUST read each changed or deleted range of the exact file in this agent run; insertions require the adjacent anchor lines. You do not need to read or submit unrelated portions of a large file. Before update_daily you MUST read a range containing the exact date. Updates are automatically rejected without the required read, even in bypass mode. Todo items are Markdown task-list items scanned from all files in daily/. Use ask_user when a missing decision or ambiguity materially affects the result; include concise options when useful, while allowing free-text answers. Use notify to surface a short, time-sensitive message in the user's TUI. Do not assume your final response is added to daily: call append_daily when content belongs there. Your final text is shown in the Agent output panel. Use tools only when the request requires local context or changes."#,
        root = root.display()
    )
}

fn prompt_with_datetime(prompt: &str, now: DateTime<Local>) -> String {
    format!(
        "Current local date and time: {}\n\n{prompt}",
        now.to_rfc3339()
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
    let name = path.file_name().context("path must name a file")?;
    Ok(canonical_parent.join(name))
}

struct ReadFile {
    root: PathBuf,
    private_config: PathBuf,
    daily_dir: PathBuf,
    reads: Arc<ReadTracker>,
}

impl ReadFile {
    fn new(root: &Path, reads: Arc<ReadTracker>) -> Result<Self> {
        let root = canonical_root(root)?;
        let private_config = fs::canonicalize(root.join("config/ai.toml"))
            .unwrap_or_else(|_| root.join("config/ai.toml"));
        Ok(Self {
            private_config,
            daily_dir: root.join("daily"),
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
        if path == self.private_config || path.starts_with(&self.daily_dir) {
            bail!("use daily tools for daily cards; AI configuration is private");
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

struct NoteMetadata {
    path: PathBuf,
    name: String,
    line_count: u64,
    created: Option<std::time::SystemTime>,
    modified: std::time::SystemTime,
    size: u64,
}

struct ListNotes {
    storage: Storage,
    root: PathBuf,
}

impl ListNotes {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            root: canonical_root(root)?,
        })
    }
}

impl Tool for ListNotes {
    fn name(&self) -> &'static str {
        "list_notes"
    }

    fn description(&self) -> &'static str {
        "List managed .md and .mb notes with line count, creation time, modification time, and byte size. Supports metadata sorting and pagination."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sort_by": {
                    "type": "string",
                    "enum": ["name", "line_count", "created_at", "modified_at", "size"],
                    "default": "modified_at"
                },
                "order": { "type": "string", "enum": ["asc", "desc"], "default": "desc" },
                "offset": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": {
                    "type": "integer", "minimum": 1,
                    "maximum": MAX_NOTE_RESULTS, "default": 200
                }
            },
            "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let sort_by = input
            .get("sort_by")
            .and_then(Value::as_str)
            .unwrap_or("modified_at");
        let descending = match input.get("order").and_then(Value::as_str).unwrap_or("desc") {
            "asc" => false,
            "desc" => true,
            other => bail!("unsupported order: {other}"),
        };
        if !matches!(
            sort_by,
            "name" | "line_count" | "created_at" | "modified_at" | "size"
        ) {
            bail!("unsupported sort_by: {sort_by}");
        }
        let offset = optional_usize(input, "offset", 0, usize::MAX)?;
        let limit = optional_usize(input, "limit", 200, MAX_NOTE_RESULTS)?;
        let mut notes = self
            .storage
            .list_note_files()?
            .into_iter()
            .map(|note| note_metadata(note.path))
            .collect::<Result<Vec<_>>>()?;
        notes.sort_by(|a, b| {
            let ordering = match sort_by {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "line_count" => a.line_count.cmp(&b.line_count),
                "created_at" => a.created.cmp(&b.created),
                "modified_at" => a.modified.cmp(&b.modified),
                "size" => a.size.cmp(&b.size),
                _ => unreachable!(),
            };
            let ordering = if descending {
                ordering.reverse()
            } else {
                ordering
            };
            ordering.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        let total = notes.len();
        let start = offset.min(total);
        let end = start.saturating_add(limit).min(total);
        let entries = notes[start..end]
            .iter()
            .map(|note| json!({
                "path": display_path(&self.root, &note.path),
                "name": note.name,
                "line_count": note.line_count,
                "created_at": note.created.map(|time| DateTime::<Local>::from(time).to_rfc3339()),
                "modified_at": DateTime::<Local>::from(note.modified).to_rfc3339(),
                "size": note.size,
            }))
            .collect::<Vec<_>>();
        serde_json::to_string_pretty(&json!({
            "sort_by": sort_by,
            "order": if descending { "desc" } else { "asc" },
            "offset": start,
            "returned": end - start,
            "total": total,
            "has_more": end < total,
            "entries": entries,
        }))
        .context("encoding note listing")
    }
}

fn note_metadata(path: PathBuf) -> Result<NoteMetadata> {
    let metadata =
        fs::metadata(&path).with_context(|| format!("reading metadata for {}", path.display()))?;
    let mut reader = BufReader::new(fs::File::open(&path)?);
    let mut buffer = Vec::new();
    let mut line_count = 0u64;
    while reader.read_until(b'\n', &mut buffer)? != 0 {
        line_count += 1;
        buffer.clear();
    }
    Ok(NoteMetadata {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path,
        line_count,
        created: metadata.created().ok(),
        modified: metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        size: metadata.len(),
    })
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
        "Case-insensitive full-text search across daily Chat cards and managed note files. Returns daily dates or note paths with matching snippets and supports result pagination."
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
                    "type": "daily",
                    "date": message.id,
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
    daily_dir: PathBuf,
    gate: ApprovalGate,
    reads: Arc<ReadTracker>,
}

impl UpdateFile {
    fn new(root: &Path, gate: ApprovalGate, reads: Arc<ReadTracker>) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            private_config: root.join("config/ai.toml"),
            daily_dir: root.join("daily"),
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
        "Apply one or more zero-based line edits to an existing UTF-8 file under the Nole root while preserving all other content. Each edit replaces [start_line, end_line) with exact content. Changed/deleted lines, or adjacent anchors for insertions, must have been read in this run. Requires user diff approval unless bypassed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "edits": {
                    "type": "array", "minItems": 1, "maxItems": 100,
                    "items": {
                        "type": "object",
                        "properties": {
                            "start_line": {
                                "type": "integer", "minimum": 0,
                                "description": "Zero-based inclusive source line"
                            },
                            "end_line": {
                                "type": "integer", "minimum": 0,
                                "description": "Zero-based exclusive source line"
                            },
                            "content": {
                                "type": "string",
                                "description": "Exact replacement text; include newlines where required"
                            }
                        },
                        "required": ["start_line", "end_line", "content"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["path", "edits"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let edits = parse_line_edits(input)?;
        let unresolved = safe_relative(&self.root, relative)?;
        if fs::symlink_metadata(&unresolved)?.file_type().is_symlink() {
            bail!("refusing to update through a symlink");
        }
        let path = fs::canonicalize(&unresolved)
            .with_context(|| format!("resolving existing file {}", unresolved.display()))?;
        if path == self.private_config || path.starts_with(&self.daily_dir) {
            bail!("generic file tools cannot operate on this special file");
        }
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            bail!("target must be a regular UTF-8 file no larger than 1 MB");
        }
        let old = fs::read_to_string(&path)
            .with_context(|| format!("reading current file {}", path.display()))?;
        let state = self
            .reads
            .file_state(&path)?
            .context("update_file requires read_file on the same path first")?;
        if state.snapshot != old {
            self.reads.consume_file(&path)?;
            bail!("file changed since read_file; read it again before updating");
        }
        let offsets = line_byte_offsets(&old);
        let total_lines = offsets.len().saturating_sub(1);
        for edit in &edits {
            if edit.start_line > edit.end_line || edit.end_line > total_lines {
                bail!(
                    "invalid edit range {}..{} for file with {total_lines} lines",
                    edit.start_line,
                    edit.end_line
                );
            }
            state.ensure_edit_read(edit.start_line, edit.end_line)?;
        }
        let content = apply_line_edits(&old, &offsets, &edits);
        if content.len() as u64 > MAX_FILE_BYTES {
            bail!("updated content exceeds 1 MB");
        }
        if old == content {
            return Ok(format!("no changes needed for {relative}"));
        }
        self.gate.request(ApprovalRequest {
            title: format!("Update {relative}"),
            diff: limited_diff(&old, &content, relative, relative),
        })?;
        let current =
            fs::read_to_string(&path).with_context(|| format!("rechecking {}", path.display()))?;
        if current != old {
            self.reads.consume_file(&path)?;
            bail!("file changed while awaiting approval; read it again before updating");
        }
        fs::write(&path, &content).with_context(|| format!("updating {}", path.display()))?;
        self.reads.consume_file(&path)?;
        Ok(format!("updated {relative}"))
    }
}

#[derive(Debug)]
struct LineEdit {
    start_line: usize,
    end_line: usize,
    content: String,
}

fn parse_line_edits(input: &Value) -> Result<Vec<LineEdit>> {
    let values = input
        .get("edits")
        .and_then(Value::as_array)
        .context("field edits must be an array")?;
    if values.is_empty() || values.len() > 100 {
        bail!("edits must contain between 1 and 100 entries");
    }
    let mut edits = values
        .iter()
        .map(|value| {
            let start_line = value
                .get("start_line")
                .and_then(Value::as_u64)
                .context("edit start_line must be a non-negative integer")?;
            let end_line = value
                .get("end_line")
                .and_then(Value::as_u64)
                .context("edit end_line must be a non-negative integer")?;
            Ok(LineEdit {
                start_line: usize::try_from(start_line).context("start_line is too large")?,
                end_line: usize::try_from(end_line).context("end_line is too large")?,
                content: required_string(value, "content")?.to_string(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    edits.sort_by_key(|edit| (edit.start_line, edit.end_line));
    for pair in edits.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if current.start_line < previous.end_line || current.start_line == previous.start_line {
            bail!("edits must not overlap or share a start_line");
        }
    }
    Ok(edits)
}

fn line_byte_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            offsets.push(index + 1);
        }
    }
    if offsets.last().copied() != Some(content.len()) {
        offsets.push(content.len());
    }
    offsets
}

fn apply_line_edits(old: &str, offsets: &[usize], edits: &[LineEdit]) -> String {
    let mut content = old.to_string();
    for edit in edits.iter().rev() {
        content.replace_range(
            offsets[edit.start_line]..offsets[edit.end_line],
            &edit.content,
        );
    }
    content
}

struct ReadDaily {
    storage: Storage,
    reads: Arc<ReadTracker>,
}

impl ReadDaily {
    fn new(root: &Path, reads: Arc<ReadTracker>) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            reads,
        })
    }
}

impl Tool for ReadDaily {
    fn name(&self) -> &'static str {
        "read_daily"
    }

    fn description(&self) -> &'static str {
        "Read all existing daily cards in an inclusive YYYY-MM-DD date range. Use the same start_date and end_date for one day. Every returned card is eligible for update_daily in this Agent run."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "start_date": {
                    "type": "string", "description": "Inclusive YYYY-MM-DD start date"
                },
                "end_date": {
                    "type": "string", "description": "Inclusive YYYY-MM-DD end date"
                }
            },
            "required": ["start_date", "end_date"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let start_text = required_string(input, "start_date")?;
        let end_text = required_string(input, "end_date")?;
        let start = NaiveDate::parse_from_str(start_text, "%Y-%m-%d")
            .with_context(|| format!("invalid start_date: {start_text}"))?;
        let end = NaiveDate::parse_from_str(end_text, "%Y-%m-%d")
            .with_context(|| format!("invalid end_date: {end_text}"))?;
        if start > end {
            bail!("start_date must not be after end_date");
        }
        let messages = self
            .storage
            .load_messages()?
            .into_iter()
            .filter(|message| {
                let date = message.created_at.date_naive();
                date >= start && date <= end
            })
            .collect::<Vec<_>>();
        let cards = messages
            .iter()
            .map(|message| json!({ "date": message.id, "body": message.body }))
            .collect::<Vec<_>>();
        let encoded = serde_json::to_string_pretty(&json!({
            "start_date": start_text,
            "end_date": end_text,
            "count": cards.len(),
            "cards": cards,
        }))
        .context("encoding daily range")?;
        if encoded.len() as u64 > MAX_FETCH_BYTES {
            bail!("daily range response exceeds 1 MB; request a narrower date range");
        }
        for message in messages {
            self.reads.mark_message(message.id, message.body)?;
        }
        Ok(encoded)
    }
}

struct UpdateDaily {
    storage: Storage,
    gate: ApprovalGate,
    reads: Arc<ReadTracker>,
}

impl UpdateDaily {
    fn new(root: &Path, gate: ApprovalGate, reads: Arc<ReadTracker>) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
            gate,
            reads,
        })
    }
}

impl Tool for UpdateDaily {
    fn name(&self) -> &'static str {
        "update_daily"
    }

    fn description(&self) -> &'static str {
        "Replace an existing daily card body by YYYY-MM-DD date after read_daily and, unless bypassed, user diff approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "date": { "type": "string" },
                "body": { "type": "string" }
            },
            "required": ["date", "body"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let date = required_string(input, "date")?;
        let body = required_string(input, "body")?;
        if body.len() as u64 > MAX_FILE_BYTES {
            bail!("message body exceeds 1 MB");
        }
        let mut message = self.storage.read_daily_by_date(date)?;
        let old = message.body.clone();
        let snapshot = self
            .reads
            .message_snapshot(date)?
            .context("update_daily requires read_daily for the same date first")?;
        if snapshot != old {
            self.reads.consume_message(date)?;
            bail!("daily card changed since read_daily; read it again before updating");
        }
        if old == body {
            return Ok(format!("no changes needed for daily {date}"));
        }
        let label = format!("daily/{date}.md");
        self.gate.request(ApprovalRequest {
            title: format!("Update daily {date}"),
            diff: limited_diff(&old, body, &label, &label),
        })?;
        let current = self
            .storage
            .read_daily_by_date(date)
            .with_context(|| format!("daily card disappeared while awaiting approval: {date}"))?;
        if current.body != old {
            self.reads.consume_message(date)?;
            bail!("daily card changed while awaiting approval; read it again before updating");
        }
        message.body = body.to_string();
        if !self.storage.replace_message(&message)? {
            bail!("daily card not found: {date}");
        }
        self.reads.consume_message(date)?;
        Ok(format!("updated daily {date}"))
    }
}

struct AppendDaily {
    storage: Storage,
}

impl AppendDaily {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            storage: Storage::new(root)?,
        })
    }
}

impl Tool for AppendDaily {
    fn name(&self) -> &'static str {
        "append_daily"
    }

    fn description(&self) -> &'static str {
        "Append content to a YYYY-MM-DD daily card, creating it if absent. This operation does not require approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": {
                "date": { "type": "string" }, "body": { "type": "string" }
            },
            "required": ["date", "body"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let date = required_string(input, "date")?;
        let body = required_string(input, "body")?;
        if body.len() as u64 > MAX_FILE_BYTES {
            bail!("message body exceeds 1 MB");
        }
        let message = self.storage.append_daily(date, body)?;
        serde_json::to_string(&json!({ "date": message.id })).context("encoding daily result")
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

fn resolve_transfer_source(root: &Path, input: &str) -> Result<PathBuf> {
    let unresolved = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        root.join(input)
    };
    let file_type = fs::symlink_metadata(&unresolved)
        .with_context(|| format!("checking source {}", unresolved.display()))?
        .file_type();
    if file_type.is_symlink() || !file_type.is_file() {
        bail!("source must be a regular file and cannot be a symlink");
    }
    let source = fs::canonicalize(&unresolved)
        .with_context(|| format!("resolving source {}", unresolved.display()))?;
    ensure_not_special(root, &source)?;
    Ok(source)
}

fn resolve_new_destination(root: &Path, input: &str) -> Result<PathBuf> {
    let destination = safe_relative(root, input)?;
    ensure_not_special(root, &destination)?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => bail!("destination already exists: {input}"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(error).with_context(|| format!("checking destination {input}")),
    }
}

fn ensure_not_special(root: &Path, path: &Path) -> Result<()> {
    if path == root.join("config/ai.toml") || path.starts_with(root.join("daily")) {
        bail!("generic file tools cannot operate on this special file");
    }
    Ok(())
}

fn copy_to_new_file(source: &Path, destination: &Path) -> Result<u64> {
    let mut input =
        fs::File::open(source).with_context(|| format!("opening source {}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("creating destination {}", destination.display()))?;
    match std::io::copy(&mut input, &mut output) {
        Ok(bytes) => Ok(bytes),
        Err(error) => {
            drop(output);
            let _ = fs::remove_file(destination);
            Err(error).with_context(|| format!("copying to {}", destination.display()))
        }
    }
}

fn move_to_new_file(source: &Path, destination: &Path) -> Result<u64> {
    let bytes = copy_to_new_file(source, destination)?;
    if let Err(error) = fs::remove_file(source) {
        let rollback = fs::remove_file(destination);
        bail!(
            "could not remove move source {}: {error}; destination rollback {}",
            source.display(),
            if rollback.is_ok() {
                "succeeded"
            } else {
                "failed"
            }
        );
    }
    Ok(bytes)
}

struct CopyFile {
    root: PathBuf,
}

impl CopyFile {
    fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
        })
    }
}

impl Tool for CopyFile {
    fn name(&self) -> &'static str {
        "copy_file"
    }

    fn description(&self) -> &'static str {
        "Copy a regular file from any absolute path (or a Nole-relative source) to a new path under the Nole root. Never overwrites and does not require approval."
    }

    fn input_schema(&self) -> Value {
        transfer_schema()
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let source = resolve_transfer_source(&self.root, required_string(input, "source")?)?;
        let destination_text = required_string(input, "destination")?;
        let destination = resolve_new_destination(&self.root, destination_text)?;
        let bytes = copy_to_new_file(&source, &destination)?;
        Ok(format!("copied {bytes} bytes to {destination_text}"))
    }
}

struct MoveFile {
    root: PathBuf,
    events: Sender<AgentEvent>,
}

impl MoveFile {
    fn new(root: &Path, events: Sender<AgentEvent>) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
        })
    }
}

impl Tool for MoveFile {
    fn name(&self) -> &'static str {
        "move_file"
    }

    fn description(&self) -> &'static str {
        "Move a regular file from any absolute path (or a Nole-relative source) to a new path under the Nole root. Never overwrites and does not require approval."
    }

    fn input_schema(&self) -> Value {
        transfer_schema()
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let source = resolve_transfer_source(&self.root, required_string(input, "source")?)?;
        let destination_text = required_string(input, "destination")?;
        let destination = resolve_new_destination(&self.root, destination_text)?;
        let bytes = move_to_new_file(&source, &destination)?;
        send_file_moved(&self.events, &self.root, &source, &destination);
        Ok(format!("moved {bytes} bytes to {destination_text}"))
    }
}

struct MoveFiles {
    root: PathBuf,
    events: Sender<AgentEvent>,
}

impl MoveFiles {
    fn new(root: &Path, events: Sender<AgentEvent>) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
        })
    }
}

impl Tool for MoveFiles {
    fn name(&self) -> &'static str {
        "move_files"
    }

    fn description(&self) -> &'static str {
        "Move multiple regular files into one existing directory under the Nole root, preserving each basename. Sources may be absolute or Nole-relative. Preflights duplicate names and destination conflicts, never overwrites, and does not require approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "sources": {
                    "type": "array", "minItems": 1, "maxItems": 200,
                    "items": { "type": "string" }
                },
                "destination_directory": {
                    "type": "string",
                    "description": "Existing directory relative to the Nole root"
                }
            },
            "required": ["sources", "destination_directory"],
            "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let source_values = input
            .get("sources")
            .and_then(Value::as_array)
            .context("field sources must be an array")?;
        if source_values.is_empty() || source_values.len() > 200 {
            bail!("sources must contain between 1 and 200 paths");
        }
        let directory_text = required_string(input, "destination_directory")?;
        let destination_directory = resolve_destination_directory(&self.root, directory_text)?;
        let mut transfers = Vec::with_capacity(source_values.len());
        let mut sources = std::collections::HashSet::new();
        let mut destinations = std::collections::HashSet::new();
        for value in source_values {
            let source_text = value.as_str().context("each source must be a string")?;
            let source = resolve_transfer_source(&self.root, source_text)?;
            if !sources.insert(source.clone()) {
                bail!("duplicate source: {source_text}");
            }
            let name = source.file_name().context("source must have a file name")?;
            let destination = destination_directory.join(name);
            ensure_not_special(&self.root, &destination)?;
            if !destinations.insert(destination.clone()) {
                bail!(
                    "multiple sources have the same basename: {}",
                    name.to_string_lossy()
                );
            }
            match fs::symlink_metadata(&destination) {
                Ok(_) => bail!("destination already exists: {}", destination.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("checking batch destination"),
            }
            transfers.push((source, destination));
        }

        let mut completed = Vec::with_capacity(transfers.len());
        for (source, destination) in &transfers {
            match move_to_new_file(source, destination) {
                Ok(bytes) => completed.push((source.clone(), destination.clone(), bytes)),
                Err(error) => {
                    let rollback_errors = rollback_moves(&completed);
                    if rollback_errors.is_empty() {
                        bail!("batch move failed and was rolled back: {error}");
                    }
                    bail!(
                        "batch move failed: {error}; rollback failures: {}",
                        rollback_errors.join("; ")
                    );
                }
            }
        }
        let moved = completed
            .iter()
            .map(|(source, destination, bytes)| {
                json!({
                    "source": source.to_string_lossy(),
                    "destination": display_path(&self.root, destination),
                    "bytes": bytes,
                })
            })
            .collect::<Vec<_>>();
        for (source, destination, _) in &completed {
            send_file_moved(&self.events, &self.root, source, destination);
        }
        serde_json::to_string_pretty(&json!({
            "destination_directory": directory_text,
            "count": moved.len(),
            "moved": moved,
        }))
        .context("encoding batch move result")
    }
}

fn resolve_destination_directory(root: &Path, input: &str) -> Result<PathBuf> {
    let relative = Path::new(input);
    if relative.is_absolute() {
        bail!("destination_directory must be relative to the Nole root");
    }
    let unresolved = root.join(relative);
    if fs::symlink_metadata(&unresolved)?.file_type().is_symlink() {
        bail!("destination_directory cannot be a symlink");
    }
    let directory = fs::canonicalize(&unresolved)
        .with_context(|| format!("resolving destination directory {input}"))?;
    if !directory.starts_with(root) {
        bail!("destination_directory escapes the Nole root");
    }
    ensure_not_special(root, &directory)?;
    if !fs::metadata(&directory)?.is_dir() {
        bail!("destination_directory must be an existing directory");
    }
    Ok(directory)
}

fn rollback_moves(completed: &[(PathBuf, PathBuf, u64)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (source, destination, _) in completed.iter().rev() {
        match move_to_new_file(destination, source) {
            Ok(_) => {}
            Err(error) => errors.push(format!(
                "{} -> {}: {error}",
                destination.display(),
                source.display()
            )),
        }
    }
    errors
}

struct RenameFile {
    root: PathBuf,
    events: Sender<AgentEvent>,
}

impl RenameFile {
    fn new(root: &Path, events: Sender<AgentEvent>) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            events,
        })
    }
}

impl Tool for RenameFile {
    fn name(&self) -> &'static str {
        "rename_file"
    }

    fn description(&self) -> &'static str {
        "Rename one regular file under the Nole root without changing its directory. The new name must be a basename, never overwrites, and does not require approval."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the Nole root" },
                "new_name": { "type": "string", "description": "New basename only" }
            },
            "required": ["path", "new_name"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let path_text = required_string(input, "path")?;
        if Path::new(path_text).is_absolute() {
            bail!("rename_file path must be relative to the Nole root");
        }
        let source = resolve_transfer_source(&self.root, path_text)?;
        if !source.starts_with(&self.root) {
            bail!("rename_file source must be under the Nole root");
        }
        let new_name = required_string(input, "new_name")?;
        let candidate = Path::new(new_name);
        if candidate.file_name().is_none()
            || candidate.components().count() != 1
            || candidate == Path::new(".")
            || candidate == Path::new("..")
        {
            bail!("new_name must be a file basename without directory components");
        }
        let destination = source
            .parent()
            .context("source must have a parent directory")?
            .join(candidate);
        ensure_not_special(&self.root, &destination)?;
        match fs::symlink_metadata(&destination) {
            Ok(_) => bail!("destination already exists: {}", destination.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("checking rename destination"),
        }
        let bytes = move_to_new_file(&source, &destination)?;
        send_file_moved(&self.events, &self.root, &source, &destination);
        Ok(format!(
            "renamed {path_text} to {} ({bytes} bytes)",
            display_path(&self.root, &destination)
        ))
    }
}

fn send_file_moved(events: &Sender<AgentEvent>, root: &Path, from: &Path, to: &Path) {
    let display = |path: &Path| {
        path.strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let _ = events.send(AgentEvent::FileMoved {
        from: display(from),
        to: display(to),
    });
}

fn transfer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "source": { "type": "string" },
            "destination": { "type": "string", "description": "Path relative to the Nole root" }
        },
        "required": ["source", "destination"], "additionalProperties": false
    })
}

struct DeleteFile {
    root: PathBuf,
    gate: ApprovalGate,
}

impl DeleteFile {
    fn new(root: &Path, gate: ApprovalGate) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            gate,
        })
    }
}

impl Tool for DeleteFile {
    fn name(&self) -> &'static str {
        "delete_file"
    }

    fn description(&self) -> &'static str {
        "Delete a regular file under the Nole root after user approval, unless permission checks are bypassed. Special files are forbidden."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object", "properties": {
                "path": { "type": "string", "description": "Path relative to the Nole root" }
            },
            "required": ["path"], "additionalProperties": false
        })
    }

    fn execute(&self, input: &Value) -> Result<String> {
        let relative = required_string(input, "path")?;
        let unresolved = safe_relative(&self.root, relative)?;
        let metadata = fs::symlink_metadata(&unresolved)
            .with_context(|| format!("checking {}", unresolved.display()))?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            bail!("delete_file only accepts regular files, not symlinks or directories");
        }
        let path = fs::canonicalize(&unresolved)?;
        ensure_not_special(&self.root, &path)?;
        let modified = metadata.modified().ok();
        let preview = if metadata.len() <= MAX_FILE_BYTES {
            fs::read_to_string(&path)
                .ok()
                .map(|content| limited_diff(&content, "", relative, "/dev/null"))
        } else {
            None
        }
        .unwrap_or_else(|| format!("Delete {relative}\nSize: {} bytes\n", metadata.len()));
        self.gate.request(ApprovalRequest {
            title: format!("Delete {relative}"),
            diff: preview,
        })?;

        let current = fs::symlink_metadata(&unresolved)
            .with_context(|| format!("rechecking {}", unresolved.display()))?;
        if current.file_type().is_symlink()
            || !current.file_type().is_file()
            || current.len() != metadata.len()
            || current.modified().ok() != modified
            || fs::canonicalize(&unresolved)? != path
        {
            bail!("file changed while awaiting approval; delete it again to review the current target");
        }
        fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
        Ok(format!("deleted {relative}"))
    }
}

struct WriteFile {
    root: PathBuf,
    private_config: PathBuf,
    daily_dir: PathBuf,
}

impl WriteFile {
    fn new(root: &Path) -> Result<Self> {
        let root = canonical_root(root)?;
        Ok(Self {
            private_config: root.join("config/ai.toml"),
            daily_dir: root.join("daily"),
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
        if path == self.private_config || path.starts_with(&self.daily_dir) {
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
    fn paginated_file_reads_require_only_the_ranges_touched_by_update() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
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
            .execute(&json!({
                "path": "data/large.md",
                "edits": [{"start_line": 450, "end_line": 450, "content": "done\n"}]
            }))
            .is_err());

        read.execute(&json!({"path": "data/large.md", "offset": 400, "limit": 50}))
            .unwrap();
        update
            .execute(&json!({
                "path": "data/large.md",
                "edits": [{"start_line": 450, "end_line": 450, "content": "done\n"}]
            }))
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
        assert_eq!(result["matches"][0]["type"], "daily");
        assert!(result["matches"][0]["date"].is_string());

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
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::write(directory.path().join("config/ai.toml"), "api_key='secret'").unwrap();
        fs::write(
            directory.path().join("daily/2026-07-27.md"),
            "private daily",
        )
        .unwrap();
        let read = ReadFile::new(directory.path(), Arc::new(ReadTracker::default())).unwrap();
        assert!(read.execute(&json!({"path": "config/ai.toml"})).is_err());
        assert!(read
            .execute(&json!({"path": "daily/2026-07-27.md"}))
            .is_err());
    }

    #[test]
    fn note_listing_includes_metadata_and_supports_sorting_and_pagination() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(directory.path().join("data/Alpha.md"), "one\ntwo\nthree\n").unwrap();
        fs::write(directory.path().join("data/Beta.mb"), "one\n").unwrap();
        fs::write(directory.path().join("data/ignored.txt"), "ignored").unwrap();
        let list = ListNotes::new(directory.path()).unwrap();

        let result: Value = serde_json::from_str(
            &list
                .execute(&json!({
                    "sort_by": "line_count", "order": "desc", "offset": 0, "limit": 1
                }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["returned"], 1);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["entries"][0]["name"], "Alpha.md");
        assert_eq!(result["entries"][0]["line_count"], 3);
        assert!(result["entries"][0].get("created_at").is_some());
        assert!(result["entries"][0]["modified_at"].is_string());
        assert_eq!(result["entries"][0]["size"], 14);

        let by_name: Value = serde_json::from_str(
            &list
                .execute(&json!({"sort_by": "name", "order": "asc"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(by_name["entries"][0]["name"], "Alpha.md");
        assert_eq!(by_name["entries"][1]["name"], "Beta.mb");
    }

    #[test]
    fn copy_and_move_accept_external_sources_but_only_new_internal_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let copy_source = outside.path().join("copy.txt");
        let move_source = outside.path().join("move.txt");
        fs::write(&copy_source, "copy me").unwrap();
        fs::write(&move_source, "move me").unwrap();
        let canonical_move_source = fs::canonicalize(&move_source).unwrap();

        let copy = CopyFile::new(directory.path()).unwrap();
        copy.execute(&json!({
            "source": copy_source,
            "destination": "data/copied.md"
        }))
        .unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("data/copied.md")).unwrap(),
            "copy me"
        );
        assert!(copy_source.exists());
        assert!(copy
            .execute(&json!({
                "source": copy_source,
                "destination": "data/copied.md"
            }))
            .is_err());
        assert!(copy
            .execute(&json!({
                "source": copy_source,
                "destination": "../escaped.md"
            }))
            .is_err());

        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let move_file = MoveFile::new(directory.path(), event_sender).unwrap();
        move_file
            .execute(&json!({
                "source": move_source,
                "destination": "data/moved.md"
            }))
            .unwrap();
        assert!(!move_source.exists());
        assert_eq!(
            fs::read_to_string(directory.path().join("data/moved.md")).unwrap(),
            "move me"
        );
        let AgentEvent::FileMoved { from, to } = event_receiver.recv().unwrap() else {
            panic!("expected file-moved event");
        };
        assert_eq!(from, canonical_move_source);
        assert_eq!(to, PathBuf::from("data/moved.md"));
    }

    #[test]
    fn batch_move_and_rename_are_explicit_non_overwriting_tools() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let destination = directory.path().join("data/collected");
        fs::create_dir(&destination).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alpha = outside.path().join("alpha.md");
        let beta = outside.path().join("beta.md");
        fs::write(&alpha, "alpha").unwrap();
        fs::write(&beta, "beta").unwrap();

        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let mover = MoveFiles::new(directory.path(), event_sender.clone()).unwrap();
        let result = mover
            .execute(&json!({
                "sources": [alpha, beta],
                "destination_directory": "data/collected"
            }))
            .unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["count"], 2);
        assert!(!alpha.exists());
        assert!(!beta.exists());
        assert_eq!(
            fs::read_to_string(destination.join("alpha.md")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs::read_to_string(destination.join("beta.md")).unwrap(),
            "beta"
        );
        let moved_events = [
            event_receiver.recv().unwrap(),
            event_receiver.recv().unwrap(),
        ];
        assert!(moved_events
            .iter()
            .all(|event| matches!(event, AgentEvent::FileMoved { .. })));

        let rename = RenameFile::new(directory.path(), event_sender).unwrap();
        rename
            .execute(&json!({
                "path": "data/collected/alpha.md",
                "new_name": "renamed.md"
            }))
            .unwrap();
        assert!(!destination.join("alpha.md").exists());
        assert_eq!(
            fs::read_to_string(destination.join("renamed.md")).unwrap(),
            "alpha"
        );
        let AgentEvent::FileMoved { from, to } = event_receiver.recv().unwrap() else {
            panic!("expected file-moved event");
        };
        assert_eq!(from, PathBuf::from("data/collected/alpha.md"));
        assert_eq!(to, PathBuf::from("data/collected/renamed.md"));
        assert!(rename
            .execute(&json!({
                "path": "data/collected/renamed.md",
                "new_name": "beta.md"
            }))
            .is_err());
    }

    #[test]
    fn delete_file_is_internal_and_waits_for_approval() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let target = directory.path().join("data/delete.md");
        fs::write(&target, "remove me\n").unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let (event_sender, event_receiver) = std::sync::mpsc::channel();
        let (decision_sender, decision_receiver) = std::sync::mpsc::channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(Mutex::new(decision_receiver)),
        };
        let delete = DeleteFile::new(directory.path(), gate).unwrap();
        assert!(delete.execute(&json!({"path": outside.path()})).is_err());
        let worker = std::thread::spawn(move || delete.execute(&json!({"path": "data/delete.md"})));
        let AgentEvent::Approval(request) = event_receiver.recv().unwrap() else {
            panic!("expected approval request");
        };
        assert_eq!(request.title, "Delete data/delete.md");
        assert!(request.diff.contains("-remove me"));
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        worker.join().unwrap().unwrap();
        assert!(!target.exists());
        assert!(outside.path().exists());
    }

    #[test]
    fn user_prompt_includes_current_local_datetime() {
        let now = Local::now();
        let prompt = prompt_with_datetime("Summarize this", now);
        assert!(prompt.starts_with("Current local date and time: "));
        assert!(prompt.contains(&now.to_rfc3339()));
        assert!(prompt.ends_with("\n\nSummarize this"));
    }

    #[test]
    fn tool_activity_uses_readable_names_and_web_fetch_wording() {
        assert_eq!(tool_start_activity("web_fetch"), "Fetching Web...");
        assert_eq!(tool_start_activity("read_file"), "Calling Read File...");
        assert_eq!(tool_display_name("update_daily"), "Update Daily");
    }

    #[test]
    fn system_prompt_requires_ask_user_for_single_turn_clarification() {
        let prompt = system_prompt(Path::new("/tmp/nole"));
        assert!(prompt.contains("This is a single-turn task"));
        assert!(prompt.contains("MUST call ask_user"));
        assert!(
            prompt.contains("Never put a question that requires an answer in the final response")
        );
    }

    #[test]
    fn system_prompt_describes_mbdown_tags_and_wikilinks() {
        let prompt = system_prompt(Path::new("/tmp/nole"));
        assert!(prompt.contains("#tag at word boundaries"));
        assert!(prompt.contains("[[wikilink]]"));
    }

    #[test]
    fn file_update_requires_read_and_write_file_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::write(directory.path().join("data/note.md"), "old\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let update = UpdateFile::new(directory.path(), bypass_gate(), reads.clone()).unwrap();
        let input = json!({
            "path": "data/note.md",
            "edits": [{"start_line": 0, "end_line": 1, "content": "new\n"}]
        });
        assert!(update.execute(&input).is_err());

        let read = ReadFile::new(directory.path(), reads).unwrap();
        read.execute(&json!({"path": "data/note.md"})).unwrap();
        update.execute(&input).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("data/note.md")).unwrap(),
            "new\n"
        );
        assert!(update
            .execute(&json!({
                "path": "data/note.md",
                "edits": [{"start_line": 0, "end_line": 1, "content": "again\n"}]
            }))
            .is_err());

        let write = WriteFile::new(directory.path()).unwrap();
        assert!(write
            .execute(&json!({"path": "data/note.md", "content": "overwrite"}))
            .is_err());
    }

    #[test]
    fn file_update_requires_only_changed_lines_and_insertion_anchors() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        let path = directory.path().join("data/large.md");
        let original = (0..20)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(&path, &original).unwrap();

        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let update = UpdateFile::new(directory.path(), bypass_gate(), reads).unwrap();
        read.execute(&json!({"path": "data/large.md", "offset": 10, "limit": 1}))
            .unwrap();
        update
            .execute(&json!({
                "path": "data/large.md",
                "edits": [{"start_line": 10, "end_line": 11, "content": "changed 10\n"}]
            }))
            .unwrap();

        fs::write(&path, &original).unwrap();
        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let update = UpdateFile::new(directory.path(), bypass_gate(), reads).unwrap();
        read.execute(&json!({"path": "data/large.md", "offset": 10, "limit": 1}))
            .unwrap();
        let error = update
            .execute(&json!({
                "path": "data/large.md",
                "edits": [{"start_line": 2, "end_line": 3, "content": "changed 2\n"}]
            }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("changed zero-based lines 2..3"));

        fs::write(&path, "first\nsecond\nthird\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let update = UpdateFile::new(directory.path(), bypass_gate(), reads).unwrap();
        read.execute(&json!({"path": "data/large.md", "offset": 1, "limit": 1}))
            .unwrap();
        let error = update
            .execute(&json!({
                "path": "data/large.md",
                "edits": [{"start_line": 1, "end_line": 1, "content": "inserted\n"}]
            }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("insertion anchor lines 0..2"));
    }

    #[test]
    fn daily_update_requires_read_but_append_does_not_require_approval() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let message = storage.append_daily("2026-07-27", "old").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let update = UpdateDaily::new(directory.path(), bypass_gate(), reads.clone()).unwrap();
        let input = json!({"date": message.id.clone(), "body": "new"});
        assert!(update.execute(&input).is_err());

        let read = ReadDaily::new(directory.path(), reads).unwrap();
        read.execute(&json!({
            "start_date": message.id,
            "end_date": message.id
        }))
        .unwrap();
        update.execute(&input).unwrap();
        assert_eq!(storage.load_messages().unwrap()[0].body, "new");
        assert!(update
            .execute(&json!({"date": message.id, "body": "again"}))
            .is_err());

        let append = AppendDaily::new(directory.path()).unwrap();
        append
            .execute(&json!({"date": "2026-07-27", "body": "added"}))
            .unwrap();
        assert_eq!(storage.load_messages().unwrap()[0].body, "new\n\nadded");
    }

    #[test]
    fn read_daily_returns_an_inclusive_range_and_marks_each_card_read() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage.append_daily("2026-07-25", "before").unwrap();
        storage.append_daily("2026-07-26", "first").unwrap();
        storage.append_daily("2026-07-28", "last").unwrap();
        storage.append_daily("2026-07-29", "after").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let read = ReadDaily::new(directory.path(), reads.clone()).unwrap();

        let result = read
            .execute(&json!({
                "start_date": "2026-07-26",
                "end_date": "2026-07-28"
            }))
            .unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["count"], 2);
        assert_eq!(result["cards"][0]["date"], "2026-07-26");
        assert_eq!(result["cards"][1]["date"], "2026-07-28");
        assert_eq!(
            reads.message_snapshot("2026-07-26").unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            reads.message_snapshot("2026-07-28").unwrap().as_deref(),
            Some("last")
        );
        assert!(reads.message_snapshot("2026-07-25").unwrap().is_none());
        assert!(read
            .execute(&json!({
                "start_date": "2026-07-29",
                "end_date": "2026-07-28"
            }))
            .is_err());
    }

    #[test]
    fn update_waits_for_diff_approval() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
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
            update.execute(&json!({
                "path": "data/note.md",
                "edits": [{"start_line": 0, "end_line": 1, "content": "new\n"}]
            }))
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
