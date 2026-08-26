//! Tools that interact with the user's TUI: open a note, notify, ask a question.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::util::{display_path, required_string};
use crate::agent::{
    canonical_root, recv_while_active, AgentEvent, AgentEventSender, AgentTerminalHandle,
    AskUserRequest, AskUserResponse, PrivateTerminalInputDecision, PrivateTerminalInputRequest,
    Tool,
};
use crate::storage::Storage;

pub struct Open {
    root: PathBuf,
    storage: Storage,
    events: AgentEventSender,
}

impl Open {
    pub fn new(root: &Path, events: AgentEventSender) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            storage: Storage::new(root)?,
            events,
        })
    }
}

#[async_trait::async_trait]
impl Tool for Open {
    fn name(&self) -> &'static str {
        "open"
    }

    fn description(&self) -> &'static str {
        "Open an existing managed .md or .mb note from daily/, data/, or archives/ in the user's TUI. The path may be absolute or relative to the Nole root."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "minLength": 1 } },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let requested = required_string(input, "path")?;
        let path = if Path::new(requested).is_absolute() {
            PathBuf::from(requested)
        } else {
            self.root.join(requested)
        };
        let path = fs::canonicalize(&path)
            .with_context(|| format!("resolving document {}", path.display()))?;
        self.storage.read_document_file(&path)?;
        self.events
            .send(AgentEvent::OpenFile(path.clone()))
            .context("requesting document open")?;
        Ok(format!("opened {}", display_path(&self.root, &path)))
    }
}

pub struct Notify {
    pub events: AgentEventSender,
}

#[async_trait::async_trait]
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

    async fn execute(&self, input: &Value) -> Result<String> {
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

pub struct Ask {
    pub events: AgentEventSender,
    pub responses: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<AskUserResponse>>>,
    pub cancelled: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Tool for Ask {
    fn name(&self) -> &'static str {
        "ask"
    }

    fn description(&self) -> &'static str {
        "Ask the user an interactive clarification question in the TUI. Optional choices may be provided, and the user can always enter a different free-text answer."
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

    async fn execute(&self, input: &Value) -> Result<String> {
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
        match recv_while_active(
            &self.responses,
            &self.cancelled,
            "waiting for user response",
        )
        .await?
        {
            AskUserResponse::Answer(answer) => Ok(answer),
            AskUserResponse::Cancelled => bail!("user cancelled the question"),
        }
    }
}

pub struct AskPrivate {
    pub events: AgentEventSender,
    pub responses:
        Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<PrivateTerminalInputDecision>>>,
    pub terminal: AgentTerminalHandle,
    pub cancelled: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Tool for AskPrivate {
    fn name(&self) -> &'static str {
        "ask_private"
    }

    fn description(&self) -> &'static str {
        "Ask the user for one masked private value and submit it with Enter directly to the active Agent PTY. Use for passwords, passphrases, and MFA codes. The value is never returned to the Agent; the result is only submitted or cancelled. Call terminal_read afterward to observe the outcome."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "purpose": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 500,
                    "pattern": "\\S",
                    "description": "Brief reason the Agent needs the user to provide private terminal input."
                },
                "prompt": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 500,
                    "pattern": "\\S",
                    "description": "User-facing description of the value requested by the terminal."
                }
            },
            "required": ["session_id", "purpose", "prompt"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let session_id = required_string(input, "session_id")?.trim();
        let purpose = required_string(input, "purpose")?.trim();
        let prompt = required_string(input, "prompt")?.trim();
        if session_id.is_empty() {
            bail!("session_id must not be empty");
        }
        if purpose.is_empty() || prompt.is_empty() {
            bail!("purpose and prompt must not be empty");
        }
        if purpose.chars().count() > 500 || prompt.chars().count() > 500 {
            bail!("purpose and prompt must not exceed 500 characters");
        }
        self.terminal.ensure_running_session(session_id)?;
        self.events
            .send(AgentEvent::PrivateTerminalInput(
                PrivateTerminalInputRequest {
                    session_id: session_id.to_string(),
                    purpose: purpose.to_string(),
                    prompt: prompt.to_string(),
                },
            ))
            .context("requesting private terminal input")?;

        match recv_while_active(
            &self.responses,
            &self.cancelled,
            "waiting for private terminal input",
        )
        .await?
        {
            PrivateTerminalInputDecision::Submit(mut private_value) => {
                self.terminal.ensure_running_session(session_id)?;
                private_value.push('\r');
                self.terminal.write(session_id, private_value.as_bytes())?;
                Ok(json!({"status": "submitted"}).to_string())
            }
            PrivateTerminalInputDecision::Cancelled => {
                Ok(json!({"status": "cancelled"}).to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    use zeroize::Zeroizing;

    fn private_input_tool(
        terminal: AgentTerminalHandle,
    ) -> (
        AskPrivate,
        tokio::sync::broadcast::Receiver<AgentEvent>,
        tokio::sync::mpsc::UnboundedSender<PrivateTerminalInputDecision>,
    ) {
        let (events, event_receiver) = tokio::sync::broadcast::channel(8);
        let (response_sender, responses) = tokio::sync::mpsc::unbounded_channel();
        (
            AskPrivate {
                events,
                responses: Arc::new(tokio::sync::Mutex::new(responses)),
                terminal,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
            event_receiver,
            response_sender,
        )
    }

    #[test]
    fn private_input_schema_has_no_value_field() {
        let (tool, _, _) = private_input_tool(AgentTerminalHandle::default());
        let schema = tool.input_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let valid = json!({
            "session_id": "terminal-1",
            "purpose": "Authenticate the command",
            "prompt": "Enter the sudo password"
        });
        assert!(validator.is_valid(&valid));
        let mut with_value = valid.clone();
        with_value["value"] = json!("must-not-be-an-argument");
        assert!(!validator.is_valid(&with_value));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn private_input_writes_directly_and_returns_only_status() {
        let directory = tempfile::tempdir().unwrap();
        let terminal = AgentTerminalHandle::default();
        let session_id = terminal
            .open_process_for_test(
                directory.path(),
                "stty -echo; printf 'Password: '; IFS= read -r value; stty echo; if [ \"$value\" = 's3cret-value' ]; then printf authenticated; else printf rejected; fi",
            )
            .unwrap();
        let prompt_deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let observation = terminal.observation(&session_id).unwrap();
            if observation["screen"]
                .as_str()
                .is_some_and(|screen| screen.contains("Password:"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < prompt_deadline,
                "password prompt did not appear"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (tool, mut events, responses) = private_input_tool(terminal.clone());
        let input = json!({
            "session_id": session_id,
            "purpose": "Authenticate the command",
            "prompt": "Enter the test password"
        });
        let execution = tokio::spawn(async move { tool.execute(&input).await });

        let request = events.recv().await.unwrap();
        assert!(matches!(
            request,
            AgentEvent::PrivateTerminalInput(PrivateTerminalInputRequest { .. })
        ));
        responses
            .send(PrivateTerminalInputDecision::Submit(Zeroizing::new(
                "s3cret-value".to_string(),
            )))
            .unwrap_or_else(|_| panic!("private-input response channel closed"));
        let result = execution.await.unwrap().unwrap();
        assert_eq!(result, r#"{"status":"submitted"}"#);
        assert!(!result.contains("s3cret-value"));
        assert!(!result.contains("screen"));
        assert!(!result.contains("length"));

        let cancelled = AtomicBool::new(false);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        // A single change can be an intermediate redraw, so keep polling
        // until the final status actually renders or the deadline passes.
        let observation = loop {
            let observation = terminal
                .wait_for_change(&session_id, Duration::from_millis(100), &cancelled)
                .unwrap();
            if observation["screen"]
                .as_str()
                .is_some_and(|screen| screen.contains("authenticated"))
                || std::time::Instant::now() >= deadline
            {
                break observation;
            }
        };
        assert!(observation["screen"]
            .as_str()
            .is_some_and(|screen| screen.contains("authenticated")));
        assert!(!observation["screen"]
            .as_str()
            .is_some_and(|screen| screen.contains("s3cret-value")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_private_input_preserves_the_pty() {
        let directory = tempfile::tempdir().unwrap();
        let terminal = AgentTerminalHandle::default();
        let session_id = terminal
            .open_process_for_test(directory.path(), "sleep 5")
            .unwrap();
        let (tool, mut events, responses) = private_input_tool(terminal.clone());
        let input = json!({
            "session_id": session_id,
            "purpose": "Authenticate the command",
            "prompt": "Enter a password"
        });
        let execution = tokio::spawn(async move { tool.execute(&input).await });
        let _ = events.recv().await.unwrap();
        responses
            .send(PrivateTerminalInputDecision::Cancelled)
            .unwrap_or_else(|_| panic!("private-input response channel closed"));
        assert_eq!(
            execution.await.unwrap().unwrap(),
            r#"{"status":"cancelled"}"#
        );
        assert!(terminal.is_running());
        terminal.terminate();
    }

    #[tokio::test]
    async fn inactive_session_is_rejected_before_opening_a_dialog() {
        let (tool, mut events, _) = private_input_tool(AgentTerminalHandle::default());
        let error = tool
            .execute(&json!({
                "session_id": "terminal-404",
                "purpose": "Authenticate",
                "prompt": "Enter a password"
            }))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("no active Agent terminal session"));
        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}
