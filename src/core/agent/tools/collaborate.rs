//! Model-facing lifecycle operations for Session-scoped collaborators.
use super::subagent_tools::SubagentTools;
use crate::core::agent::collaboration::CollaboratorsHandle;
use crate::core::agent::subagent::{SubagentProfile, SubagentRunner, SubagentRuntime};
use crate::core::agent::{AgentJobsHandle, Tool, ToolOutput};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub struct Collaborate {
    tools: SubagentTools,
    runtime: SubagentRuntime,
    agents: CollaboratorsHandle,
    jobs: AgentJobsHandle,
    maximum: usize,
}

impl Collaborate {
    pub(crate) fn new(
        tools: SubagentTools,
        runtime: SubagentRuntime,
        agents: CollaboratorsHandle,
        jobs: AgentJobsHandle,
        maximum: usize,
    ) -> Self {
        Self {
            tools,
            runtime,
            agents,
            jobs,
            maximum,
        }
    }

    fn runner(&self, id: Uuid) -> Result<Arc<SubagentRunner>> {
        let note = self.agents.get(id)?.snapshot();
        let instructions = match note.profile.as_str() {
            "review" => "Independently evaluate the supplied artifact against the task's goals and constraints. Gather evidence, prioritize actionable findings, and explain their impact.",
            _ => "Investigate the supplied task, gather evidence with the available tools, and report concrete findings, relevant paths and uncertainties.",
        };
        let profile = SubagentProfile::new("collaborator", format!(
            "You are {} in a Nole Agent Session. {instructions} {}\nKeep your scope focused and use inspection tools to gather evidence. Messages from your parent arrive between rounds. Your conversation is retained across assignments. Return a complete report to your parent when the assignment is complete. Your parent handles user questions and file changes.", note.name, note.instructions),
            "Synthesize the gathered evidence into a complete report now.", "Continue the assignment and return a complete report.");
        let runtime = self
            .runtime
            .clone()
            .with_cancellation(self.agents.cancellation(id)?);
        let mut runner = SubagentRunner::new(runtime, profile);
        self.tools.register(&mut runner)?;
        Ok(Arc::new(runner))
    }

    fn id(input: &Value) -> Result<Uuid> {
        input
            .get("id")
            .and_then(Value::as_str)
            .context("Choose a collaborator id")?
            .parse()
            .context("Use a valid collaborator id")
    }
    fn text(input: &Value, name: &str) -> Result<String> {
        Ok(input
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .with_context(|| format!("Provide {name}"))?
            .to_string())
    }
    fn rows(&self, ids: &[Uuid]) -> Value {
        json!(self.agents.rows().into_iter().filter(|note| ids.is_empty() || ids.contains(&note.id)).map(|note| json!({
            "id": note.id, "name": note.name, "profile": note.profile, "status": note.status,
            "generation": note.generation, "result": note.result, "pending_messages": note.inbox.len(),
        })).collect::<Vec<_>>())
    }
}

#[async_trait::async_trait]
impl Tool for Collaborate {
    fn name(&self) -> &'static str {
        "agents"
    }
    fn description(&self) -> &'static str {
        "Create and interact with persistent collaborators inside this Session. Spawn returns immediately. Send steers a running collaborator between rounds or wakes an idle collaborator with its retained conversation. Explore and review profiles inspect sources; the parent handles changes and user interaction. List shows results and stable ids. Wait observes selected collaborators for up to 60 seconds. Completed reports also arrive as background results and wake the parent when idle. Stop interrupts one collaborator. Session histories survive restarts; interrupted collaborators can be resumed with send."
    }
    fn allows_auto_background(&self) -> bool {
        false
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {
            "action": {"type": "string", "enum": ["spawn", "send", "list", "wait", "stop"]},
            "name": {"type": "string", "minLength": 1, "maxLength": 100},
            "profile": {"type": "string", "enum": ["explore", "review"]},
            "instructions": {"type": "string"},
            "message": {"type": "string", "minLength": 1},
            "id": {"type": "string"},
            "ids": {"type": "array", "items": {"type": "string"}},
            "seconds": {"type": "integer", "minimum": 1, "maximum": 60}
        }, "required": ["action"], "additionalProperties": false })
    }
    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        let action = Self::text(input, "action")?;
        let output = match action.as_str() {
            "spawn" => {
                let name = Self::text(input, "name")?;
                let profile = input["profile"].as_str().unwrap_or("explore");
                anyhow::ensure!(
                    ["explore", "review"].contains(&profile),
                    "Choose explore or review"
                );
                let message = Self::text(input, "message")?;
                let agent = self.agents.create(
                    name,
                    profile.into(),
                    input["instructions"].as_str().unwrap_or("").to_string(),
                )?;
                let id = agent.snapshot().id;
                self.agents.send(
                    agent,
                    message,
                    self.runner(id)?,
                    self.jobs.clone(),
                    self.maximum,
                )?;
                self.rows(&[id])
            }
            "send" => {
                let id = Self::id(input)?;
                self.agents.send(
                    self.agents.get(id)?,
                    Self::text(input, "message")?,
                    self.runner(id)?,
                    self.jobs.clone(),
                    self.maximum,
                )?;
                self.rows(&[id])
            }
            "stop" => {
                let id = Self::id(input)?;
                self.agents.stop(id)?;
                self.rows(&[id])
            }
            "list" => self.rows(&[]),
            "wait" => {
                let ids = input
                    .get("ids")
                    .map(|ids| serde_json::from_value::<Vec<Uuid>>(ids.clone()))
                    .transpose()?
                    .unwrap_or_default();
                for id in &ids {
                    self.agents.get(*id)?;
                }
                let seconds = input["seconds"].as_u64().unwrap_or(30).clamp(1, 60);
                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
                while self.agents.any_running(&ids) {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    self.runtime.ensure_active()?;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                self.rows(&ids)
            }
            _ => anyhow::bail!("Choose spawn, send, list, wait or stop"),
        };
        Ok(ToolOutput::text(serde_json::to_string(&output)?))
    }
}
