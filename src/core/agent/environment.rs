//! Shared resources and explicit per-Agent execution policy.
use super::*;

#[derive(Clone, Default)]
pub(super) enum RunPolicy {
    #[default]
    Session,
    Collaborator {
        round_budget: u32,
    },
}
impl RunPolicy {
    pub fn final_round(&self, round: u32) -> Result<bool> {
        match self {
            Self::Session => Ok(false),
            Self::Collaborator { round_budget } => {
                anyhow::ensure!(
                    round <= *round_budget,
                    "collaborator exhausted its round budget"
                );
                Ok(round == *round_budget)
            }
        }
    }
    pub fn is_session(&self) -> bool {
        matches!(self, Self::Session)
    }
}

/// Immutable Session resources reused by every Agent. Conversation state,
/// cancellation, tools, and event visibility belong to each individual Agent.
#[derive(Clone)]
pub(crate) struct AgentEnvironment {
    pub config: AgentConfig,
    pub provider: Arc<dyn Provider>,
    pub system: Vec<SystemBlock>,
    pub events: AgentEventSender,
    pub concurrency: ToolConcurrencyLimits,
    pub attachments: AttachmentStore,
    pub client: Client,
    pub results: SessionResultStore,
}
impl AgentEnvironment {
    pub fn create(&self, cancelled: Arc<AtomicBool>, policy: RunPolicy) -> Agent {
        Agent {
            config: self.config.clone(),
            provider: self.provider.clone(),
            system: self.system.clone(),
            events: self.events.clone(),
            concurrency: self.concurrency.clone(),
            attachments: self.attachments.clone(),
            client: self.client.clone(),
            results: self.results.clone(),
            tools: HashMap::new(),
            definitions: Vec::new(),
            input_buffer: Arc::default(),
            cancelled,
            jobs: AgentJobsHandle::new(self.events.clone()).with_result_store(
                self.results.clone(),
                tool_result_token_cap_for(&self.config),
            ),
            todos: TodoHandle::default(),
            interactions: UserInteractionWaits::default(),
            policy,
        }
    }
    pub fn collaborator(&self, cancelled: Arc<AtomicBool>, instructions: &str) -> Agent {
        let mut agent = self.create(
            cancelled,
            RunPolicy::Collaborator {
                round_budget: self.config.max_subagent_rounds,
            },
        );
        agent.system.push(SystemBlock {
            text: instructions.into(),
            cache: false,
        });
        agent
    }
}
impl Agent {
    pub(super) fn emit(
        &self,
        event: AgentEvent,
    ) -> Result<(), tokio::sync::broadcast::error::SendError<AgentEvent>> {
        if self.policy.is_session()
            || matches!(
                event,
                AgentEvent::Usage(_) | AgentEvent::Retry | AgentEvent::ResponseTiming { .. }
            )
        {
            self.events.send(event).map(|_| ())
        } else {
            Ok(())
        }
    }
    #[cfg(test)]
    pub(super) async fn run_task(&self, task: &str) -> Result<String> {
        match self.run(task, &mut AgentConversation::default()).await? {
            AgentRunCompletion::Finished(text) => Ok(text),
            AgentRunCompletion::Stopped(_) => bail!("Agent stopped after tool approval was denied"),
        }
    }
}
