//! Approved non-interactive shell execution and persistent Agent PTY tools.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::agent::{
    run_noninteractive_shell, run_noninteractive_shell_untimed, terminal_input_bytes,
    terminal_input_display, validate_shell_command,
    AgentJobsHandle, AgentTerminalHandle, ApprovalGate, ApprovalKind, ApprovalRequest,
    CommandApproval, JobKind, StartedJob, Tool, ToolOutput,
};

use super::util::{backgrounded_job_result, required_string};
use crate::agent::resolve_shell_cwd;

const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
const MAX_TIMEOUT_SECONDS: u64 = 3600;
const MAX_TERMINAL_WAIT_MS: u64 = 30_000;

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
    input_buffer: Arc<Mutex<Vec<String>>>,
    auto_background: bool,
    auto_background_threshold_seconds: u64,
}

impl Shell {
    pub fn new(
        root: &Path,
        gate: ApprovalGate,
        cancelled: Arc<AtomicBool>,
        jobs: AgentJobsHandle,
        input_buffer: Arc<Mutex<Vec<String>>>,
        auto_background: bool,
        auto_background_threshold_seconds: u64,
    ) -> Self {
        Self {
            root: root.to_path_buf(),
            gate,
            cancelled,
            jobs,
            input_buffer,
            auto_background,
            auto_background_threshold_seconds,
        }
    }

    fn has_buffered_prompts(&self) -> bool {
        self.input_buffer
            .lock()
            .map(|buffer| !buffer.is_empty())
            .unwrap_or(false)
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

    async fn execute(&self, input: &Value) -> Result<String> {
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
        // Branch 1: explicit background — register and return immediately.
        // Branch 2: auto-background race — register suppressed, foreground-wait
        // bounded by the threshold, convert on threshold or steering input.
        // Branch 3: plain foreground execution (config off or at capacity).
        let raced = !background
            && self.auto_background
            && !self.jobs.at_capacity()
            && self.auto_background_threshold_seconds > 0;
        if background || raced {
            let mut started = if background {
                self.jobs.start_background(JobKind::Shell, command)?
            } else {
                self.jobs.start_raced(JobKind::Shell, command)?
            };
            // A backgrounded command has no timeout: only `jobs cancel` or
            // clearing the session stops it, exactly as the tool docs state.
            // `timeout_seconds` bounds foreground execution (and the race's
            // foreground wait) only.
            let job_timeout = if background { None } else { Some(timeout) };
            self.spawn_shell_job_body(&started, &cwd, command, job_timeout);
            if background {
                return backgrounded_job_result(
                    &started.id,
                    "The command keeps running; its result will be delivered automatically as a [background job] frame. Continue with other work or end your turn—do not wait for it.",
                );
            }
            let wait = Duration::from_secs(self.auto_background_threshold_seconds)
                .min(timeout.saturating_sub(Duration::from_secs(1)));
            let deadline = std::time::Instant::now() + wait;
            loop {
                if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    self.jobs.cancel(&started.id);
                    self.jobs.remove(&started.id);
                    anyhow::bail!("agent task cancelled");
                }
                if self.has_buffered_prompts() {
                    self.jobs.resume(&started.id);
                    return backgrounded_job_result(
                        &started.id,
                        "Backgrounded early to handle an incoming message; the command keeps running and its result will be delivered automatically.",
                    );
                }
                if let Ok(outcome) = started.completion.try_recv() {
                    // Consumed inline: the raced job was never backgrounded,
                    // so it must not linger in the registry or the UI.
                    self.jobs.remove(&started.id);
                    return match outcome {
                        Ok(text) => Ok(text),
                        Err(error) => Err(anyhow::anyhow!(error)),
                    };
                }
                if std::time::Instant::now() >= deadline {
                    self.jobs.resume(&started.id);
                    return backgrounded_job_result(
                        &started.id,
                        "Still running after the foreground wait; backgrounded and its result will be delivered automatically.",
                    );
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        let cwd = cwd.clone();
        let command = command.to_string();
        let cancelled = Arc::clone(&self.cancelled);
        let result = tokio::task::spawn_blocking(move || {
            run_noninteractive_shell(&cwd, &command, timeout, &cancelled)
        })
        .await
        .context("joining shell command")??;
        Ok(serde_json::to_string_pretty(&result)?)
    }

    async fn execute_output(&self, input: &Value) -> Result<ToolOutput> {
        let text = self.execute(input).await?;
        ToolOutput::structured_json(&text, &["stdout", "stderr"])
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
            .map(|value| serde_json::to_string_pretty(&value).unwrap_or_default())
            .map_err(|error| format!("{error:#}"));
            jobs.settle(&id, outcome);
        });
    }
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

    async fn execute(&self, input: &Value) -> Result<String> {
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
        let observation = tokio::task::spawn_blocking(move || {
            terminal.wait_until_settled(&settled_id, &cancelled)
        })
        .await
        .context("joining terminal observation")??;
        Ok(serde_json::to_string_pretty(&observation)?)
    }

    async fn execute_output(&self, input: &Value) -> Result<ToolOutput> {
        let text = self.execute(input).await?;
        ToolOutput::structured_json(&text, &["screen"])
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

    async fn execute(&self, input: &Value) -> Result<String> {
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
        let observation = tokio::task::spawn_blocking(move || {
            terminal.wait_until_settled(&session_id, &cancelled)
        })
        .await
        .context("joining terminal observation")??;
        Ok(serde_json::to_string_pretty(&observation)?)
    }

    async fn execute_output(&self, input: &Value) -> Result<ToolOutput> {
        let text = self.execute(input).await?;
        ToolOutput::structured_json(&text, &["screen"])
    }
}

/// How long a foreground terminal watch waits before converting into a
/// background TerminalWatch job (or when steering arrives).
const TERMINAL_WATCH_FOREGROUND_BUDGET_MS: u64 = 10_000;
const TERMINAL_WATCH_MAX_MS: u64 = 600_000;
/// Bytes of prior raw stream kept in front of each new chunk so patterns that
/// span poll boundaries still match.
const TERMINAL_WATCH_CARRY_BYTES: usize = 4096;
pub struct TerminalRead {
    terminal: AgentTerminalHandle,
    cancelled: Arc<AtomicBool>,
    jobs: AgentJobsHandle,
    input_buffer: Arc<Mutex<Vec<String>>>,
}

impl TerminalRead {
    pub fn new(
        terminal: AgentTerminalHandle,
        cancelled: Arc<AtomicBool>,
        jobs: AgentJobsHandle,
        input_buffer: Arc<Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            terminal,
            cancelled,
            jobs,
            input_buffer,
        }
    }

    fn has_buffered_prompts(&self) -> bool {
        self.input_buffer
            .lock()
            .map(|buffer| !buffer.is_empty())
            .unwrap_or(false)
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

    async fn execute(&self, input: &Value) -> Result<String> {
        let session_id = required_string(input, "session_id")?.to_string();
        let mode = input.get("mode").and_then(Value::as_str).unwrap_or("screen");
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
        // An explicit background watch is a background job from the start;
        // only the foreground race needs the suppressed, hidden start.
        let started = if background {
            self.jobs.start_background(JobKind::TerminalWatch, &session_id)?
        } else {
            self.jobs.start_raced(JobKind::TerminalWatch, &session_id)?
        };
        spawn_terminal_watch(
            self.jobs.clone(),
            self.terminal.clone(),
            started.id.clone(),
            session_id.clone(),
            pattern.clone(),
            Duration::from_millis(timeout_ms),
            Arc::clone(&started.cancel),
        );
        // Explicit background: deliver via the job only.
        if background {
            return backgrounded_job_result(
                &started.id,
                "Watching the terminal; the result will be delivered automatically on match, exit, or timeout.",
            );
        }
        // Foreground race: inline hit, else convert at the budget (or steering).
        let budget = Duration::from_millis(TERMINAL_WATCH_FOREGROUND_BUDGET_MS)
            .min(Duration::from_millis(timeout_ms));
        let deadline = std::time::Instant::now() + budget;
        let mut offset = 0;
        let mut carry: Vec<u8> = Vec::new();
        loop {
            if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                self.jobs.cancel(&started.id);
                self.jobs.remove(&started.id);
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
                    self.jobs.remove(&started.id);
                    return Ok(serde_json::to_string_pretty(&payload)?);
                }
            }
            if self.has_buffered_prompts() || std::time::Instant::now() >= deadline {
                self.jobs.resume(&started.id);
                return backgrounded_job_result(
                    &started.id,
                    "Watch converted to a background job; the result will be delivered automatically on match, exit, or timeout. Continue with other work or end your turn—do not wait for it.",
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn execute_output(&self, input: &Value) -> Result<ToolOutput> {
        let text = self.execute(input).await?;
        ToolOutput::structured_json(&text, &["screen", "output"])
    }
}

impl TerminalRead {
    async fn read_screen(&self, session_id: &str, input: &Value) -> Result<String> {
        let wait_ms = input.get("wait_ms").and_then(Value::as_u64).unwrap_or(0);
        let terminal = self.terminal.clone();
        let cancelled = Arc::clone(&self.cancelled);
        let session_id = session_id.to_string();
        let observation = tokio::task::spawn_blocking(move || {
            if wait_ms == 0 {
                terminal.observation(&session_id)
            } else {
                terminal.wait_for_change(&session_id, Duration::from_millis(wait_ms), &cancelled)
            }
        })
        .await
        .context("joining terminal read")??;
        Ok(serde_json::to_string_pretty(&observation)?)
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
        let observation = tokio::task::spawn_blocking(move || {
            let initial = terminal.output_since(&session_id, cursor)?;
            let has_output = initial["output"]
                .as_str()
                .is_some_and(|output| !output.is_empty());
            let running = initial["status"] == "running";
            if wait_ms == 0 || has_output || !running {
                return Ok(initial);
            }
            terminal.wait_for_change(
                &session_id,
                Duration::from_millis(wait_ms),
                &cancelled,
            )?;
            terminal.output_since(&session_id, cursor)
        })
        .await
        .context("joining terminal output read")??;
        Ok(serde_json::to_string_pretty(&observation)?)
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

    async fn execute(&self, input: &Value) -> Result<String> {
        let session_id = required_string(input, "session_id")?;
        self.terminal.close_exited(session_id)?;
        Ok(format!("closed terminal session {session_id}"))
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
                        match serde_json::to_string_pretty(&payload) {
                            Ok(text) => jobs.settle(&id, Ok(text)),
                            Err(error) => jobs.settle(&id, Err(format!("{error:#}"))),
                        }
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
                match serde_json::to_string_pretty(&payload) {
                    Ok(text) => jobs.settle(&id, Ok(text)),
                    Err(error) => jobs.settle(&id, Err(format!("{error:#}"))),
                }
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
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["backgrounded"], json!(true));
        let job = value["job"].as_str().unwrap().to_string();
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
        assert!(error.to_string().contains("timeout_seconds does not apply to background"));
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
        let value: Value = serde_json::from_str(&output).unwrap();
        assert!(value.get("backgrounded").is_none());
        // Inline consumption removes the raced job entirely: no delivery,
        // and no lingering row in the registry or the statistics panel.
        assert!(!jobs.has_pending_deliveries());
        assert!(jobs.rows().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn race_backgrounds_when_threshold_expires() {
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
            1,
        );
        let started = std::time::Instant::now();
        let output = shell
            .execute(&json!({
                "purpose": "Sleep long",
                "command": "sleep 30",
                "timeout_seconds": 60
            }))
            .await
            .unwrap();
        assert!(started.elapsed() >= Duration::from_secs(1));
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["backgrounded"], json!(true));
        let job = value["job"].as_str().unwrap();
        // Converted to background: the job is now listed and running.
        assert!(jobs.rows().iter().any(|row| row.id == job));
        assert!(jobs.has_running());
        assert!(jobs.cancel(job));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn race_backgrounds_early_on_buffered_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let (events, _) = tokio::sync::broadcast::channel::<AgentEvent>(8);
        let jobs = AgentJobsHandle::new(events);
        let input_buffer = Arc::new(Mutex::new(Vec::new()));
        let shell = Shell::new(
            directory.path(),
            yolo_gate(directory.path()),
            Arc::new(AtomicBool::new(false)),
            jobs.clone(),
            input_buffer.clone(),
            true,
            60,
        );
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            input_buffer
                .lock()
                .unwrap()
                .push("new user message".to_string());
        });
        let started = std::time::Instant::now();
        let output = shell
            .execute(&json!({
                "purpose": "Sleep",
                "command": "sleep 30",
                "timeout_seconds": 60
            }))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["backgrounded"], json!(true));
        assert!(value["note"].as_str().unwrap().contains("incoming message"));
        let job = value["job"].as_str().unwrap();
        assert!(jobs.cancel(job));
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
            Arc::new(Mutex::new(Vec::new())),
        );
        let first: Value = serde_json::from_str(
            &read
                .execute(&json!({
                    "session_id": session,
                    "mode": "output",
                    "cursor": "0"
                }))
                .await
                .unwrap(),
        )
        .unwrap();
        let output = first["output"].as_str().unwrap();
        assert!(output.len() > 256 * 1024);
        assert!(output.contains("line-00000"));
        assert!(output.contains("line-39999"));
        let cursor = first["cursor"].as_str().unwrap();

        let second: Value = serde_json::from_str(
            &read
                .execute(&json!({
                    "session_id": session,
                    "mode": "output",
                    "cursor": cursor
                }))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(second["output"], "");
        assert_eq!(second["cursor"], cursor);
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
    async fn terminal_read_wait_for_converts_to_background_job() {
        use crate::agent::{AgentTerminalHandle, JobStatus};
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
            Arc::new(Mutex::new(Vec::new())),
        );
        let output = read_tool
            .execute(&json!({
                "session_id": session,
                "purpose": "Wait for marker",
                "wait_for": { "pattern": "MARKER_DONE", "timeout_ms": 15000 }
            }))
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["backgrounded"], json!(true));
        let job = value["job"].as_str().unwrap().to_string();
        // The foreground budget elapsed, so the job keeps watching. Cancel it
        // and confirm the watcher settles it as failed/cancelled without
        // delivering a frame.
        assert!(jobs.cancel(&job));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let rows = jobs.rows();
            if rows
                .iter()
                .any(|row| row.id == job && row.status != JobStatus::Running)
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "watcher never settled after cancel"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!jobs.has_pending_deliveries());
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
            Arc::new(Mutex::new(Vec::new())),
        );
        let output = read_tool
            .execute(&json!({
                "session_id": session,
                "purpose": "Wait for marker",
                "wait_for": { "pattern": "MARKER_HIT", "timeout_ms": 15000 }
            }))
            .await
            .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        // The pattern hits inside the foreground budget, so the payload comes
        // back inline and the raced job is removed instead of lingering as a
        // settled row in the statistics panel.
        assert!(value.get("backgrounded").is_none());
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
            Arc::new(Mutex::new(Vec::new())),
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
        let value: Value = serde_json::from_str(&output).unwrap();
        let job = value["job"].as_str().unwrap().to_string();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let deliveries = jobs.take_deliveries();
            if let Some(delivery) = deliveries.first() {
                assert_eq!(delivery.id, job);
                assert_eq!(delivery.status, JobStatus::Done);
                let payload: Value = serde_json::from_str(&delivery.result).unwrap();
                assert_eq!(payload["exited"], json!(true));
                assert_eq!(payload["exit_code"], json!(0));
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
