use super::*;
use crate::core::agent::subagent::{SubagentProfile, SubagentRuntime};
use crate::core::agent::{AgentConfig, ToolConcurrencyLimits};
use crate::core::agent_session::TokenUsage;
use crate::core::observable::{BoxFuture, Observable};
use crate::core::provider::{
    AssistantMessage, MessagePart, Provider, ProviderEvent, ProviderRequest, StopReason,
};
use std::time::{Duration, Instant};

type Request = (
    ProviderRequest,
    tokio::sync::oneshot::Sender<AssistantMessage>,
);
struct ControlledProvider(std::sync::mpsc::Sender<Request>);
impl Provider for ControlledProvider {
    fn call<'a>(&'a self, _: ProviderRequest) -> BoxFuture<'a, AssistantMessage> {
        Box::pin(async { anyhow::bail!("Use streaming") })
    }
    fn call_streaming(
        &self,
        request: ProviderRequest,
    ) -> Observable<AssistantMessage, ProviderEvent> {
        let (answer, output) = tokio::sync::oneshot::channel();
        self.0.send((request, answer)).unwrap();
        let (_, events) = tokio::sync::broadcast::channel(16);
        Observable {
            events,
            output: Box::pin(async move { Ok(output.await?) }),
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }
}
fn response(text: &str) -> AssistantMessage {
    AssistantMessage {
        message: Message::assistant(vec![MessagePart::Text { text: text.into() }]),
        stop_reason: StopReason::End,
        token_usage: TokenUsage::default(),
        generation_duration: Duration::ZERO,
        tool_input_errors: HashMap::new(),
    }
}
fn runner(
    pool: &CollaboratorsHandle,
    id: Uuid,
    provider: Arc<dyn Provider>,
) -> Arc<SubagentRunner> {
    let config: AgentConfig =
        toml::from_str(crate::core::agent::test_support::TEST_MESSAGES_CONFIG).unwrap();
    let (events, _) = tokio::sync::broadcast::channel(32);
    let runtime = SubagentRuntime::new(
        &config,
        provider,
        Vec::new(),
        events,
        pool.cancellation(id).unwrap(),
        ToolConcurrencyLimits::new(8, 8, 4),
    );
    Arc::new(SubagentRunner::new(
        runtime,
        SubagentProfile::new("explore", "Investigate", "Finish", "Continue"),
    ))
}
fn job_registry() -> AgentJobsHandle {
    let (events, _) = tokio::sync::broadcast::channel(32);
    AgentJobsHandle::new(events)
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "collaborator timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn messages_steer_running_collaborators_and_wake_retained_conversations_after_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let pool = CollaboratorsHandle::new(directory.path());
    let jobs = job_registry();
    let (requests, arrivals) = std::sync::mpsc::channel();
    let provider = Arc::new(ControlledProvider(requests));
    let agent = pool
        .create("Research".into(), "explore".into(), "Find evidence".into())
        .unwrap();
    let id = agent.snapshot().id;
    pool.send(
        agent.clone(),
        "First task".into(),
        runner(&pool, id, provider.clone()),
        jobs.clone(),
        2,
    )
    .unwrap();
    let (first, answer) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(first.messages.last().unwrap().text(), "First task");
    pool.send(
        agent.clone(),
        "Also inspect this".into(),
        runner(&pool, id, provider.clone()),
        jobs.clone(),
        2,
    )
    .unwrap();
    assert_eq!(agent.snapshot().inbox.len(), 1);
    answer.send(response("First findings")).unwrap();
    let (second, answer) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(second.messages.last().unwrap().text(), "Also inspect this");
    assert!(second
        .messages
        .iter()
        .any(|message| message.text() == "First findings"));
    answer.send(response("Combined findings")).unwrap();
    wait_until(|| !pool.is_running() && !jobs.has_running());
    assert_eq!(
        agent.snapshot().result.as_deref(),
        Some("Combined findings")
    );
    assert_eq!(jobs.take_deliveries().len(), 1);

    let reopened = CollaboratorsHandle::new(directory.path());
    reopened.load().unwrap();
    let restored = reopened.get(id).unwrap();
    reopened
        .send(
            restored,
            "Follow-up task".into(),
            runner(&reopened, id, provider),
            jobs.clone(),
            2,
        )
        .unwrap();
    let (third, answer) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(third
        .messages
        .iter()
        .any(|message| message.text() == "Combined findings"));
    assert_eq!(third.messages.last().unwrap().text(), "Follow-up task");
    answer.send(response("Follow-up findings")).unwrap();
    wait_until(|| !reopened.is_running() && !jobs.has_running());
    assert_eq!(reopened.get(id).unwrap().snapshot().generation, 2);
}

#[test]
fn collaborators_execute_concurrently_and_one_can_be_stopped_while_the_other_continues() {
    let directory = tempfile::tempdir().unwrap();
    let pool = CollaboratorsHandle::new(directory.path());
    let jobs = job_registry();
    let (requests, arrivals) = std::sync::mpsc::channel();
    let provider = Arc::new(ControlledProvider(requests));
    let a = pool
        .create("A".into(), "explore".into(), String::new())
        .unwrap();
    let b = pool
        .create("B".into(), "review".into(), String::new())
        .unwrap();
    let id_a = a.snapshot().id;
    let id_b = b.snapshot().id;
    pool.send(
        a.clone(),
        "Task A".into(),
        runner(&pool, id_a, provider.clone()),
        jobs.clone(),
        2,
    )
    .unwrap();
    let (_, answer_a) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    pool.send(
        b.clone(),
        "Task B".into(),
        runner(&pool, id_b, provider),
        jobs.clone(),
        2,
    )
    .unwrap();
    let (_, answer_b) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(a.is_running() && b.is_running());
    pool.stop(id_a).unwrap();
    wait_until(|| !a.is_running());
    assert!(matches!(a.snapshot().status, CollaboratorStatus::Stopped));
    assert!(b.is_running());
    drop(answer_a);
    answer_b.send(response("B complete")).unwrap();
    wait_until(|| !pool.is_running() && !jobs.has_running());
    assert_eq!(b.snapshot().result.as_deref(), Some("B complete"));
}

#[test]
fn ending_a_session_prevents_late_collaborator_checkpoints_from_recreating_storage() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("session");
    let pool = CollaboratorsHandle::new(&root);
    let jobs = job_registry();
    let (requests, arrivals) = std::sync::mpsc::channel();
    let provider = Arc::new(ControlledProvider(requests));
    let agent = pool
        .create("A".into(), "explore".into(), String::new())
        .unwrap();
    let id = agent.snapshot().id;
    pool.send(
        agent,
        "Task".into(),
        runner(&pool, id, provider),
        jobs.clone(),
        1,
    )
    .unwrap();
    let (_, answer) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    pool.terminate();
    std::fs::remove_dir_all(&root).unwrap();
    let _ = answer.send(response("late"));
    wait_until(|| !pool.is_running() && !jobs.has_running());
    assert!(!root.exists());
}
