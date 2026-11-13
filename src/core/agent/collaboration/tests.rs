use super::*;
use crate::core::agent::AgentConfig;
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
fn runner(pool: &CollaboratorsHandle, id: Uuid, provider: Arc<dyn Provider>) -> Arc<Agent> {
    let config: AgentConfig =
        toml::from_str(crate::core::agent::test_support::TEST_MESSAGES_CONFIG).unwrap();
    let (events, _) = tokio::sync::broadcast::channel(32);
    let environment = crate::core::agent::test_support::environment(
        &config,
        provider,
        events,
        pool.directory.parent().unwrap(),
    );
    Arc::new(environment.collaborator(pool.cancellation(id).unwrap(), "Investigate"))
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
        .create(
            "research".into(),
            CollaboratorAccess::ReadOnly,
            "Find evidence".into(),
        )
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
        agent.snapshot().summary().result.as_deref(),
        Some("Combined findings")
    );
    let deliveries = jobs.take_deliveries();
    assert_eq!(
        deliveries
            .iter()
            .map(|delivery| delivery.output.content.as_str())
            .collect::<Vec<_>>(),
        ["First findings", "Combined findings"]
    );
    assert_eq!(agent.snapshot().reports.len(), 2);

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
        .create("a".into(), CollaboratorAccess::ReadOnly, String::new())
        .unwrap();
    let b = pool
        .create("b".into(), CollaboratorAccess::ReadOnly, String::new())
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
    assert_eq!(b.snapshot().summary().result.as_deref(), Some("B complete"));
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
        .create("a".into(), CollaboratorAccess::ReadOnly, String::new())
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

#[test]
fn every_completed_report_is_delivered_before_the_next_steered_reply_and_survives_restart() {
    let directory = tempfile::tempdir().unwrap();
    let pool = CollaboratorsHandle::new(directory.path());
    let jobs = job_registry();
    jobs.set_max_running(1);
    let (requests, arrivals) = std::sync::mpsc::channel();
    let provider = Arc::new(ControlledProvider(requests));
    let agent = pool
        .create(
            "fc16-scout".into(),
            CollaboratorAccess::ReadOnly,
            String::new(),
        )
        .unwrap();
    let id = agent.snapshot().id;
    let first = pool
        .send(
            agent.clone(),
            "Main investigation".into(),
            runner(&pool, id, provider.clone()),
            jobs.clone(),
            1,
        )
        .unwrap();
    assert_eq!(first.sequence, 1);
    assert_eq!(first.pending_messages_at_acceptance, 1);
    let (_, answer) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = pool
        .send(
            agent.clone(),
            "Check edition".into(),
            runner(&pool, id, provider.clone()),
            jobs.clone(),
            1,
        )
        .unwrap();
    assert_eq!((second.sequence, second.generation), (2, 1));
    let main_report = "1. Structure\n2. Crates\n3. Toolchain\n4. Findings";
    answer.send(response(main_report)).unwrap();
    let (request, answer) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.messages.last().unwrap().text(), "Check edition");
    let delivered = jobs.take_deliveries();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].output.content, main_report);
    assert!(agent.is_running());
    assert_eq!(
        agent.snapshot().summary().result.as_deref(),
        Some(main_report)
    );
    pool.send(
        agent.clone(),
        "Check PICO-8".into(),
        runner(&pool, id, provider),
        jobs.clone(),
        1,
    )
    .unwrap();
    answer.send(response("Edition follow-up")).unwrap();
    let (_, answer) = arrivals.recv_timeout(Duration::from_secs(5)).unwrap();
    let delivered = jobs.take_deliveries();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].output.content, "Edition follow-up");
    answer.send(response("PICO-8 follow-up")).unwrap();
    wait_until(|| !pool.is_running() && !jobs.has_running());
    let delivered = jobs.take_deliveries();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].output.content, "PICO-8 follow-up");
    let restored = CollaboratorsHandle::new(directory.path());
    restored.load().unwrap();
    let saved = restored.get(id).unwrap().snapshot();
    assert_eq!(
        saved
            .reports
            .iter()
            .map(|report| (
                report.sequence,
                report.through_message,
                report.text.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (1, 1, main_report),
            (2, 2, "Edition follow-up"),
            (3, 3, "PICO-8 follow-up"),
        ]
    );
    assert_eq!(
        (
            saved.received_messages,
            saved.consumed_messages,
            saved.inbox.len()
        ),
        (3, 3, 0)
    );
}

struct ToolHarness {
    _directory: tempfile::TempDir,
    agents: CollaboratorsHandle,
    jobs: AgentJobsHandle,
    tool: crate::core::agent::RegisteredTool,
    requests: std::sync::mpsc::Receiver<Request>,
    events: tokio::sync::broadcast::Receiver<crate::core::agent::AgentEvent>,
    decisions: tokio::sync::mpsc::UnboundedSender<crate::core::agent::ApprovalDecision>,
}
impl ToolHarness {
    fn new(maximum: usize) -> Self {
        Self::with_mode(maximum, crate::core::agent::PermissionMode::Yolo)
    }
    fn with_mode(maximum: usize, mode: crate::core::agent::PermissionMode) -> Self {
        use crate::core::agent::tools::{AgentTools, Collaborate};
        use crate::core::agent::{RegisteredTool, Tool};
        let directory = tempfile::tempdir().unwrap();
        let storage = crate::core::storage::Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let session_directory = directory.path().join("session");
        let agents = CollaboratorsHandle::new(&session_directory);
        agents.load().unwrap();
        let jobs = job_registry();
        let (requests, arrivals) = std::sync::mpsc::channel();
        let config: AgentConfig =
            toml::from_str(crate::core::agent::test_support::TEST_MESSAGES_CONFIG).unwrap();
        let (events, event_receiver) = tokio::sync::broadcast::channel(128);
        let (decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = crate::core::agent::ApprovalGate::new(
            Arc::new(std::sync::atomic::AtomicU8::new(mode.code())),
            directory.path().canonicalize().unwrap(),
            Arc::default(),
            events.clone(),
            Arc::new(tokio::sync::Mutex::new(decision_receiver)),
            Default::default(),
        );
        let environment = crate::core::agent::test_support::environment(
            &config,
            Arc::new(ControlledProvider(requests)),
            events,
            &session_directory,
        );
        let tools = AgentTools {
            mutations: crate::core::agent::tools::mutations::MutationTools {
                root: directory.path().to_path_buf(),
                workspace_index: Default::default(),
                wiki_links: Default::default(),
                attachment_usage: Default::default(),
                gate,
                lock: Arc::default(),
                cancelled: Arc::default(),
            },
            jobs: jobs.clone(),
            root: directory.path().to_path_buf(),
            session_directory,
            workspace_index: Default::default(),
            wiki_links: Default::default(),
            client: reqwest::Client::new(),
            tavily_api_key: String::new(),
            skills: Vec::new(),
        };
        let tool = Collaborate::new(tools, environment, agents.clone(), jobs.clone(), maximum);
        let schema = tool.input_schema();
        Self {
            _directory: directory,
            agents,
            jobs,
            tool: RegisteredTool::new(tool, &schema),
            requests: arrivals,
            events: event_receiver,
            decisions,
        }
    }
    fn execute(&self, mut input: serde_json::Value) -> serde_json::Value {
        if input["action"] == "spawn" {
            if input.get("access").is_none() {
                input["access"] = serde_json::json!("read_only");
            }
            if input.get("instructions").is_none() {
                input["instructions"] = serde_json::json!("Investigate the assigned task");
            }
        }
        let output = crate::core::agent::test_support::test_runtime()
            .block_on(self.tool.execute(&input))
            .unwrap();
        serde_json::from_str(&output.content).unwrap()
    }
    fn error(&self, input: serde_json::Value) -> String {
        format!(
            "{:#}",
            crate::core::agent::test_support::test_runtime()
                .block_on(self.tool.execute(&input))
                .unwrap_err()
        )
    }
}
impl Drop for ToolHarness {
    fn drop(&mut self) {
        self.agents.terminate();
        wait_until(|| !self.agents.is_running());
    }
}

#[test]
fn tool_schema_and_runtime_agree_on_spawn_required_fields_and_name_validation() {
    use serde_json::json;
    let harness = ToolHarness::new(1);
    for input in [
        json!({"action": "spawn", "name": "scout", "instructions": "Investigate"}),
        json!({"action": "spawn", "message": "Investigate"}),
    ] {
        let error = harness.error(input);
        assert!(error.contains("invalid input for tool agents"));
        assert!(error.contains("required"));
        assert!(harness.agents.rows().is_empty());
    }
    for name in ["Big-Name", "with space", "a--b", "-scout", "scout-", ""] {
        assert!(harness
            .error(json!({"action":"spawn", "name":name,"access":"read_only","instructions":"Investigate","message":"Task"}))
            .contains("invalid input for tool agents"));
    }
    assert!(harness
        .error(json!({"action":"spawn","name":"scout","access":"read_only","instructions":"Investigate","message":" \t\n"}))
        .contains("invalid input for tool agents"));
    let accepted = harness.execute(json!({"action":"spawn","name":"notes-scout","message":"Task"}));
    let id = accepted["agents"][0]["id"].as_str().unwrap();
    assert_eq!(
        harness.agents.resolve("notes-scout").unwrap().to_string(),
        id
    );
    assert_eq!(accepted["receipt"]["sequence"], 1);
    let (request, answer) = harness
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let tool_names = request
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"read") && tool_names.contains(&"grep"));
    assert!(!tool_names.contains(&"shell") && !tool_names.contains(&"write"));
    assert!(harness
        .error(json!({"action":"spawn","name":"notes-scout","access":"read_only","instructions":"Investigate","message":"Duplicate"}))
        .contains("already exists"));
    assert_eq!(harness.agents.rows().len(), 1);
    assert!(harness
        .error(json!({"action":"spawn","name":"other-scout","access":"read_only","instructions":"Investigate","message":"Over capacity"}))
        .contains("slots are busy"));
    assert_eq!(
        harness.agents.rows().len(),
        1,
        "a rejected spawn leaves the existing catalog intact"
    );
    answer.send(response("Done")).unwrap();
    harness.execute(json!({"action":"wait", "ids":["notes-scout"], "seconds":1}));
}

#[test]
fn tool_receipts_names_reports_and_stop_acknowledgements_have_observable_contracts() {
    use serde_json::json;
    let harness = ToolHarness::new(1);
    let spawned = harness.execute(json!({"action":"spawn", "name":"scout", "message":"Initial"}));
    let id = spawned["agents"][0]["id"].as_str().unwrap().to_string();
    let (_, answer) = harness
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let one = harness.execute(json!({"action":"send", "id":"scout", "message":"First follow-up"}));
    let two = harness.execute(json!({"action":"send", "id":id, "message":"Second follow-up"}));
    assert_eq!(
        (
            one["receipt"]["sequence"].as_u64(),
            two["receipt"]["sequence"].as_u64()
        ),
        (Some(2), Some(3))
    );
    assert_eq!(two["receipt"]["pending_messages_at_acceptance"], 2);
    let list = harness.execute(json!({"action":"list"}));
    assert_eq!(list["agents"][0]["id"], id);
    assert_eq!(list["agents"][0]["name"], "scout");
    assert_eq!(list["agents"][0]["pending_messages"], 2);
    assert_eq!(list["agents"][0]["received_messages"], 3);
    assert_eq!(list["agents"][0]["consumed_messages"], 1);
    answer.send(response("Main report")).unwrap();
    let (_, answer) = harness
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let list = harness.execute(json!({"action":"list"}));
    assert_eq!(list["agents"][0]["pending_messages"], 0);
    assert_eq!(list["agents"][0]["consumed_messages"], 3);
    answer.send(response("Follow-up report")).unwrap();
    let waited = harness.execute(json!({"action":"wait", "ids":[id,"scout"],"seconds":1}));
    assert_eq!(waited["timed_out"], false);
    assert_eq!(
        harness.execute(json!({"action":"stop","id":"scout"}))["outcome"],
        "already_idle"
    );
    let resumed =
        harness.execute(json!({"action":"send","id":"scout","message":"Next assignment"}));
    assert_eq!(resumed["agents"][0]["result"], "Follow-up report");
    assert_eq!(resumed["receipt"]["generation"], 2);
    let (_, answer) = harness
        .requests
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let stopped = harness.execute(json!({"action":"stop","id":"scout"}));
    assert_eq!(stopped["outcome"], "stopping");
    assert!(matches!(
        stopped["agents"][0]["status"].as_str(),
        Some("stopping" | "stopped")
    ));
    let waited = harness.execute(json!({"action":"wait","ids":["scout"],"seconds":1}));
    assert_eq!(waited["agents"][0]["status"], "stopped");
    assert_eq!(waited["agents"][0]["error"], "agent task cancelled");
    assert_eq!(waited["agents"][0]["result"], "Follow-up report");
    assert_eq!(
        harness.execute(json!({"action":"stop","id":id}))["outcome"],
        "already_stopped"
    );
    drop(answer);
    let reports = harness.execute(json!({"action":"reports","id":"scout", "after":1,"limit":1}));
    assert_eq!(reports["reports"].as_array().unwrap().len(), 1);
    assert_eq!(reports["reports"][0]["text"], "Follow-up report");
    assert!(harness
        .error(json!({"action":"send","id":"unknown","message":"Task"}))
        .contains("full UUID or exact collaborator name"));
    wait_until(|| !harness.jobs.has_running());
}

fn tool_response(name: &str, input: serde_json::Value) -> AssistantMessage {
    let mut answer = response("");
    answer.message = Message::assistant(vec![MessagePart::ToolUse(
        crate::core::provider::ToolCall {
            id: "operation".into(),
            name: name.into(),
            input,
        },
    )]);
    answer.stop_reason = StopReason::ToolUse;
    answer
}
fn tool_result(request: &ProviderRequest) -> &crate::core::provider::ToolResult {
    request
        .messages
        .iter()
        .rev()
        .flat_map(|message| &message.parts)
        .find_map(|part| match part {
            MessagePart::ToolResult(result) => Some(result),
            _ => None,
        })
        .unwrap()
}
impl ToolHarness {
    fn request(&self) -> Request {
        self.requests.recv_timeout(Duration::from_secs(5)).unwrap()
    }
    fn event(
        &mut self,
        predicate: impl Fn(&crate::core::agent::AgentEvent) -> bool,
    ) -> crate::core::agent::AgentEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(event) = self.events.try_recv() {
                if predicate(&event) {
                    return event;
                }
            }
            assert!(Instant::now() < deadline, "event timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn access_is_required_fixed_and_enforced_by_actual_tool_execution() {
    use serde_json::json;
    let harness = ToolHarness::new(1);
    for input in [
        json!({"action":"spawn","name":"reader","instructions":"Inspect","message":"Task"}),
        json!({"action":"spawn","name":"reader","access":"read_only","message":"Task"}),
        json!({"action":"spawn","name":"reader","access":"read_only","instructions":"  ","message":"Task"}),
        json!({"action":"spawn","name":"reader","access":"admin","instructions":"Inspect","message":"Task"}),
        json!({"action":"spawn","name":"reader","profile":"explore","instructions":"Inspect","message":"Task"}),
    ] {
        assert!(harness
            .error(input)
            .contains("invalid input for tool agents"));
        assert!(harness.agents.rows().is_empty());
    }
    let accepted = harness.execute(json!({"action":"spawn","name":"reader","access":"read_only","instructions":"Attempt to write if asked","message":"Task"}));
    assert_eq!(accepted["agents"][0]["access"], "read_only");
    assert!(harness
        .error(json!({"action":"send","id":"reader","access":"read_write","message":"Upgrade"}))
        .contains("invalid input for tool agents"));
    let (request, answer) = harness.request();
    assert!(request
        .system
        .iter()
        .any(|block| block.text.contains("Attempt to write if asked")));
    answer
        .send(tool_response(
            "write",
            json!({"path":"data/forbidden.md","content":"changed"}),
        ))
        .unwrap();
    let (request, answer) = harness.request();
    assert!(tool_result(&request).is_error);
    assert!(!harness._directory.path().join("data/forbidden.md").exists());
    answer.send(response("Inspection complete")).unwrap();
    wait_until(|| !harness.agents.is_running());
    let reopened = CollaboratorsHandle::new(&harness._directory.path().join("session"));
    reopened.load().unwrap();
    assert_eq!(reopened.rows()[0].access, CollaboratorAccess::ReadOnly);
}

#[test]
fn read_write_executes_file_tools_and_retains_access_and_custom_role() {
    use serde_json::json;
    let harness = ToolHarness::with_mode(1, crate::core::agent::PermissionMode::Auto);
    harness.execute(json!({"action":"spawn","name":"editor","access":"read_write","instructions":"Own the release checklist","message":"Write checklist"}));
    let (request, answer) = harness.request();
    for name in ["read", "write", "edit", "append", "shell"] {
        assert!(request.tools.iter().any(|tool| tool.name == name));
    }
    assert!(request
        .system
        .iter()
        .any(|block| block.text.contains("Own the release checklist")));
    answer
        .send(tool_response(
            "write",
            json!({"path":"data/checklist.md","content":"Checklist"}),
        ))
        .unwrap();
    let (request, answer) = harness.request();
    assert!(
        !tool_result(&request).is_error,
        "{}",
        tool_result(&request).content
    );
    assert_eq!(
        std::fs::read_to_string(harness._directory.path().join("data/checklist.md")).unwrap(),
        "Checklist"
    );
    answer
        .send(tool_response(
            "shell",
            json!({"command":"printf done", "purpose":"Check output", "background":true}),
        ))
        .unwrap();
    let (request, answer) = harness.request();
    assert!(tool_result(&request).is_error);
    assert_eq!(
        harness
            .jobs
            .rows()
            .iter()
            .filter(|job| job.kind == crate::core::agent::JobKind::Shell)
            .count(),
        0
    );
    answer.send(response("Checklist complete")).unwrap();
    wait_until(|| !harness.agents.is_running());
    let reopened = CollaboratorsHandle::new(&harness._directory.path().join("session"));
    reopened.load().unwrap();
    let note = reopened.get(reopened.rows()[0].id).unwrap().snapshot();
    assert_eq!(note.access, CollaboratorAccess::ReadWrite);
    assert_eq!(note.instructions, "Own the release checklist");
}

#[test]
fn writers_share_approval_policy_and_stopping_releases_the_next_writer() {
    use crate::core::agent::{AgentEvent, ApprovalDecision, PermissionMode};
    use serde_json::json;
    let mut harness = ToolHarness::with_mode(2, PermissionMode::Approve);
    harness.execute(json!({"action":"spawn","name":"first","access":"read_write","instructions":"Write first","message":"Task"}));
    let (_, answer) = harness.request();
    answer
        .send(tool_response(
            "write",
            json!({"path":"data/first.md","content":"First"}),
        ))
        .unwrap();
    let event = harness.event(|event| matches!(event, AgentEvent::Approval(_)));
    assert!(matches!(event, AgentEvent::Approval(request) if request.title.starts_with("first:")));
    harness.execute(json!({"action":"spawn","name":"second","access":"read_write","instructions":"Write second","message":"Task"}));
    let (_, answer) = harness.request();
    answer
        .send(tool_response(
            "write",
            json!({"path":"data/second.md","content":"Second"}),
        ))
        .unwrap();
    assert!(!harness._directory.path().join("data/first.md").exists());
    assert!(!harness._directory.path().join("data/second.md").exists());
    harness.execute(json!({"action":"stop","id":"first"}));
    harness.event(|event| matches!(event, AgentEvent::ApprovalResolved));
    let event = harness.event(|event| matches!(event, AgentEvent::Approval(_)));
    assert!(matches!(event, AgentEvent::Approval(request) if request.title.starts_with("second:")));
    harness.decisions.send(ApprovalDecision::Deny).unwrap();
    wait_until(|| !harness.agents.is_running());
    let note = harness
        .agents
        .get(harness.agents.resolve("second").unwrap())
        .unwrap()
        .snapshot();
    assert!(note.messages.iter().flat_map(|message| &message.parts).any(|part| matches!(part, MessagePart::ToolResult(result) if result.is_error && result.content.contains("approval_denied"))));
    assert!(!harness._directory.path().join("data/second.md").exists());
    harness.execute(json!({"action":"send","id":"second","message":"Retry the write"}));
    let (_, answer) = harness.request();
    answer
        .send(tool_response(
            "write",
            json!({"path":"data/second.md","content":"Approved"}),
        ))
        .unwrap();
    harness.event(|event| matches!(event, AgentEvent::Approval(_)));
    harness.decisions.send(ApprovalDecision::Approve).unwrap();
    let (request, answer) = harness.request();
    assert!(!tool_result(&request).is_error);
    assert_eq!(
        std::fs::read_to_string(harness._directory.path().join("data/second.md")).unwrap(),
        "Approved"
    );
    answer.send(response("Done")).unwrap();
    wait_until(|| !harness.agents.is_running());
    assert!(!harness._directory.path().join("data/first.md").exists());
}

#[test]
fn auto_mode_requires_approval_for_collaborator_shell_and_external_writes() {
    use crate::core::agent::{AgentEvent, ApprovalDecision, PermissionMode};
    use serde_json::json;
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("external.md");
    let mut harness = ToolHarness::with_mode(1, PermissionMode::Auto);
    harness.execute(json!({"action":"spawn","name":"editor","access":"read_write","instructions":"Handle external operations","message":"Task"}));
    let (_, answer) = harness.request();
    answer
        .send(tool_response(
            "write",
            json!({"path":path,"content":"External"}),
        ))
        .unwrap();
    harness.event(|event| matches!(event, AgentEvent::Approval(_)));
    assert!(!path.exists());
    harness.decisions.send(ApprovalDecision::Deny).unwrap();
    wait_until(|| !harness.agents.is_running());
    assert!(!path.exists());
    harness.execute(json!({"action":"send","id":"editor","message":"Run a command"}));
    let (_, answer) = harness.request();
    answer
        .send(tool_response(
            "shell",
            json!({"command":"printf done", "purpose":"Check output"}),
        ))
        .unwrap();
    harness.event(|event| matches!(event, AgentEvent::Approval(_)));
    harness.decisions.send(ApprovalDecision::Approve).unwrap();
    let (request, answer) = harness.request();
    assert!(
        !tool_result(&request).is_error,
        "{}",
        tool_result(&request).content
    );
    assert!(tool_result(&request).content.contains("done"));
    answer.send(response("Done")).unwrap();
    wait_until(|| !harness.agents.is_running());
}
