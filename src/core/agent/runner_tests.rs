fn test_directory() -> &'static tempfile::TempDir {
    static DIRECTORY: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIRECTORY.get_or_init(|| tempfile::tempdir().unwrap())
}

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context};
use parking_lot::Mutex;
use serde_json::{json, Value};

use super::*;
use crate::core::observable::{BoxFuture, Observable};
use crate::core::provider::{
    transient_provider_error, ApiFormat, AssistantMessage, MessagePart, ProviderEvent,
    DEFAULT_STREAM_BUFFER,
};

struct ScriptedProvider {
    responses: Mutex<VecDeque<AssistantMessage>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl Provider for ScriptedProvider {
    fn call<'a>(&'a self, _request: ProviderRequest) -> BoxFuture<'a, AssistantMessage> {
        Box::pin(async { bail!("subagents must use streaming provider calls") })
    }

    fn call_streaming(
        &self,
        request: ProviderRequest,
    ) -> Observable<AssistantMessage, ProviderEvent> {
        self.requests.lock().push(request);
        let result = self
            .responses
            .lock()
            .pop_front()
            .context("missing scripted response");
        let (_events, receiver) = tokio::sync::broadcast::channel(DEFAULT_STREAM_BUFFER);
        Observable {
            output: Box::pin(async move { result }),
            events: receiver,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }
}

struct FlakyProvider {
    responses: Mutex<VecDeque<std::result::Result<AssistantMessage, String>>>,
    requests: AtomicUsize,
}

impl Provider for FlakyProvider {
    fn call<'a>(&'a self, _request: ProviderRequest) -> BoxFuture<'a, AssistantMessage> {
        Box::pin(async { bail!("subagents must use streaming provider calls") })
    }

    fn call_streaming(
        &self,
        _request: ProviderRequest,
    ) -> Observable<AssistantMessage, ProviderEvent> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let result = match self.responses.lock().pop_front() {
            Some(Ok(response)) => Ok(response),
            Some(Err(error)) => Err(transient_provider_error(error)),
            None => Err(anyhow::anyhow!("missing flaky response")),
        };
        let (_events, receiver) = tokio::sync::broadcast::channel(DEFAULT_STREAM_BUFFER);
        Observable {
            output: Box::pin(async move { result }),
            events: receiver,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }
}

fn response(parts: Vec<MessagePart>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        message: Message::assistant(parts),
        stop_reason,
        token_usage: TokenUsage::default(),
        generation_duration: Duration::ZERO,
        tool_input_errors: HashMap::new(),
    }
}

fn runtime(
    max_subagent_rounds: u32,
    provider: Arc<dyn Provider>,
    cancelled: Arc<AtomicBool>,
) -> Agent {
    let config = AgentConfig {
        api_format: ApiFormat::Messages,
        api_key: "test".to_string(),
        tavily_api_key: String::new(),
        model: "test-model".to_string(),
        base_url: "https://example.com".to_string(),
        max_tokens: 1_024,
        effort: None,
        context_window_tokens: 8_192,
        max_subagent_rounds,
        max_concurrent_local_reads: 8,
        max_concurrent_network_tools: 8,
        max_concurrent_subagents: 4,
        auto_background: false,
        auto_background_threshold_seconds: 60,
        max_background_jobs: 15,
        supports_images: false,
    };
    let (events, _receiver) = tokio::sync::broadcast::channel(16);
    let directory = test_directory();
    let mut environment = test_support::environment(&config, provider, events, directory.path());
    environment.system.push(SystemBlock {
        text: "shared parent context".into(),
        cache: true,
    });
    environment.collaborator(cancelled, "Review the supplied implementation.")
}

fn image_support_runtime(
    max_subagent_rounds: u32,
    provider: Arc<dyn Provider>,
    cancelled: Arc<AtomicBool>,
) -> Agent {
    let config = AgentConfig {
        api_format: ApiFormat::Messages,
        api_key: "test".to_string(),
        tavily_api_key: String::new(),
        model: "test-model".to_string(),
        base_url: "https://example.com".to_string(),
        max_tokens: 1_024,
        effort: None,
        context_window_tokens: 8_192,
        max_subagent_rounds,
        max_concurrent_local_reads: 8,
        max_concurrent_network_tools: 8,
        max_concurrent_subagents: 4,
        auto_background: false,
        auto_background_threshold_seconds: 60,
        max_background_jobs: 15,
        supports_images: true,
    };
    let (events, _receiver) = tokio::sync::broadcast::channel(16);
    let directory = test_directory();
    let mut environment = test_support::environment(&config, provider, events, directory.path());
    environment.system.push(SystemBlock {
        text: "shared parent context".into(),
        cache: true,
    });
    environment.collaborator(cancelled, "Review the supplied implementation.")
}

struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Echo a value"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::LocalRead
    }

    async fn execute(&self, input: &Value) -> Result<crate::core::agent::ToolOutput> {
        let content: Result<String> =
            async { Ok(input["value"].as_str().unwrap().to_string()) }.await;
        Ok(crate::core::agent::ToolOutput::text(content?))
    }
}

#[test]
fn runner_retries_a_transient_provider_failure() {
    let provider = Arc::new(FlakyProvider {
        responses: Mutex::new(VecDeque::from([
            Err("temporary network failure".to_string()),
            Ok(response(
                vec![MessagePart::Text {
                    text: "Recovered report.".to_string(),
                }],
                StopReason::End,
            )),
        ])),
        requests: AtomicUsize::new(0),
    });
    let runner = runtime(3, provider.clone(), Arc::new(AtomicBool::new(false)));

    assert_eq!(
        crate::core::agent::test_support::test_runtime()
            .block_on(runner.run_task("Review"))
            .unwrap(),
        "Recovered report."
    );
    assert_eq!(provider.requests.load(Ordering::SeqCst), 2);
}

#[test]
fn runner_stops_after_bounded_transient_retries() {
    let provider = Arc::new(FlakyProvider {
        responses: Mutex::new(VecDeque::from([
            Err("temporary failure one".to_string()),
            Err("temporary failure two".to_string()),
            Err("temporary failure three".to_string()),
        ])),
        requests: AtomicUsize::new(0),
    });
    let runner = runtime(3, provider.clone(), Arc::new(AtomicBool::new(false)));

    let error = crate::core::agent::test_support::test_runtime()
        .block_on(runner.run_task("Review"))
        .unwrap_err();
    assert!(error.to_string().contains("temporary failure three"));
    assert_eq!(
        provider.requests.load(Ordering::SeqCst),
        MAX_PROVIDER_REQUEST_ATTEMPTS
    );
}

#[test]
fn runner_continues_after_length_and_finalizes_without_tools() {
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::from([
            response(
                vec![MessagePart::Text {
                    text: "Partial review".to_string(),
                }],
                StopReason::Length,
            ),
            response(
                vec![MessagePart::Text {
                    text: "Complete review".to_string(),
                }],
                StopReason::End,
            ),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let mut runner = runtime(2, provider.clone(), Arc::new(AtomicBool::new(false)));
    runner.register(EchoTool);

    let output = crate::core::agent::test_support::test_runtime()
        .block_on(runner.run_task("Review"))
        .unwrap();
    assert_eq!(output, "Complete review");

    let requests = provider.requests.lock();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].tools.is_empty());
    assert!(requests[1].tools.is_empty());
    assert_eq!(requests[1].messages[1].text(), "Partial review");
    assert!(requests[1].messages[2..]
        .iter()
        .all(|message| message.role == crate::core::provider::MessageRole::User));
}

#[test]
fn runner_rejects_a_length_limited_final_response() {
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::from([response(
            vec![MessagePart::Text {
                text: "Still incomplete".to_string(),
            }],
            StopReason::Length,
        )])),
        requests: Mutex::new(Vec::new()),
    });
    let runner = runtime(1, provider, Arc::new(AtomicBool::new(false)));

    let error = crate::core::agent::test_support::test_runtime()
        .block_on(runner.run_task("Review"))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "collaborator returned no complete response within its round budget"
    );
}

#[test]
fn agent_uses_custom_instructions_and_registered_tools() {
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::from([
            response(
                vec![MessagePart::ToolUse(ToolCall {
                    id: "echo-call".to_string(),
                    name: "echo".to_string(),
                    input: json!({"value": "evidence"}),
                })],
                StopReason::ToolUse,
            ),
            response(
                vec![MessagePart::Text {
                    text: "Review complete.".to_string(),
                }],
                StopReason::End,
            ),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let mut runner = runtime(3, provider.clone(), Arc::new(AtomicBool::new(false)));
    runner.register(EchoTool);

    let output = crate::core::agent::test_support::test_runtime()
        .block_on(runner.run_task("Inspect the change"))
        .unwrap();
    assert_eq!(output, "Review complete.");

    let requests = provider.requests.lock();
    assert_eq!(requests[0].system.len(), 2);
    assert_eq!(
        requests[0].system[1].text,
        "Review the supplied implementation."
    );
    assert_eq!(requests[0].tools.len(), 1);
    assert_eq!(requests[0].tools[0].name, "echo");
    assert!(requests[0].tools[0].cache);
    assert_eq!(
        requests[1].messages[2].role,
        crate::core::provider::MessageRole::Tool
    );
}

struct ConcurrentProbe {
    barrier: Arc<tokio::sync::Barrier>,
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for ConcurrentProbe {
    fn name(&self) -> &'static str {
        "concurrent_probe"
    }

    fn description(&self) -> &'static str {
        "Observe concurrent execution"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::Network
    }

    async fn execute(&self, input: &Value) -> Result<crate::core::agent::ToolOutput> {
        let content: Result<String> = async {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(input["label"].as_str().unwrap().to_string())
        }
        .await;
        Ok(crate::core::agent::ToolOutput::text(content?))
    }
}

#[test]
fn runner_executes_concurrent_tools_and_preserves_result_order() {
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::from([
            response(
                vec![
                    MessagePart::ToolUse(ToolCall {
                        id: "a".to_string(),
                        name: "concurrent_probe".to_string(),
                        input: json!({"label": "first"}),
                    }),
                    MessagePart::ToolUse(ToolCall {
                        id: "b".to_string(),
                        name: "concurrent_probe".to_string(),
                        input: json!({"label": "second"}),
                    }),
                ],
                StopReason::ToolUse,
            ),
            response(
                vec![MessagePart::Text {
                    text: "Combined result.".to_string(),
                }],
                StopReason::End,
            ),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let maximum = Arc::new(AtomicUsize::new(0));
    let mut runner = runtime(3, provider.clone(), Arc::new(AtomicBool::new(false)));
    runner.register(ConcurrentProbe {
        barrier: Arc::new(tokio::sync::Barrier::new(2)),
        active: Arc::new(AtomicUsize::new(0)),
        maximum: maximum.clone(),
    });

    let output = crate::core::agent::test_support::test_runtime()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(1), runner.run_task("Compare inputs")).await
        })
        .expect("concurrent subagent tools timed out")
        .unwrap();
    assert_eq!(output, "Combined result.");
    assert_eq!(maximum.load(Ordering::SeqCst), 2);

    let requests = provider.requests.lock();
    let results = &requests[1].messages[2..];
    assert_eq!(results.len(), 2);
    let contents = results
        .iter()
        .map(|message| match &message.parts[0] {
            MessagePart::ToolResult(result) => result.content.as_str(),
            _ => panic!("expected tool result"),
        })
        .collect::<Vec<_>>();
    assert_eq!(contents, ["first", "second"]);
}

struct SubagentImageProbe {
    name: &'static str,
    label: &'static str,
    receiver: Option<Arc<Mutex<Option<tokio::sync::mpsc::Receiver<()>>>>>,
    sender: Option<tokio::sync::mpsc::Sender<()>>,
    completion: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Tool for SubagentImageProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "Test tool returning a native image block"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::Network
    }

    async fn execute(&self, _input: &Value) -> Result<crate::core::agent::ToolOutput> {
        // Take the receiver out of the mutex and drop the guard before
        // the await, so the future stays Send.
        let receiver = self.receiver.as_ref().and_then(|slot| slot.lock().take());
        if let Some(mut receiver) = receiver {
            let _ = receiver.recv().await;
        }
        self.completion.lock().push(self.label.to_string());
        if let Some(sender) = &self.sender {
            let _ = sender.send(()).await;
        }
        Ok(crate::core::agent::ToolOutput {
            exit_code: None,
            content: format!("read {}", self.label),
            images: vec![crate::core::provider::ImageBlock {
                source: crate::core::provider::ImageSource::Url {
                    url: format!("https://example.test/{}.png", self.label),
                },
                label: format!("{}.png", self.label),
                media_type: crate::core::provider::ImageMediaType::Png,
                width: 8,
                height: 4,
                bytes: Some(Arc::from(vec![0xCDu8; 16])),
            }],
            ..crate::core::agent::ToolOutput::text("")
        })
    }
}

#[test]
fn runner_places_tool_images_after_all_tool_results() {
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::from([
            response(
                vec![
                    MessagePart::ToolUse(ToolCall {
                        id: "a".to_string(),
                        name: "image_probe_a".to_string(),
                        input: json!({"label": "first"}),
                    }),
                    MessagePart::ToolUse(ToolCall {
                        id: "b".to_string(),
                        name: "image_probe_b".to_string(),
                        input: json!({"label": "second"}),
                    }),
                ],
                StopReason::ToolUse,
            ),
            response(
                vec![MessagePart::Text {
                    text: "Images seen.".to_string(),
                }],
                StopReason::End,
            ),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let (sender, receiver) = tokio::sync::mpsc::channel::<()>(1);
    let completion = Arc::new(Mutex::new(Vec::new()));
    let mut runner = image_support_runtime(3, provider.clone(), Arc::new(AtomicBool::new(false)));
    runner.register(SubagentImageProbe {
        name: "image_probe_a",
        label: "first", // offset 0: waits for the second probe to finish
        receiver: Some(Arc::new(Mutex::new(Some(receiver)))),
        sender: None,
        completion: completion.clone(),
    });
    runner.register(SubagentImageProbe {
        name: "image_probe_b",
        label: "second", // offset 1: signals completion before returning
        receiver: None,
        sender: Some(sender),
        completion: completion.clone(),
    });

    let output = crate::core::agent::test_support::test_runtime()
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(1), runner.run_task("Compare inputs")).await
        })
        .expect("concurrent subagent image tools timed out")
        .unwrap();
    assert_eq!(output, "Images seen.");
    // Completed out of order (second before first) ...
    let completion_order = completion.lock();
    assert_eq!(completion_order.first().map(String::as_str), Some("second"));
    assert!(
        completion_order
            .iter()
            .position(|label| label == "second")
            .unwrap()
            < completion_order
                .iter()
                .position(|label| label == "first")
                .unwrap()
    );
    // ... yet the subagent conversation carries every text tool result in
    // offset order, then ONE user image message with both images in order.
    let requests = provider.requests.lock();
    let results = &requests[1].messages[2..];
    assert_eq!(results.len(), 3);
    assert!(
        matches!(&results[0].parts[0], MessagePart::ToolResult(result) if result.content == "read first")
    );
    assert!(
        matches!(&results[1].parts[0], MessagePart::ToolResult(result) if result.content == "read second")
    );
    assert_eq!(results[2].role, crate::core::provider::MessageRole::User);
    let parts = &results[2].parts;
    assert_eq!(parts.len(), 4);
    assert!(
        matches!(&parts[0], MessagePart::Text { text } if text == "Image returned by tool image_probe_a: first.png")
    );
    assert!(matches!(&parts[1], MessagePart::Image(block) if block.label == "first.png"));
    assert!(
        matches!(&parts[2], MessagePart::Text { text } if text == "Image returned by tool image_probe_b: second.png")
    );
    assert!(matches!(&parts[3], MessagePart::Image(block) if block.label == "second.png"));
}

struct PhaseTool {
    name: &'static str,
    policy: ToolExecutionPolicy,
    prerequisite: Option<Arc<AtomicBool>>,
    complete: Arc<AtomicBool>,
    yield_before_complete: bool,
}

#[async_trait::async_trait]
impl Tool for PhaseTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "Verify tool execution phases"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        self.policy
    }

    async fn execute(&self, _input: &Value) -> Result<crate::core::agent::ToolOutput> {
        let content: Result<String> = async {
            if self.yield_before_complete {
                tokio::task::yield_now().await;
            }
            if self
                .prerequisite
                .as_ref()
                .is_some_and(|ready| !ready.load(Ordering::SeqCst))
            {
                bail!("tool phase started before its prerequisite completed");
            }
            self.complete.store(true, Ordering::SeqCst);
            Ok(self.name.to_string())
        }
        .await;
        Ok(crate::core::agent::ToolOutput::text(content?))
    }
}

#[test]
fn runner_treats_exclusive_tools_as_wave_barriers() {
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::from([
            response(
                vec![
                    MessagePart::ToolUse(ToolCall {
                        id: "before".to_string(),
                        name: "before".to_string(),
                        input: json!({}),
                    }),
                    MessagePart::ToolUse(ToolCall {
                        id: "barrier".to_string(),
                        name: "barrier".to_string(),
                        input: json!({}),
                    }),
                    MessagePart::ToolUse(ToolCall {
                        id: "after".to_string(),
                        name: "after".to_string(),
                        input: json!({}),
                    }),
                ],
                StopReason::ToolUse,
            ),
            response(
                vec![MessagePart::Text {
                    text: "Phases complete.".to_string(),
                }],
                StopReason::End,
            ),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let before = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(AtomicBool::new(false));
    let after = Arc::new(AtomicBool::new(false));
    let mut runner = runtime(3, provider.clone(), Arc::new(AtomicBool::new(false)));
    runner.register(PhaseTool {
        name: "before",
        policy: ToolExecutionPolicy::LocalRead,
        prerequisite: None,
        complete: before.clone(),
        yield_before_complete: true,
    });
    runner.register(PhaseTool {
        name: "barrier",
        policy: ToolExecutionPolicy::Exclusive,
        prerequisite: Some(before),
        complete: barrier.clone(),
        yield_before_complete: false,
    });
    runner.register(PhaseTool {
        name: "after",
        policy: ToolExecutionPolicy::LocalRead,
        prerequisite: Some(barrier),
        complete: after.clone(),
        yield_before_complete: false,
    });

    let output = crate::core::agent::test_support::test_runtime()
        .block_on(runner.run_task("Run the phases"))
        .unwrap();
    assert_eq!(output, "Phases complete.");
    assert!(after.load(Ordering::SeqCst));
    let requests = provider.requests.lock();
    assert!(requests[1].messages[2..].iter().all(
        |message| matches!(&message.parts[0], MessagePart::ToolResult(result) if !result.is_error)
    ));
}

struct OutcomeProbe(ToolOutput, bool);
#[async_trait::async_trait]
impl Tool for OutcomeProbe {
    fn name(&self) -> &'static str {
        "outcome_probe"
    }
    fn description(&self) -> &'static str {
        "Return a controlled complete outcome"
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","additionalProperties":false})
    }
    async fn execute(&self, _: &Value) -> Result<ToolOutput> {
        if self.1 {
            anyhow::bail!(self.0.content.clone());
        }
        Ok(self.0.clone())
    }
}

#[test]
fn session_and_collaborator_agents_store_and_page_success_process_and_error_outcomes_identically() {
    for session in [true, false] {
        for kind in [0, 1, 2, 3] {
            let directory = tempfile::tempdir().unwrap();
            let storage = Storage::new(directory.path()).unwrap();
            storage.ensure_files().unwrap();
            let session_directory = directory.path().join("agents/test");
            let body = "evidence: \"中文\" \\path\nnext line\n".repeat(1000);
            let (output, header, error) = match kind {
                0 => (ToolOutput::text(&body), "Status: success\n\n", false),
                1 => (
                    ToolOutput::process(7, &body),
                    "Status: error\nExit code: 7\nError code: process_exit\n\n",
                    true,
                ),
                _ => (
                    ToolOutput::failure(ToolErrorCode::ToolFailed, &body),
                    "Status: error\nError code: tool_failed\n\n",
                    true,
                ),
            };
            let provider = Arc::new(ScriptedProvider {
                responses: Mutex::new(VecDeque::from([
                    response(
                        vec![MessagePart::ToolUse(ToolCall {
                            id: "probe".into(),
                            name: "outcome_probe".into(),
                            input: json!({}),
                        })],
                        StopReason::ToolUse,
                    ),
                    response(
                        vec![MessagePart::ToolUse(ToolCall {
                            id: "page".into(),
                            name: "read".into(),
                            input: json!({"path":"nole://result/1","range":"1-6"}),
                        })],
                        StopReason::ToolUse,
                    ),
                    response(
                        vec![MessagePart::Text {
                            text: "Evidence received".into(),
                        }],
                        StopReason::End,
                    ),
                ])),
                requests: Mutex::new(Vec::new()),
            });
            let mut config: AgentConfig =
                toml::from_str(test_support::TEST_MESSAGES_CONFIG).unwrap();
            config.max_subagent_rounds = 4;
            let (events, _) = tokio::sync::broadcast::channel(64);
            let environment =
                test_support::environment(&config, provider.clone(), events, &session_directory);
            let mut agent = if session {
                environment.create(Arc::default(), environment::RunPolicy::Session)
            } else {
                environment.collaborator(Arc::default(), "Gather evidence")
            };
            agent.register(OutcomeProbe(output, kind == 3));
            agent.register(
                Read::new(directory.path(), Arc::default(), reqwest::Client::new())
                    .unwrap()
                    .with_session_directory(&session_directory),
            );
            assert_eq!(
                test_support::test_runtime()
                    .block_on(agent.run_task("Read the full evidence"))
                    .unwrap(),
                "Evidence received"
            );
            let requests = provider.requests.lock();
            let result = requests[1]
                .messages
                .iter()
                .flat_map(|message| &message.parts)
                .find_map(|part| match part {
                    MessagePart::ToolResult(result) => Some(result),
                    _ => None,
                })
                .unwrap();
            assert_eq!(result.is_error, error);
            assert!(result.content.contains("nole://result/1"));
            assert!(result.content.len() < 1000);
            assert_eq!(
                fs::read_to_string(session_directory.join("results/1")).unwrap(),
                format!("{header}{body}")
            );
            let page = requests[2]
                .messages
                .iter()
                .rev()
                .flat_map(|message| &message.parts)
                .find_map(|part| match part {
                    MessagePart::ToolResult(result) => Some(result),
                    _ => None,
                })
                .unwrap();
            assert!(!page.is_error);
            assert!(page.content.contains("1:Status:"));
            assert!(page.content.contains("evidence: \"中文\" \\path"));
        }
    }
}
