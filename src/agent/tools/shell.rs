//! Approved non-interactive shell execution and persistent Agent PTY tools.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::agent::{
    run_noninteractive_shell, run_noninteractive_shell_untimed, terminal_input_bytes,
    terminal_input_display, validate_shell_command, AgentJobsHandle, AgentTerminalHandle,
    ApprovalGate, ApprovalKind, ApprovalRequest, CommandApproval, JobKind, StartedJob, Tool,
    ToolOutput,
};

use super::util::{backgrounded_job_result, required_string};
use crate::agent::resolve_shell_cwd;

const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
const MAX_TIMEOUT_SECONDS: u64 = 3600;
const MAX_TERMINAL_WAIT_MS: u64 = 30_000;

async fn run_terminal_operation(
    join_context: &'static str,
    operation: impl FnOnce() -> Result<Value> + Send + 'static,
) -> Result<String> {
    let output = tokio::task::spawn_blocking(operation)
        .await
        .context(join_context)??;
    Ok(render_terminal_value(&output))
}

fn render_terminal_value(value: &Value) -> String {
    let Some(fields) = value.as_object() else {
        return value.to_string();
    };
    fields
        .iter()
        .map(|(name, value)| {
            let label = name.replace('_', " ");
            match value {
                Value::String(text) => format!("{label}: {text}"),
                _ => format!("{label}: {value}"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn optional_string<'a>(input: &'a Value, key: &str) -> Result<Option<&'a str>> {
    input
        .get(key)
        .map(|value| {
            value
                .as_str()
                .with_context(|| format!("field {key} must be a string"))
        })
        .transpose()
}

fn purpose_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "pattern": "\\S",
        "description": "Provide a brief, concrete purpose for this command or terminal input."
    })
}

fn required_purpose(input: &Value) -> Result<&str> {
    let purpose = required_string(input, "purpose")?.trim();
    if purpose.is_empty() {
        anyhow::bail!("field purpose must contain a concrete purpose");
    }
    Ok(purpose)
}

fn should_submit(input: &Value, has_text: bool) -> bool {
    input
        .get("submit")
        .and_then(Value::as_bool)
        .unwrap_or(has_text)
}

fn cwd_schema() -> Value {
    json!({
        "type": "string",
        "description": "Working directory. Relative paths resolve from the Nole root; defaults to the Nole root."
    })
}

fn command_approval(title: &str, purpose: &str, label: &str, code: &str) -> ApprovalRequest {
    ApprovalRequest {
        title: title.to_string(),
        message: String::new(),
        kind: ApprovalKind::Command(CommandApproval {
            purpose: purpose.to_string(),
            label: label.to_string(),
            code: code.to_string(),
        }),
    }
}

pub struct Shell {
    root: PathBuf,
    gate: ApprovalGate,
    cancelled: Arc<AtomicBool>,
    jobs: AgentJobsHandle,
}

impl Shell {
    pub fn new(
        root: &Path,
        gate: ApprovalGate,
        cancelled: Arc<AtomicBool>,
        jobs: AgentJobsHandle,
        _input_buffer: Arc<Mutex<Vec<String>>>,
        _auto_background: bool,
        _auto_background_threshold_seconds: u64,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            gate,
            cancelled,
            jobs,
        }
    }
}

#[async_trait::async_trait]
impl Tool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn description(&self) -> &'static str {
        "Run one non-interactive command in the user's Brush shell. The command has full host access. A hard safety policy rejects broad recursive forced deletions in every permission mode. stdin is closed and common pagers and prompts are disabled. stdout and stderr are each capped at 1 MiB with explicit truncation metadata."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "purpose": purpose_schema(),
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Complete shell command to execute."
                },
                "cwd": cwd_schema(),
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_SECONDS,
                    "description": "Maximum runtime in seconds for a foreground command; defaults to 120. Explicitly backgrounded commands have no timeout: they run until they finish or are cancelled with the jobs tool."
                },
                "background": {
                    "type": "boolean",
                    "default": false,
                    "description": "Run in the background and return immediately. The result is delivered automatically when the command finishes. The command keeps running when the Agent is interrupted; stop it with the jobs tool."
                }
            },
            "required": ["purpose", "command"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        let purpose = required_purpose(input)?;
        let command = required_string(input, "command")?;
        let cwd = resolve_shell_cwd(&self.root, optional_string(input, "cwd")?)?;
        validate_shell_command(command, &cwd, &self.root)?;
        let timeout_seconds = input.get("timeout_seconds");
        let background = input
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if background && timeout_seconds.is_some() {
            bail!(
                "timeout_seconds does not apply to background commands: they run until they \
                 finish or are cancelled with the jobs tool. Drop it, or run in the foreground."
            );
        }
        let timeout_seconds = timeout_seconds
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        self.gate
            .request_host_action(command_approval(
                "Run shell command",
                purpose,
                "Cmd",
                command,
            ))
            .await?;

        let timeout = Duration::from_secs(timeout_seconds);
        if background {
            let started = self.jobs.start_background(JobKind::Shell, command)?;
            // A backgrounded command has no timeout: only `jobs cancel` or
            // clearing the session stops it, exactly as the tool docs state.
            // `timeout_seconds` bounds foreground execution (and the race's
            // foreground wait) only.
            self.spawn_shell_job_body(&started, &cwd, command, None);
            return Ok(ToolOutput::text(backgrounded_job_result(
                &started.id,
                "The command keeps running; its result will be delivered automatically as a [background job] frame. Continue with other work or end your turn—do not wait for it.",
            )?));
        }
        let cwd = cwd.clone();
        let command = command.to_string();
        let cancelled = Arc::clone(&self.cancelled);
        let result = tokio::task::spawn_blocking(move || {
            run_noninteractive_shell(&cwd, &command, timeout, &cancelled)
        })
        .await
        .context("joining shell command")??;
        Ok(shell_tool_output(&result))
    }
}

impl Shell {
    /// Spawn the job body thread for a backgrounded shell command. The body
    /// runs with the job's own cancellation flag, so it survives interruption
    /// of the current Agent run; only `jobs cancel` and session clear stop it.
    /// `timeout` is `None` for explicitly backgrounded commands, which have
    /// no deadline at all.
    fn spawn_shell_job_body(
        &self,
        started: &StartedJob,
        cwd: &Path,
        command: &str,
        timeout: Option<Duration>,
    ) {
        let jobs = self.jobs.clone();
        let id = started.id.clone();
        let cancel = Arc::clone(&started.cancel);
        let cwd = cwd.to_path_buf();
        let command = command.to_string();
        std::thread::spawn(move || {
            let outcome = match timeout {
                Some(timeout) => run_noninteractive_shell(&cwd, &command, timeout, &cancel),
                None => run_noninteractive_shell_untimed(&cwd, &command, &cancel),
            }
            .map(|value| shell_tool_output(&value))
            .map_err(|error| format!("{error:#}"));
            jobs.settle(&id, outcome);
        });
    }
}

fn shell_tool_output(value: &Value) -> ToolOutput {
    let exit_code = value.get("exit_code").and_then(Value::as_i64).unwrap_or(1) as i32;
    let stdout = value
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = value
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut content = stdout.to_string();
    if !stderr.is_empty() {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("[stderr]\n");
        content.push_str(stderr);
    }
    if let Some(warning) = value.get("warning").and_then(Value::as_str) {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(warning);
    }
    ToolOutput::process(exit_code, content)
}

pub struct TerminalOpen {
    root: PathBuf,
    gate: ApprovalGate,
    terminal: AgentTerminalHandle,
    cancelled: Arc<AtomicBool>,
}

impl TerminalOpen {
    pub fn new(
        root: &Path,
        gate: ApprovalGate,
        terminal: AgentTerminalHandle,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            gate,
            terminal,
            cancelled,
        }
    }
}

#[async_trait::async_trait]
impl Tool for TerminalOpen {
    fn name(&self) -> &'static str {
        "terminal_open"
    }

    fn description(&self) -> &'static str {
        "Start one persistent interactive terminal command. Returns its current screen and output cursor. Use terminal_read with mode=screen for interactive state or mode=output without a cursor for all output since startup. Only one Agent terminal may run at a time."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "purpose": purpose_schema(),
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Complete command to start in the PTY."
                },
                "cwd": cwd_schema()
            },
            "required": ["purpose", "command"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        let content: Result<String> = async {
            let purpose = required_purpose(input)?;
            let command = required_string(input, "command")?;
            let cwd = resolve_shell_cwd(&self.root, optional_string(input, "cwd")?)?;
            validate_shell_command(command, &cwd, &self.root)?;
            self.gate
                .request_host_action(command_approval(
                    "Open interactive terminal",
                    purpose,
                    "Cmd",
                    command,
                ))
                .await?;

            let terminal = self.terminal.clone();
            let nole_root = self.root.clone();
            let command = command.to_string();
            let session_id =
                tokio::task::spawn_blocking(move || terminal.open(&cwd, &nole_root, &command))
                    .await
                    .context("joining terminal startup")??;
            let terminal = self.terminal.clone();
            let cancelled = Arc::clone(&self.cancelled);
            let settled_id = session_id.clone();
            run_terminal_operation("joining terminal observation", move || {
                terminal.wait_until_settled(&settled_id, &cancelled)
            })
            .await
        }
        .await;
        let content = content?;
        Ok(ToolOutput::text(content))
    }
}

pub struct TerminalInput {
    gate: ApprovalGate,
    terminal: AgentTerminalHandle,
    cancelled: Arc<AtomicBool>,
}

impl TerminalInput {
    pub fn new(
        gate: ApprovalGate,
        terminal: AgentTerminalHandle,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            gate,
            terminal,
            cancelled,
        }
    }
}

#[async_trait::async_trait]
impl Tool for TerminalInput {
    fn name(&self) -> &'static str {
        "terminal_input"
    }

    fn description(&self) -> &'static str {
        "Send text or one key to the active interactive terminal, then return its updated screen and output cursor. To read everything produced by this interaction, keep the cursor from before sending input and pass it unchanged to terminal_read mode=output."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "purpose": purpose_schema(),
                "text": {
                    "type": "string",
                    "description": "Exact UTF-8 text to send. Enter is appended by default; set submit=false to type without submitting."
                },
                "submit": {
                    "type": "boolean",
                    "default": true,
                    "description": "Append Enter after text. Defaults to true for text input; omit it for named keys."
                },
                "key": {
                    "type": "string",
                    "pattern": "^(enter|tab|escape|backspace|up|down|left|right|home|end|delete|page-up|page-down|ctrl-[a-z])$",
                    "description": "One named terminal key, including Ctrl+A through Ctrl+Z as ctrl-a through ctrl-z"
                }
            },
            "required": ["session_id", "purpose"],
            "oneOf": [
                { "required": ["text"], "not": { "required": ["key"] } },
                {
                    "required": ["key"],
                    "not": { "required": ["text"] },
                    "properties": { "submit": { "const": false } }
                }
            ],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        let content: Result<String> = async {
            let session_id = required_string(input, "session_id")?;
            let purpose = required_purpose(input)?;
            let text = optional_string(input, "text")?;
            let key = optional_string(input, "key")?;
            let submit = should_submit(input, text.is_some());
            let bytes = terminal_input_bytes(text, submit, key)?;
            let display = terminal_input_display(text, submit, key)?;
            self.gate
                .request_host_action(command_approval(
                    "Send terminal input",
                    purpose,
                    "Input",
                    &display,
                ))
                .await?;

            self.terminal.write(session_id, &bytes)?;
            let terminal = self.terminal.clone();
            let cancelled = Arc::clone(&self.cancelled);
            let session_id = session_id.to_string();
            run_terminal_operation("joining terminal observation", move || {
                terminal.wait_until_settled(&session_id, &cancelled)
            })
            .await
        }
        .await;
        let content = content?;
        Ok(ToolOutput::text(content))
    }
}

/// How long a foreground terminal watch waits before converting into a
/// background TerminalWatch job (or when steering arrives).
const TERMINAL_WATCH_MAX_MS: u64 = 600_000;
/// Bytes of prior raw stream kept in front of each new chunk so patterns that
/// span poll boundaries still match.
const TERMINAL_WATCH_CARRY_BYTES: usize = 4096;
pub struct TerminalRead {
    terminal: AgentTerminalHandle,
    cancelled: Arc<AtomicBool>,
    jobs: AgentJobsHandle,
}

impl TerminalRead {
    pub fn new(
        terminal: AgentTerminalHandle,
        cancelled: Arc<AtomicBool>,
        jobs: AgentJobsHandle,
    ) -> Self {
        Self {
            terminal,
            cancelled,
            jobs,
        }
    }
}

#[async_trait::async_trait]
impl Tool for TerminalRead {
    fn name(&self) -> &'static str {
        "terminal_read"
    }

    fn description(&self) -> &'static str {
        "Read an interactive terminal without sending input. mode=screen returns its current visible state. mode=output returns all output after cursor, including text that scrolled away, plus the next cursor to pass back unchanged. Omit cursor to read from startup. wait_for may watch for matching output or exit and can continue as a background job."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "mode": {
                    "type": "string",
                    "enum": ["screen", "output"],
                    "default": "screen",
                    "description": "screen returns current interactive state; output returns all output after cursor."
                },
                "cursor": {
                    "type": "string",
                    "pattern": "^[0-9]+$",
                    "description": "Cursor returned by a prior terminal response. Pass it back unchanged in output mode; omit it to read from startup."
                },
                "wait_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_TERMINAL_WAIT_MS,
                    "description": "How long to wait for a screen or status change; defaults to 0."
                },
                "wait_for": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Regular expression matched against the raw output stream."
                        },
                        "exit": {
                            "type": "boolean",
                            "description": "Report when the process exits."
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "minimum": 100,
                            "maximum": TERMINAL_WATCH_MAX_MS,
                            "description": "Total watch duration; defaults to 60000."
                        }
                    },
                    "additionalProperties": false
                },
                "background": {
                    "type": "boolean",
                    "default": false,
                    "description": "With wait_for: register the watch as a background job and return immediately instead of foreground-waiting."
                }
            },
            "required": ["session_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        let content: Result<String> = async {
        let session_id = required_string(input, "session_id")?.to_string();
        let mode = input
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("screen");
        let wait_for = input.get("wait_for");
        let Some(wait_for) = wait_for else {
            return match mode {
                "screen" => {
                    if input.get("cursor").is_some() {
                        bail!("cursor applies only to terminal_read mode output");
                    }
                    self.read_screen(&session_id, input).await
                }
                "output" => self.read_output(&session_id, input).await,
                _ => bail!("unsupported terminal_read mode {mode}"),
            };
        };
        if mode != "screen" || input.get("cursor").is_some() {
            bail!("wait_for cannot be combined with output mode or cursor");
        }
        let pattern = wait_for
            .get("pattern")
            .and_then(Value::as_str)
            .map(|source| {
                regex::Regex::new(source)
                    .with_context(|| format!("invalid terminal watch regex {source:?}"))
            })
            .transpose()?;
        let wait_exit = wait_for
            .get("exit")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if pattern.is_none() && !wait_exit {
            anyhow::bail!("wait_for requires a pattern or exit: true");
        }
        let timeout_ms = wait_for
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(60_000)
            .clamp(100, TERMINAL_WATCH_MAX_MS);
        let background = input
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if background {
            let started = self
                .jobs
                .start_background(JobKind::TerminalWatch, &session_id)?;
            spawn_terminal_watch(
                self.jobs.clone(),
                self.terminal.clone(),
                started.id.clone(),
                session_id.clone(),
                pattern.clone(),
                Duration::from_millis(timeout_ms),
                Arc::clone(&started.cancel),
            );
            return backgrounded_job_result(
                &started.id,
                "Watching the terminal; the result will be delivered automatically on match, exit, or timeout.",
            );
        }
        // Ordinary foreground watch. The shared tool executor owns automatic
        // promotion if this call outlives the global foreground threshold.
        let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut offset = 0;
        let mut carry: Vec<u8> = Vec::new();
        loop {
            if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                anyhow::bail!("agent task cancelled");
            }
            if let Some(sample) = self.terminal.sample_watch(&session_id, offset) {
                offset = sample.offset;
                carry.extend(sample.data.iter().copied());
                if carry.len() > TERMINAL_WATCH_CARRY_BYTES {
                    let keep = carry.len() - TERMINAL_WATCH_CARRY_BYTES;
                    carry.drain(..keep);
                }
                if let Some(payload) = watch_payload(
                    &pattern,
                    &carry,
                    sample.exited,
                    sample.exit_code,
                    &sample.screen,
                ) {
                    return Ok(render_watch_payload(&payload));
                }
            }
            if std::time::Instant::now() >= deadline {
                let screen = self
                    .terminal
                    .sample_watch(&session_id, offset)
                    .map(|sample| sample.screen)
                    .unwrap_or_default();
                return Ok(render_watch_payload(&json!({
                    "matched": false,
                    "timed_out": true,
                    "screen": screen,
                })));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }.await;
        let content = content?;
        Ok(ToolOutput::text(content))
    }
}

impl TerminalRead {
    async fn read_screen(&self, session_id: &str, input: &Value) -> Result<String> {
        let wait_ms = input.get("wait_ms").and_then(Value::as_u64).unwrap_or(0);
        let terminal = self.terminal.clone();
        let cancelled = Arc::clone(&self.cancelled);
        let session_id = session_id.to_string();
        run_terminal_operation("joining terminal read", move || {
            if wait_ms == 0 {
                terminal.observation(&session_id)
            } else {
                terminal.wait_for_change(&session_id, Duration::from_millis(wait_ms), &cancelled)
            }
        })
        .await
    }

    async fn read_output(&self, session_id: &str, input: &Value) -> Result<String> {
        let cursor = input
            .get("cursor")
            .and_then(Value::as_str)
            .unwrap_or("0")
            .parse::<u64>()
            .context("terminal cursor is too large")?;
        let wait_ms = input.get("wait_ms").and_then(Value::as_u64).unwrap_or(0);
        let terminal = self.terminal.clone();
        let cancelled = Arc::clone(&self.cancelled);
        let session_id = session_id.to_string();
        run_terminal_operation("joining terminal output read", move || {
            let initial = terminal.output_since(&session_id, cursor)?;
            let has_output = initial["output"]
                .as_str()
                .is_some_and(|output| !output.is_empty());
            let running = initial["status"] == "running";
            if wait_ms == 0 || has_output || !running {
                return Ok(initial);
            }
            terminal.wait_for_change(&session_id, Duration::from_millis(wait_ms), &cancelled)?;
            terminal.output_since(&session_id, cursor)
        })
        .await
    }
}

pub struct TerminalClose {
    terminal: AgentTerminalHandle,
}

impl TerminalClose {
    pub fn new(terminal: AgentTerminalHandle) -> Self {
        Self { terminal }
    }
}

#[async_trait::async_trait]
impl Tool for TerminalClose {
    fn name(&self) -> &'static str {
        "terminal_close"
    }

    fn description(&self) -> &'static str {
        "Discard a retained Agent PTY session after its process has exited. Send an approved exit, Ctrl-C, or Ctrl-D first when it is still running."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 }
            },
            "required": ["session_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        let content: Result<String> = async {
            let session_id = required_string(input, "session_id")?;
            self.terminal.close_exited(session_id)?;
            Ok(format!("closed terminal session {session_id}"))
        }
        .await;
        let content = content?;
        Ok(ToolOutput::text(content))
    }
}

/// Evaluate one watch sample: `Some(payload)` when the pattern matched or the
/// process exited, `None` to keep watching.
fn watch_payload(
    pattern: &Option<regex::Regex>,
    carry: &[u8],
    exited: bool,
    exit_code: Option<u32>,
    screen: &str,
) -> Option<Value> {
    if let Some(pattern) = pattern {
        let text = String::from_utf8_lossy(carry);
        if let Some(found) = pattern.find(&text) {
            let line = text[found.start()..]
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_string();
            return Some(json!({
                "matched": true,
                "match_line": line,
                "screen": screen,
            }));
        }
    }
    if exited {
        return Some(json!({
            "exited": true,
            "exit_code": exit_code,
            "screen": screen,
        }));
    }
    None
}

fn render_watch_payload(payload: &Value) -> String {
    let mut lines = Vec::new();
    if payload.get("matched").and_then(Value::as_bool) == Some(true) {
        lines.push("Matched terminal output".to_string());
        if let Some(line) = payload.get("match_line").and_then(Value::as_str) {
            lines.push(format!("Match: {line}"));
        }
    } else if payload.get("timed_out").and_then(Value::as_bool) == Some(true) {
        lines.push("Terminal wait timed out".to_string());
    } else if payload.get("exited").and_then(Value::as_bool) == Some(true) {
        lines.push("Terminal process exited".to_string());
        if let Some(code) = payload.get("exit_code").and_then(Value::as_u64) {
            lines.push(format!("Exit code: {code}"));
        }
    }
    if let Some(screen) = payload.get("screen").and_then(Value::as_str) {
        lines.push(format!("Screen:\n{screen}"));
    }
    lines.join("\n")
}

/// Background watcher body: polls the raw stream until the pattern matches,
/// the process exits, the timeout elapses, or the job/session is gone. A
/// process exit always settles the watch — after it the pattern can never
/// match, so spinning to the timeout would only hide the exit code.
#[allow(clippy::too_many_arguments)]
fn spawn_terminal_watch(
    jobs: AgentJobsHandle,
    terminal: AgentTerminalHandle,
    id: String,
    session_id: String,
    pattern: Option<regex::Regex>,
    timeout: Duration,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + timeout;
        let mut offset = 0_u64;
        let mut carry: Vec<u8> = Vec::new();
        loop {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                jobs.settle(&id, Err("terminal watch cancelled".to_string()));
                return;
            }
            match terminal.sample_watch(&session_id, offset) {
                None => {
                    jobs.settle(&id, Err("terminal session is no longer active".to_string()));
                    return;
                }
                Some(sample) => {
                    offset = sample.offset;
                    carry.extend(sample.data.iter().copied());
                    if carry.len() > TERMINAL_WATCH_CARRY_BYTES {
                        let keep = carry.len() - TERMINAL_WATCH_CARRY_BYTES;
                        carry.drain(..keep);
                    }
                    if let Some(payload) = watch_payload(
                        &pattern,
                        &carry,
                        sample.exited,
                        sample.exit_code,
                        &sample.screen,
                    ) {
                        jobs.settle(&id, Ok(ToolOutput::text(render_watch_payload(&payload))));
                        return;
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                let screen = terminal
                    .sample_watch(&session_id, offset)
                    .map(|sample| sample.screen)
                    .unwrap_or_default();
                let payload = json!({
                    "matched": false,
                    "timed_out": true,
                    "screen": screen,
                });
                jobs.settle(&id, Ok(ToolOutput::text(render_watch_payload(&payload))));
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU8};

    use super::*;
    use crate::agent::{AgentEvent, AgentJobsHandle, PermissionMode};

    pub(crate) fn test_gate(root: &Path) -> ApprovalGate {
        let (events, _) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let (_decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        ApprovalGate::new(
            Arc::new(AtomicU8::new(PermissionMode::Approve.code())),
            root.to_path_buf(),
            Arc::new(AtomicBool::new(false)),
            events,
            Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        )
    }

    pub(crate) fn yolo_gate(root: &Path) -> ApprovalGate {
        let (events, _) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let (_decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        ApprovalGate::new(
            Arc::new(AtomicU8::new(PermissionMode::Yolo.code())),
            root.to_path_buf(),
            Arc::new(AtomicBool::new(false)),
            events,
            Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        )
    }

    fn test_shell(root: &Path, gate: ApprovalGate, cancelled: Arc<AtomicBool>) -> Shell {
        let (events, _) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        Shell::new(
            root,
            gate,
            cancelled,
            AgentJobsHandle::new(events),
            Arc::new(Mutex::new(Vec::new())),
            false,
            60,
        )
    }

    #[tokio::test]
    async fn hard_safety_policy_precedes_approval_for_shell_and_terminal_open() {
        let directory = tempfile::tempdir().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let shell = test_shell(
            directory.path(),
            yolo_gate(directory.path()),
            cancelled.clone(),
        );
        let error = shell
            .execute(&json!({
                "purpose": "Test the safety policy",
                "command": "rm -rf /"
            }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("environment safety policy"));

        let terminal = TerminalOpen::new(
            directory.path(),
            yolo_gate(directory.path()),
            AgentTerminalHandle::default(),
            cancelled,
        );
        let error = terminal
            .execute(&json!({
                "purpose": "Test the safety policy",
                "command": "rm --recursive --force /"
            }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("environment safety policy"));
    }

    #[test]
    fn shell_and_terminal_input_require_a_purpose() {
        let directory = tempfile::tempdir().unwrap();
        let shell = test_shell(
            directory.path(),
            test_gate(directory.path()),
            Arc::new(AtomicBool::new(false)),
        );
        let shell_validator = jsonschema::validator_for(&shell.input_schema()).unwrap();
        assert!(!shell_validator.is_valid(&json!({"command": "pwd"})));
        assert!(!shell_validator.is_valid(&json!({"purpose": "   ", "command": "pwd"})));
        assert!(
            shell_validator.is_valid(&json!({"purpose": "Show the directory", "command": "pwd"}))
        );

        let input = TerminalInput::new(
            test_gate(directory.path()),
            AgentTerminalHandle::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let input_validator = jsonschema::validator_for(&input.input_schema()).unwrap();
        assert!(!input_validator.is_valid(&json!({"session_id": "terminal-1", "text": "yes"})));
        assert!(input_validator.is_valid(&json!({
            "session_id": "terminal-1",
            "purpose": "Answer the prompt",
            "text": "yes",
            "submit": true
        })));
    }

    #[test]
    fn terminal_input_schema_keeps_text_and_keys_exclusive() {
        let directory = tempfile::tempdir().unwrap();
        let input = TerminalInput::new(
            test_gate(directory.path()),
            AgentTerminalHandle::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let validator = jsonschema::validator_for(&input.input_schema()).unwrap();
        let base = json!({"session_id": "terminal-1", "purpose": "Continue"});
        let mut text = base.clone();
        text["text"] = json!("yes");
        assert!(validator.is_valid(&text));
        let mut key = base.clone();
        key["key"] = json!("ctrl-c");
        assert!(validator.is_valid(&key));
        key["key"] = json!("ctrl-l");
        assert!(validator.is_valid(&key));
        key["key"] = json!("ctrl-z");
        assert!(validator.is_valid(&key));
        key["key"] = json!("ctrl-aa");
        assert!(!validator.is_valid(&key));
        key["key"] = json!("ctrl-c");
        key["submit"] = json!(false);
        assert!(validator.is_valid(&key));
        key["submit"] = json!(true);
        assert!(!validator.is_valid(&key));
        text["key"] = json!("enter");
        assert!(!validator.is_valid(&text));
    }

    #[test]
    fn terminal_text_submits_by_default_but_named_keys_do_not() {
        assert!(should_submit(&json!({"text": "yes"}), true));
        assert!(!should_submit(
            &json!({"text": "yes", "submit": false}),
            true
        ));
        assert!(!should_submit(&json!({"key": "ctrl-c"}), false));
    }

    #[tokio::test]
    async fn explicit_background_returns_immediately_and_delivers_on_settle() {
        let directory = tempfile::tempdir().unwrap();
        let (events, mut receiver) = tokio::sync::broadcast::channel::<AgentEvent>(32);
        let jobs = AgentJobsHandle::new(events);
        let shell = Shell::new(
            directory.path(),
            yolo_gate(directory.path()),
            Arc::new(AtomicBool::new(false)),
            jobs.clone(),
            Arc::new(Mutex::new(Vec::new())),
            false,
            60,
        );
        let started = std::time::Instant::now();
        let output = shell
            .execute(&json!({
                "purpose": "Sleep briefly",
                "command": "sleep 2",
                "background": true
            }))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(output.content.starts_with("Background job: "));
        let job = output.content.lines().next().unwrap()["Background job: ".len()..].to_string();
        // The real helper process finishes; the job settles and delivers.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if jobs.has_pending_deliveries() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "job never settled");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let deliveries = jobs.take_deliveries();
        assert!(deliveries.iter().all(|delivery| delivery.id == job));
        let _ = receiver.try_recv();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_background_rejects_timeout_seconds() {
        let directory = tempfile::tempdir().unwrap();
        let (events, _) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let jobs = AgentJobsHandle::new(events);
        let shell = Shell::new(
            directory.path(),
            yolo_gate(directory.path()),
            Arc::new(AtomicBool::new(false)),
            jobs.clone(),
            Arc::new(Mutex::new(Vec::new())),
            false,
            60,
        );
        // Backgrounded commands have no timeout, so an explicit
        // timeout_seconds is a contradiction the tool must surface instead
        // of silently ignoring.
        let error = shell
            .execute(&json!({
                "purpose": "Long task",
                "command": "sleep 150",
                "background": true,
                "timeout_seconds": 1
            }))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("timeout_seconds does not apply to background"));
        assert!(jobs.rows().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn race_returns_inline_when_command_finishes_fast() {
        let directory = tempfile::tempdir().unwrap();
        let (events, _) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let jobs = AgentJobsHandle::new(events);
        let shell = Shell::new(
            directory.path(),
            yolo_gate(directory.path()),
            Arc::new(AtomicBool::new(false)),
            jobs.clone(),
            Arc::new(Mutex::new(Vec::new())),
            true,
            60,
        );
        let output = shell
            .execute(&json!({
                "purpose": "Print",
                "command": "echo raced"
            }))
            .await
            .unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.content, "raced\n");
        // Inline consumption removes the raced job entirely: no delivery,
        // and no lingering row in the registry or the statistics panel.
        assert!(!jobs.has_pending_deliveries());
        assert!(jobs.rows().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn foreground_shell_is_not_registered_as_a_job_before_the_threshold() {
        let directory = tempfile::tempdir().unwrap();
        let (events, _) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let jobs = AgentJobsHandle::new(events);
        let shell = Shell::new(
            directory.path(),
            yolo_gate(directory.path()),
            Arc::new(AtomicBool::new(false)),
            jobs.clone(),
            Arc::new(Mutex::new(Vec::new())),
            true,
            60,
        );
        let execution = tokio::spawn(async move {
            shell
                .execute(&json!({
                    "purpose": "Remain foreground briefly",
                    "command": "sleep 1; echo done"
                }))
                .await
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(jobs.rows().is_empty());
        assert!(!jobs.has_running());
        let output = execution.await.unwrap().unwrap();
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.content, "done\n");
        assert!(jobs.rows().is_empty());
    }
}

#[cfg(test)]
mod terminal_watch_tests {
    use super::*;
    use crate::agent::test_support::event_channel;

    fn sample_payload(pattern: &str, stream: &[u8]) -> Option<Value> {
        let regex = regex::Regex::new(pattern).unwrap();
        watch_payload(&Some(regex), stream, false, None, "screen")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_output_mode_reads_the_complete_spooled_stream_incrementally() {
        let directory = tempfile::tempdir().unwrap();
        let (events, _receiver) = event_channel();
        let terminal = AgentTerminalHandle::default();
        let session = terminal
            .open_process_for_test(
                directory.path(),
                "i=0; while [ $i -lt 40000 ]; do printf 'line-%05d\\n' \"$i\"; i=$((i+1)); done",
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let observation = terminal.observation(&session).unwrap();
            if observation["status"] != "running" {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "PTY did not exit");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let read = TerminalRead::new(
            terminal,
            Arc::new(AtomicBool::new(false)),
            AgentJobsHandle::new(events),
        );
        let first = read
            .execute(&json!({
                "session_id": session,
                "mode": "output",
                "cursor": "0"
            }))
            .await
            .unwrap();
        let output = first.content.split_once("output: ").unwrap().1;
        assert!(output.len() > 256 * 1024);
        assert!(output.contains("line-00000"));
        assert!(output.contains("line-39999"));
        let cursor = first
            .content
            .lines()
            .find_map(|line| line.strip_prefix("cursor: "))
            .unwrap();

        let second = read
            .execute(&json!({
                "session_id": session,
                "mode": "output",
                "cursor": cursor
            }))
            .await
            .unwrap();
        assert!(second.content.contains("output: "));
        assert!(second.content.contains(&format!("cursor: {cursor}")));
    }

    #[test]
    fn watch_payload_matches_pattern_line() {
        let payload = sample_payload(
            "BUILD (SUCCEEDED|FAILED)",
            b"\x1b[1mBUILD SUCCEEDED in 3s\r\n",
        )
        .unwrap();
        assert_eq!(payload["matched"], json!(true));
        assert!(payload["match_line"]
            .as_str()
            .unwrap()
            .contains("BUILD SUCCEEDED"));
    }

    #[test]
    fn watch_payload_reports_exit_when_requested() {
        let regex = regex::Regex::new("never").unwrap();
        let payload = watch_payload(&Some(regex), b"other", true, Some(0), "screen").unwrap();
        assert_eq!(payload["exited"], json!(true));
        assert_eq!(payload["exit_code"], json!(0));
        assert!(sample_payload("never", b"other").is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_read_wait_for_times_out_inline_without_registering_a_job() {
        use crate::agent::AgentTerminalHandle;
        let directory = tempfile::tempdir().unwrap();
        let (events, _receiver) = event_channel();
        let jobs = AgentJobsHandle::new(events);
        let terminal = AgentTerminalHandle::default();
        let session = terminal
            .open_process_for_test(directory.path(), "echo one; sleep 30; echo two")
            .unwrap();
        let read_tool = TerminalRead::new(
            terminal.clone(),
            Arc::new(AtomicBool::new(false)),
            jobs.clone(),
        );
        let output = read_tool
            .execute(&json!({
                "session_id": session,
                "purpose": "Wait for marker",
                "wait_for": { "pattern": "MARKER_DONE", "timeout_ms": 15000 }
            }))
            .await
            .unwrap();
        assert!(output.content.contains("Terminal wait timed out"));
        assert!(jobs.rows().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_read_wait_for_inline_hit_removes_the_raced_job() {
        use crate::agent::AgentTerminalHandle;
        let directory = tempfile::tempdir().unwrap();
        let (events, _receiver) = event_channel();
        let jobs = AgentJobsHandle::new(events);
        let terminal = AgentTerminalHandle::default();
        let session = terminal
            .open_process_for_test(directory.path(), "echo MARKER_HIT; sleep 30")
            .unwrap();
        let read_tool = TerminalRead::new(
            terminal.clone(),
            Arc::new(AtomicBool::new(false)),
            jobs.clone(),
        );
        let output = read_tool
            .execute(&json!({
                "session_id": session,
                "purpose": "Wait for marker",
                "wait_for": { "pattern": "MARKER_HIT", "timeout_ms": 15000 }
            }))
            .await
            .unwrap();
        // The pattern hits inside the foreground budget, so the payload comes
        // back inline and the raced job is removed instead of lingering as a
        // settled row in the statistics panel.
        assert!(output.content.contains("Matched terminal output"));
        assert!(jobs.rows().is_empty());
        assert!(!jobs.has_pending_deliveries());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_watch_settles_with_the_exit_when_the_process_dies() {
        use crate::agent::{AgentTerminalHandle, JobStatus};
        let directory = tempfile::tempdir().unwrap();
        let (events, _receiver) = event_channel();
        let jobs = AgentJobsHandle::new(events);
        let terminal = AgentTerminalHandle::default();
        // The process exits immediately without ever printing the pattern:
        // the watch must settle with the exit payload right away instead of
        // spinning to its timeout with nothing left to match.
        let session = terminal
            .open_process_for_test(directory.path(), "echo bye")
            .unwrap();
        let read_tool = TerminalRead::new(
            terminal.clone(),
            Arc::new(AtomicBool::new(false)),
            jobs.clone(),
        );
        let output = read_tool
            .execute(&json!({
                "session_id": session,
                "purpose": "Wait for a marker that never appears",
                "wait_for": { "pattern": "NEVER_PRINTED", "timeout_ms": 60000 },
                "background": true
            }))
            .await
            .unwrap();
        let job = output.content.lines().next().unwrap()["Background job: ".len()..].to_string();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let deliveries = jobs.take_deliveries();
            if let Some(delivery) = deliveries.first() {
                assert_eq!(delivery.id, job);
                assert_eq!(delivery.status, JobStatus::Done);
                assert!(delivery.output.content.contains("Terminal process exited"));
                assert!(delivery.output.content.contains("Exit code: 0"));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "watcher never settled after the process exited"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
