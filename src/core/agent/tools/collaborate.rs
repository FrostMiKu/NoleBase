//! Model-facing lifecycle operations for Session-scoped collaborators.
use super::capabilities::AgentTools;
use crate::core::agent::collaboration::{
    Collaborator, CollaboratorAccess, CollaboratorsHandle, MAX_NAME_LENGTH, NAME_PATTERN,
};
use crate::core::agent::environment::AgentEnvironment;
use crate::core::agent::Agent;
use crate::core::agent::{AgentJobsHandle, Tool, ToolOutput};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub struct Collaborate {
    tools: AgentTools,
    environment: AgentEnvironment,
    agents: CollaboratorsHandle,
    jobs: AgentJobsHandle,
    maximum: usize,
}

impl Collaborate {
    pub(crate) fn new(
        tools: AgentTools,
        environment: AgentEnvironment,
        agents: CollaboratorsHandle,
        jobs: AgentJobsHandle,
        maximum: usize,
    ) -> Self {
        Self {
            tools,
            environment,
            agents,
            jobs,
            maximum,
        }
    }

    fn runner(&self, agent: &Collaborator) -> Result<Arc<Agent>> {
        let note = agent.snapshot();
        let scope = match note.access {
            CollaboratorAccess::ReadOnly => "Inspect sources using your read-only tools and report findings to your parent.",
            CollaboratorAccess::ReadWrite => "Carry out the assigned work using inspection, file mutation and foreground shell tools. Coordinate file ownership with your parent; this Session's approval policy applies to your operations.",
        };
        let instructions = format!(
            "You are {} in a Nole Agent Session. Your parent defines your role:\n{}\n{scope}\nFollow messages from your parent between rounds. Your conversation is retained across assignments. Return a complete report of the work and its outcome. Route user questions through your parent.", note.name, note.instructions);
        let cancelled = self.agents.cancellation(note.id)?;
        let mut runner = self
            .environment
            .collaborator(cancelled.clone(), &instructions);
        self.tools
            .register(&mut runner, note.access, &note.name, cancelled)?;
        Ok(Arc::new(runner))
    }

    fn id(&self, input: &Value) -> Result<Uuid> {
        self.agents.resolve(&Self::text(input, "id")?)
    }
    fn text(input: &Value, name: &str) -> Result<String> {
        Ok(input
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .with_context(|| {
                format!("invalid input for tool agents ({name}): provide a non-empty {name}")
            })?
            .to_string())
    }
    fn rows(&self, ids: &[Uuid]) -> Value {
        json!(self
            .agents
            .rows()
            .into_iter()
            .filter(|note| ids.is_empty() || ids.contains(&note.id))
            .collect::<Vec<_>>())
    }
}

#[async_trait::async_trait]
impl Tool for Collaborate {
    fn name(&self) -> &'static str {
        "agents"
    }
    fn description(&self) -> &'static str {
        "Manage persistent collaborators in this Session. Spawn requires a unique lowercase name, access (read_only or read_write), custom role instructions and an initial message. read_only grants inspection tools. read_write adds file operations and foreground shell under this Session's approval policy. The parent defines every collaborator role and handles user questions. Access is fixed at creation. Send, stop and reports accept a full UUID or exact name; wait accepts those identifiers in ids. Every complete reply is saved and delivered as its own background result, including replies followed by steering. List includes UUID, name, latest successful result, report_count, error, and received/consumed/pending message counts. Pending is the current queue length, not a cumulative count. Send returns a durable receipt with message sequence and queue size at acceptance. Reports retrieves saved replies after a sequence. Stop returns an explicit stopping/idle/stopped acknowledgement; wait observes confirmation for up to 60 seconds. Resume an idle or stopped collaborator using send; its previous result remains readable."
    }
    fn allows_auto_background(&self) -> bool {
        false
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {
            "action": {"type": "string", "enum": ["spawn", "send", "list", "wait", "stop", "reports"]},
            "name": {"type": "string", "maxLength": MAX_NAME_LENGTH, "pattern": NAME_PATTERN, "description": "Unique Session-scoped lowercase name, for example notes-scout"},
            "access": {"type": "string", "enum": ["read_only", "read_write"], "description":"Tool access granted at creation, within the Session approval policy"},
            "instructions": {"type": "string", "minLength":1, "pattern": r"\S", "description": "Custom role, responsibilities and working instructions defined by the parent"},
            "message": {"type": "string", "minLength": 1, "pattern": "\\S", "description": "Initial task for spawn, or follow-up for send"},
            "id": {"type": "string", "minLength": 1, "description": "Full UUID or exact name returned by list"},
            "ids": {"type": "array", "items": {"type": "string", "minLength": 1}},
            "seconds": {"type": "integer", "minimum": 1, "maximum": 60},
            "after": {"type": "integer", "minimum": 0},
            "limit": {"type": "integer", "minimum": 1, "maximum": 100}
        }, "required": ["action"], "additionalProperties": false,
        "allOf": [
            {"if": {"properties": {"action": {"const": "spawn"}}}, "then": {"required": ["name", "access", "instructions", "message"]}},
            {"if": {"properties": {"action": {"not": {"const":"spawn"}}}}, "then": {"not": {"anyOf":[{"required":["access"]},{"required":["instructions"]},{"required":["name"]}]}}},
            {"if": {"properties": {"action": {"const": "send"}}}, "then": {"required": ["id", "message"]}},
            {"if": {"properties": {"action": {"enum": ["stop", "reports"]}}}, "then": {"required": ["id"]}}
        ] })
    }
    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        let action = Self::text(input, "action")?;
        let output = match action.as_str() {
            "spawn" => {
                let name = Self::text(input, "name")?;
                let access: CollaboratorAccess = serde_json::from_value(input["access"].clone()).context("invalid input for tool agents (access): choose read_only or read_write")?;
                let instructions = Self::text(input, "instructions")?;
                let message = Self::text(input, "message")?;
                let agent = self.agents.create(
                    name,
                    access,
                    instructions,
                )?;
                let id = agent.snapshot().id;
                let started = self.runner(&agent).and_then(|runner| self.agents.send(agent, message, runner, self.jobs.clone(), self.maximum));
                match started {
                    Ok(receipt) => json!({"agents": self.rows(&[id]), "receipt": receipt}),
                    Err(error) => {
                        self.agents.discard_unstarted(id).context("cleaning up the unsuccessful spawn")?;
                        return Err(error);
                    }
                }
            }
            "send" => {
                let id = self.id(input)?;
                let agent = self.agents.get(id)?;
                let runner = self.runner(&agent)?;
                let receipt = self.agents.send(agent, Self::text(input, "message")?, runner, self.jobs.clone(), self.maximum)?;
                json!({"agents": self.rows(&[id]), "receipt": receipt})
            }
            "stop" => {
                let id = self.id(input)?;
                let outcome = self.agents.stop(id)?;
                json!({"agents": self.rows(&[id]), "outcome": outcome})
            }
            "reports" => {
                let id = self.id(input)?;
                let after = input["after"].as_u64().unwrap_or(0);
                let limit = input["limit"].as_u64().unwrap_or(20) as usize;
                let note = self.agents.get(id)?.snapshot();
                let reports = note.reports.iter().filter(|report| report.sequence > after).take(limit).collect::<Vec<_>>();
                json!({"id": id, "name": note.name, "reports": reports, "report_count": note.reports.len()})
            }
            "list" => json!({"agents": self.rows(&[])}),
            "wait" => {
                let identifiers = input.get("ids").map(|ids| serde_json::from_value::<Vec<String>>(ids.clone())).transpose()?.unwrap_or_default();
                let ids = identifiers.iter().map(|id| self.agents.resolve(id)).collect::<Result<Vec<_>>>()?;
                let seconds = input["seconds"].as_u64().unwrap_or(30).clamp(1, 60);
                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
                while self.agents.any_running(&ids) {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    anyhow::ensure!(!self.tools.mutations.cancelled.load(std::sync::atomic::Ordering::Relaxed), "agent task cancelled");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                json!({"agents": self.rows(&ids), "timed_out": self.agents.any_running(&ids)})
            }
            _ => anyhow::bail!("invalid input for tool agents (action): choose spawn, send, list, wait, stop or reports"),
        };
        Ok(ToolOutput::text(serde_json::to_string(&output)?))
    }
}
