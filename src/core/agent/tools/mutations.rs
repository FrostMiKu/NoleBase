//! Shared file mutation capabilities for the main Agent and read-write collaborators.
use super::*;
use crate::core::agent::{ApprovalGate, RegisterBank, SnapshotStore, Tool, ToolOutput};
use crate::core::attachment_usage::AttachmentUsageHandle;
use crate::core::wiki_link_index::WikiLinkIndexHandle;
use crate::core::workspace_index::WorkspaceIndexHandle;
use anyhow::Result;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct MutationTools {
    pub root: PathBuf,
    pub workspace_index: WorkspaceIndexHandle,
    pub wiki_links: WikiLinkIndexHandle,
    pub attachment_usage: AttachmentUsageHandle,
    pub gate: ApprovalGate,
    pub lock: Arc<tokio::sync::Mutex<()>>,
    pub cancelled: Arc<AtomicBool>,
}
impl MutationTools {
    pub fn for_actor(&self, name: &str, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            gate: self.gate.for_actor(name, cancelled.clone()),
            cancelled,
            ..self.clone()
        }
    }

    pub fn wrap<T: Tool + 'static>(&self, tool: T) -> impl Tool + use<T> {
        SerializedMutation {
            tool,
            lock: self.lock.clone(),
            cancelled: self.cancelled.clone(),
        }
    }

    pub fn register(
        &self,
        registry: &mut crate::core::agent::Agent,
        reads: Arc<SnapshotStore>,
        registers: Arc<RegisterBank>,
    ) -> Result<()> {
        let root = &self.root;
        let gate = &self.gate;
        registry.register(self.wrap(RenameTag::new(
            root,
            self.workspace_index.clone(),
            gate.clone(),
        )?));
        registry.register(self.wrap(RenameWikilink::new(
            root,
            self.wiki_links.clone(),
            gate.clone(),
        )?));
        registry.register(self.wrap(Write::new(root, gate.clone())?));
        registry.register(self.wrap(Append::new(
            root,
            gate.clone(),
            reads.clone(),
            registers.clone(),
        )?));
        registry.register(self.wrap(Copy::new(root, gate.clone())?));
        registry.register(self.wrap(Export::new(root, gate.clone())?));
        registry.register(self.wrap(Mkdir::new(root, gate.clone())?));
        registry.register(self.wrap(Rmdir::new(root, gate.clone())?));
        registry.register(self.wrap(Move::new(root, gate.events.clone(), gate.clone())?));
        registry.register(self.wrap(MoveMany::new(root, gate.events.clone(), gate.clone())?));
        registry.register(self.wrap(Rename::new(root, gate.events.clone(), gate.clone())?));
        registry.register(self.wrap(Delete::new(root, gate.clone())?));
        registry.register(self.wrap(Edit::new(root, gate.clone(), reads, registers)?));
        registry.register(self.wrap(Daily::new(root, gate.clone())?));
        registry.register(self.wrap(ImportAttachment::new(root, gate.clone())?));
        registry.register(self.wrap(CheckoutAttachment::new(root, gate.clone())?));
        registry.register(self.wrap(UpdateAttachment::new(root, gate.clone())?));
        registry.register(self.wrap(DeleteAttachment::new(
            root,
            gate.clone(),
            self.attachment_usage.clone(),
        )?));
        Ok(())
    }
}

struct SerializedMutation<T> {
    tool: T,
    lock: Arc<tokio::sync::Mutex<()>>,
    cancelled: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl<T: Tool + 'static> Tool for SerializedMutation<T> {
    fn name(&self) -> &'static str {
        self.tool.name()
    }
    fn description(&self) -> &'static str {
        self.tool.description()
    }
    fn input_schema(&self) -> Value {
        self.tool.input_schema()
    }
    fn execution_policy(&self) -> crate::core::agent::ToolExecutionPolicy {
        self.tool.execution_policy()
    }
    fn allows_auto_background(&self) -> bool {
        self.tool.allows_auto_background()
    }
    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        let _guard = loop {
            anyhow::ensure!(
                !self.cancelled.load(Ordering::Relaxed),
                "agent task cancelled"
            );
            if let Ok(guard) =
                tokio::time::timeout(std::time::Duration::from_millis(100), self.lock.lock()).await
            {
                break guard;
            }
        };
        anyhow::ensure!(
            !self.cancelled.load(Ordering::Relaxed),
            "agent task cancelled"
        );
        self.tool.execute(input).await
    }
}

/// A collaborator owns its foreground command, so stopping it cancels that command.
pub(crate) struct CollaboratorShell(pub Shell);
#[async_trait::async_trait]
impl Tool for CollaboratorShell {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn description(&self) -> &'static str {
        "Run a foreground shell command under this Session's approval policy. The collaborator owns the command and stopping the collaborator cancels it."
    }
    fn input_schema(&self) -> Value {
        let mut schema = self.0.input_schema();
        schema["properties"]["background"] = serde_json::json!({"type":"boolean", "const":false, "description":"Collaborator commands run in the foreground"});
        schema
    }
    fn allows_auto_background(&self) -> bool {
        false
    }
    async fn execute(&self, input: &Value) -> Result<ToolOutput> {
        self.0.execute(input).await
    }
}
