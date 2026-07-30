//! Tools that interact with the user's TUI: open a note, notify, ask a question.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::util::required_string;
use crate::agent::{
    AgentEvent, AgentEventSender, AskUserKind, AskUserRequest, AskUserResponse, Tool,
    canonical_root, recv_while_active,
};
use crate::storage::Storage;

pub struct OpenFile {
    root: PathBuf,
    storage: Storage,
    events: AgentEventSender,
}

impl OpenFile {
    pub fn new(root: &Path, events: AgentEventSender) -> Result<Self> {
        Ok(Self {
            root: canonical_root(root)?,
            storage: Storage::new(root)?,
            events,
        })
    }
}

#[async_trait::async_trait]
impl Tool for OpenFile {
    fn name(&self) -> &'static str {
        "open_file"
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
        Ok(format!("opened {}", path.display()))
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

pub struct AskUser {
    pub events: AgentEventSender,
    pub responses: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<AskUserResponse>>>,
    pub cancelled: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Tool for AskUser {
    fn name(&self) -> &'static str {
        "ask_user"
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
                kind: AskUserKind::Tool,
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
