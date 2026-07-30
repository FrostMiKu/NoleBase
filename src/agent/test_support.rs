//! Shared test helpers used by both the agent-runtime tests and the tool tests.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;

use super::{
    AgentEvent, AgentEventSender, AgentRunCompletion, ApprovalGate, ToolBatchExecution,
    AGENT_STREAM_BUFFER,
};
use crate::agent::{Agent, AgentConversation};
use crate::provider::Message;

pub(crate) trait TestFutureResultExt<T> {
    fn unwrap(self) -> T;
    fn unwrap_err(self) -> anyhow::Error;
    fn returns_err(self) -> bool;
}

impl<F, T> TestFutureResultExt<T> for F
where
    F: std::future::Future<Output = Result<T>>,
{
    fn unwrap(self) -> T {
        test_runtime().block_on(self).unwrap()
    }

    fn unwrap_err(self) -> anyhow::Error {
        match test_runtime().block_on(self) {
            Ok(_) => panic!("expected future to return an error"),
            Err(error) => error,
        }
    }

    fn returns_err(self) -> bool {
        test_runtime().block_on(self).is_err()
    }
}

pub(crate) fn test_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

pub(crate) fn event_channel() -> (AgentEventSender, tokio::sync::broadcast::Receiver<AgentEvent>) {
    tokio::sync::broadcast::channel(AGENT_STREAM_BUFFER)
}

pub(crate) fn drain_events(
    receiver: &mut tokio::sync::broadcast::Receiver<AgentEvent>,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    events
}

pub(crate) fn run_agent(
    agent: &Agent,
    prompt: &str,
    conversation: &mut AgentConversation,
) -> Result<String> {
    test_runtime()
        .block_on(agent.run(prompt, conversation))
        .map(|completion| match completion {
            AgentRunCompletion::Finished(output) => output,
            AgentRunCompletion::Stopped(_) => String::new(),
        })
}

pub(crate) fn completed_tool_results(execution: ToolBatchExecution) -> Vec<Message> {
    match execution {
        ToolBatchExecution::Completed(results) => results,
        ToolBatchExecution::Denied(_) => panic!("expected completed tool batch"),
    }
}

pub(crate) const TEST_MESSAGES_CONFIG: &str = "api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'https://api.anthropic.com'\n";

pub(crate) fn bypass_gate() -> ApprovalGate {
    let (event_sender, _event_receiver) = event_channel();
    let (_decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
    ApprovalGate {
        bypass: Arc::new(AtomicBool::new(true)),
        cancelled: Arc::new(AtomicBool::new(false)),
        events: event_sender,
        decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
    }
}
