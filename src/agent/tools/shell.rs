//! Approved non-interactive shell execution and persistent Agent PTY tools.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::agent::{
    run_noninteractive_shell, terminal_input_bytes, terminal_input_display, validate_shell_command,
    AgentTerminalHandle, ApprovalGate, ApprovalKind, ApprovalRequest, CommandApproval, Tool,
};

use super::util::required_string;
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
}

impl Shell {
    pub fn new(root: &Path, gate: ApprovalGate, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            root: root.to_path_buf(),
            gate,
            cancelled,
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
                    "description": "Maximum runtime in seconds; defaults to 120."
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
        let timeout_seconds = input
            .get("timeout_seconds")
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

        let cwd = cwd.clone();
        let command = command.to_string();
        let cancelled = Arc::clone(&self.cancelled);
        let result = tokio::task::spawn_blocking(move || {
            run_noninteractive_shell(
                &cwd,
                &command,
                Duration::from_secs(timeout_seconds),
                &cancelled,
            )
        })
        .await
        .context("joining shell command")??;
        Ok(serde_json::to_string_pretty(&result)?)
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
        "Start one persistent PTY command in the user's interactive Brush shell. A hard safety policy checks the initial command in every permission mode. Use terminal_input and terminal_read to interact with it. Only one Agent PTY process may run at a time; opening a new one replaces any exited session."
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
        "Send one approved text entry or key to the active Agent PTY session, then return its updated screen."
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
}

pub struct TerminalRead {
    terminal: AgentTerminalHandle,
    cancelled: Arc<AtomicBool>,
}

impl TerminalRead {
    pub fn new(terminal: AgentTerminalHandle, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            terminal,
            cancelled,
        }
    }
}

#[async_trait::async_trait]
impl Tool for TerminalRead {
    fn name(&self) -> &'static str {
        "terminal_read"
    }

    fn description(&self) -> &'static str {
        "Read the Agent PTY screen or its retained final screen after exit, optionally waiting for it to change. This does not send input."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "wait_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_TERMINAL_WAIT_MS,
                    "description": "How long to wait for a screen or status change; defaults to 0."
                }
            },
            "required": ["session_id"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let session_id = required_string(input, "session_id")?.to_string();
        let wait_ms = input.get("wait_ms").and_then(Value::as_u64).unwrap_or(0);
        let terminal = self.terminal.clone();
        let cancelled = Arc::clone(&self.cancelled);
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU8};

    use super::*;
    use crate::agent::{AgentEvent, PermissionMode};

    fn test_gate(root: &Path) -> ApprovalGate {
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

    fn yolo_gate(root: &Path) -> ApprovalGate {
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

    #[tokio::test]
    async fn hard_safety_policy_precedes_approval_for_shell_and_terminal_open() {
        let directory = tempfile::tempdir().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let shell = Shell::new(
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
        let shell = Shell::new(
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
}
