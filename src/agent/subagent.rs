//! Reusable runtime for isolated, task-scoped agents.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::stream::{self, StreamExt};

use super::{
    images::DISABLED_IMAGE_ERROR, prompt_with_datetime, wait_for_provider_retry, AgentConfig,
    AgentEvent, AgentEventSender, RegisteredTool, Tool, ToolCallOutput, ToolConcurrencyLimits,
    ToolExecutionPolicy, MAX_PROVIDER_REQUEST_ATTEMPTS,
};
use crate::agent_session::TokenUsage;
use crate::provider::{
    is_transient_provider_error, Message, MessagePart, Provider, ProviderEvent, ProviderRequest,
    ReasoningEffort, StopReason, SystemBlock, ToolCall, ToolResult, ToolSpec,
};

#[derive(Clone)]
pub(crate) struct SubagentRuntime {
    model: String,
    max_tokens: u32,
    effort: Option<ReasoningEffort>,
    max_rounds: u32,
    supports_images: bool,
    provider: Arc<dyn Provider>,
    system: Vec<SystemBlock>,
    events: AgentEventSender,
    cancelled: Arc<AtomicBool>,
    concurrency: ToolConcurrencyLimits,
}

impl SubagentRuntime {
    pub(crate) fn new(
        config: &AgentConfig,
        provider: Arc<dyn Provider>,
        system: Vec<SystemBlock>,
        events: AgentEventSender,
        cancelled: Arc<AtomicBool>,
        concurrency: ToolConcurrencyLimits,
    ) -> Self {
        Self {
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            max_rounds: config.max_rounds,
            supports_images: config.supports_images,
            effort: config.effort,
            provider,
            system,
            events,
            cancelled,
            concurrency,
        }
    }
}

pub(crate) struct SubagentProfile {
    name: &'static str,
    instructions: String,
    final_round_prompt: String,
    incomplete_response_prompt: String,
}

impl SubagentProfile {
    pub(crate) fn new(
        name: &'static str,
        instructions: impl Into<String>,
        final_round_prompt: impl Into<String>,
        incomplete_response_prompt: impl Into<String>,
    ) -> Self {
        Self {
            name,
            instructions: instructions.into(),
            final_round_prompt: final_round_prompt.into(),
            incomplete_response_prompt: incomplete_response_prompt.into(),
        }
    }
}

pub(crate) struct SubagentRunner {
    runtime: SubagentRuntime,
    profile: SubagentProfile,
    tools: HashMap<String, Arc<RegisteredTool>>,
    definitions: Vec<ToolSpec>,
    system: Vec<SystemBlock>,
}

impl SubagentRunner {
    pub(crate) fn new(runtime: SubagentRuntime, profile: SubagentProfile) -> Self {
        let mut system = runtime.system.clone();
        system.push(SystemBlock {
            text: profile.instructions.clone(),
            cache: false,
        });
        Self {
            runtime,
            profile,
            tools: HashMap::new(),
            definitions: Vec::new(),
            system,
        }
    }

    pub(crate) fn register<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.name().to_string();
        let schema = tool.input_schema();
        let definition = ToolSpec {
            name: name.clone(),
            description: tool.description().to_string(),
            input_schema: schema.clone(),
            cache: false,
        };
        if let Some(index) = self
            .definitions
            .iter()
            .position(|definition| definition.name == name)
        {
            self.definitions[index] = definition;
        } else {
            self.definitions.push(definition);
        }
        self.tools
            .insert(name, Arc::new(RegisteredTool::new(tool, &schema)));
    }

    pub(crate) async fn run(&self, task: &str) -> Result<String> {
        let mut messages = vec![Message::user(prompt_with_datetime(
            task,
            chrono::Local::now(),
        ))];
        for round in 0..self.runtime.max_rounds {
            self.ensure_active()?;
            let final_round = round.saturating_add(1) == self.runtime.max_rounds;
            if final_round {
                messages.push(Message::user(self.profile.final_round_prompt.clone()));
            }
            let response = self.request(&messages, final_round).await?;
            let calls = response.message.tool_calls().cloned().collect::<Vec<_>>();
            let text = response.text();
            if !text.trim().is_empty() || !calls.is_empty() {
                messages.push(response.message);
            }
            if calls.is_empty() {
                if !text.trim().is_empty() && response.stop_reason != StopReason::Length {
                    return Ok(text);
                }
                if final_round {
                    bail!(
                        "{} subagent returned no complete response within its round budget",
                        self.profile.name
                    );
                }
                messages.push(Message::user(
                    self.profile.incomplete_response_prompt.clone(),
                ));
                continue;
            }
            let results = self
                .execute_tool_batch(&calls, &response.tool_input_errors)
                .await;
            messages.extend(results);
        }
        bail!("{} subagent exhausted its round budget", self.profile.name)
    }

    async fn request(
        &self,
        messages: &[Message],
        final_round: bool,
    ) -> Result<crate::provider::AssistantMessage> {
        let mut definitions = self.definitions.clone();
        if let Some(definition) = definitions.last_mut() {
            definition.cache = true;
        }
        let provider_request = ProviderRequest {
            model: self.runtime.model.clone(),
            max_tokens: self.runtime.max_tokens,
            effort: self.runtime.effort,
            system: self.system.clone(),
            messages: messages.to_vec(),
            tools: if final_round { Vec::new() } else { definitions },
        };
        for attempt in 0..MAX_PROVIDER_REQUEST_ATTEMPTS {
            let observable = self
                .runtime
                .provider
                .call_streaming(provider_request.clone());
            let provider_cancel = observable.cancel;
            let mut events = observable.events;
            let mut output = observable.output;
            let mut reported_usage = TokenUsage::default();
            let mut events_open = true;
            let result = loop {
                tokio::select! {
                    event = events.recv(), if events_open => match event {
                        Ok(event) => self.report_provider_event(event, &mut reported_usage),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            events_open = false;
                        }
                    },
                    result = &mut output => {
                        while let Ok(event) = events.try_recv() {
                            self.report_provider_event(event, &mut reported_usage);
                        }
                        if let Ok(response) = &result {
                            self.report_usage_delta(&mut reported_usage, response.token_usage);
                        }
                        break result;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if let Err(error) = self.ensure_active() {
                            provider_cancel.cancel();
                            break Err(error);
                        }
                    }
                }
            };
            match result {
                Err(error)
                    if is_transient_provider_error(&error)
                        && attempt + 1 < MAX_PROVIDER_REQUEST_ATTEMPTS =>
                {
                    let _ = self.runtime.events.send(AgentEvent::Retry);
                    wait_for_provider_retry(&self.runtime.cancelled, attempt).await?;
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn execute_tool_batch(
        &self,
        calls: &[ToolCall],
        input_errors: &HashMap<String, String>,
    ) -> Vec<Message> {
        let mut outputs = Vec::with_capacity(calls.len());
        let mut index = 0;
        while index < calls.len() {
            let policy = self.tool_execution_policy(&calls[index]);
            let end = if policy.is_concurrent() {
                calls[index..]
                    .iter()
                    .position(|call| !self.tool_execution_policy(call).is_concurrent())
                    .map_or(calls.len(), |offset| index + offset)
            } else {
                index + 1
            };
            let wave = &calls[index..end];
            let mut executions = stream::iter(wave.iter().cloned().enumerate())
                .map(|(offset, call)| async move {
                    let result = self.execute_call(&call, input_errors.get(&call.id)).await;
                    (index + offset, result)
                })
                .buffer_unordered(wave.len().max(1))
                .collect::<Vec<_>>()
                .await;
            executions.sort_by_key(|(offset, _)| *offset);
            outputs.extend(executions);
            index = end;
        }
        outputs.sort_by_key(|(offset, _)| *offset);
        let mut messages = Vec::with_capacity(outputs.len() + 1);
        let mut trailing = Vec::new();
        for (offset, output) in outputs {
            messages.push(Message::tool(output.result));
            if !output.images.is_empty() {
                let name = calls
                    .get(offset)
                    .map(|call| call.name.as_str())
                    .unwrap_or("");
                for block in output.images {
                    trailing.push(MessagePart::Text {
                        text: format!("Image returned by tool {name}: {}", block.label),
                    });
                    trailing.push(MessagePart::Image(block));
                }
            }
        }
        if !trailing.is_empty() {
            messages.push(Message::user_parts(trailing));
        }
        messages
    }

    fn tool_execution_policy(&self, call: &ToolCall) -> ToolExecutionPolicy {
        self.tools
            .get(&call.name)
            .map_or(ToolExecutionPolicy::Exclusive, |tool| {
                tool.execution_policy()
            })
    }

    async fn execute_call(&self, call: &ToolCall, input_error: Option<&String>) -> ToolCallOutput {
        let result = match input_error {
            Some(error) => Err(anyhow::anyhow!(error.clone())),
            None => match self
                .tools
                .get(&call.name)
                .cloned()
                .context("unknown subagent tool")
            {
                Ok(tool) => {
                    let _permit = self
                        .runtime
                        .concurrency
                        .acquire(tool.execution_policy())
                        .await;
                    match self.ensure_active() {
                        Ok(()) => tool.execute(&call.input).await,
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            },
        };
        match result {
            Ok(output) => {
                if !output.images.is_empty() && !self.runtime.supports_images {
                    ToolCallOutput::text(ToolResult {
                        tool_use_id: call.id.clone(),
                        content: DISABLED_IMAGE_ERROR.to_string(),
                        is_error: true,
                    })
                } else {
                    ToolCallOutput::with_images(
                        ToolResult {
                            tool_use_id: call.id.clone(),
                            content: output.text,
                            is_error: false,
                        },
                        output.images,
                    )
                }
            }
            Err(error) => ToolCallOutput::text(ToolResult {
                tool_use_id: call.id.clone(),
                content: super::tool_error_message(&error),
                is_error: true,
            }),
        }
    }

    fn report_provider_event(&self, event: ProviderEvent, reported_usage: &mut TokenUsage) {
        match event {
            ProviderEvent::Usage { usage, .. } => {
                self.report_usage_delta(reported_usage, usage);
            }
            ProviderEvent::Retry => {
                let _ = self.runtime.events.send(AgentEvent::Retry);
            }
            ProviderEvent::TextDelta(_)
            | ProviderEvent::ThinkingDelta(_)
            | ProviderEvent::ThinkingFinished
            | ProviderEvent::ToolCallDelta { .. }
            | ProviderEvent::ToolCallFinished { .. } => {}
        }
    }

    fn report_usage_delta(&self, reported_usage: &mut TokenUsage, usage: TokenUsage) {
        self.report_usage(usage.saturating_sub(*reported_usage));
        reported_usage.input_tokens = reported_usage.input_tokens.max(usage.input_tokens);
        reported_usage.output_tokens = reported_usage.output_tokens.max(usage.output_tokens);
        reported_usage.cache_creation_input_tokens = reported_usage
            .cache_creation_input_tokens
            .max(usage.cache_creation_input_tokens);
        reported_usage.cache_read_input_tokens = reported_usage
            .cache_read_input_tokens
            .max(usage.cache_read_input_tokens);
    }

    fn ensure_active(&self) -> Result<()> {
        if self.runtime.cancelled.load(Ordering::Relaxed) {
            bail!("agent task cancelled");
        }
        Ok(())
    }

    fn report_usage(&self, usage: TokenUsage) {
        if !usage.is_empty() {
            let _ = self.runtime.events.send(AgentEvent::Usage(usage));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use anyhow::{bail, Context};
    use parking_lot::Mutex;
    use serde_json::{json, Value};

    use super::*;
    use crate::observable::{BoxFuture, Observable};
    use crate::provider::{
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

        fn count_tokens<'a>(&'a self, _request: ProviderRequest) -> BoxFuture<'a, Option<u64>> {
            Box::pin(async { Ok(None) })
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

        fn count_tokens<'a>(&'a self, _request: ProviderRequest) -> BoxFuture<'a, Option<u64>> {
            Box::pin(async { Ok(None) })
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
        max_rounds: u32,
        provider: Arc<dyn Provider>,
        cancelled: Arc<AtomicBool>,
    ) -> SubagentRuntime {
        let config = AgentConfig {
            api_format: ApiFormat::Messages,
            api_key: "test".to_string(),
            tavily_api_key: String::new(),
            model: "test-model".to_string(),
            base_url: "https://example.com".to_string(),
            max_tokens: 1_024,
            effort: None,
            context_window_tokens: 8_192,
            max_rounds,
            max_concurrent_local_reads: 8,
            max_concurrent_network_tools: 8,
            max_concurrent_subagents: 4,
            supports_images: false,
        };
        let (events, _receiver) = tokio::sync::broadcast::channel(16);
        SubagentRuntime::new(
            &config,
            provider,
            vec![SystemBlock {
                text: "shared parent context".to_string(),
                cache: true,
            }],
            events,
            cancelled,
            ToolConcurrencyLimits::new(8, 8, 4),
        )
    }

    fn profile() -> SubagentProfile {
        SubagentProfile::new(
            "review",
            "Review the supplied implementation.",
            "Return the final review now.",
            "Return a complete review.",
        )
    }

    fn image_support_runtime(
        max_rounds: u32,
        provider: Arc<dyn Provider>,
        cancelled: Arc<AtomicBool>,
    ) -> SubagentRuntime {
        let config = AgentConfig {
            api_format: ApiFormat::Messages,
            api_key: "test".to_string(),
            tavily_api_key: String::new(),
            model: "test-model".to_string(),
            base_url: "https://example.com".to_string(),
            max_tokens: 1_024,
            effort: None,
            context_window_tokens: 8_192,
            max_rounds,
            max_concurrent_local_reads: 8,
            max_concurrent_network_tools: 8,
            max_concurrent_subagents: 4,
            supports_images: true,
        };
        let (events, _receiver) = tokio::sync::broadcast::channel(16);
        SubagentRuntime::new(
            &config,
            provider,
            vec![SystemBlock {
                text: "shared parent context".to_string(),
                cache: true,
            }],
            events,
            cancelled,
            ToolConcurrencyLimits::new(8, 8, 4),
        )
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

        async fn execute(&self, input: &Value) -> Result<String> {
            Ok(input["value"].as_str().unwrap().to_string())
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
        let runner = SubagentRunner::new(
            runtime(3, provider.clone(), Arc::new(AtomicBool::new(false))),
            profile(),
        );

        assert_eq!(
            crate::agent::test_support::test_runtime()
                .block_on(runner.run("Review"))
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
        let runner = SubagentRunner::new(
            runtime(3, provider.clone(), Arc::new(AtomicBool::new(false))),
            profile(),
        );

        let error = crate::agent::test_support::test_runtime()
            .block_on(runner.run("Review"))
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
        let mut runner = SubagentRunner::new(
            runtime(2, provider.clone(), Arc::new(AtomicBool::new(false))),
            profile(),
        );
        runner.register(EchoTool);

        let output = crate::agent::test_support::test_runtime()
            .block_on(runner.run("Review"))
            .unwrap();
        assert_eq!(output, "Complete review");

        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].tools.is_empty());
        assert!(requests[1].tools.is_empty());
        assert_eq!(requests[1].messages[1].text(), "Partial review");
        assert_eq!(requests[1].messages[2].text(), "Return a complete review.");
        assert_eq!(
            requests[1].messages[3].text(),
            "Return the final review now."
        );
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
        let runner = SubagentRunner::new(
            runtime(1, provider, Arc::new(AtomicBool::new(false))),
            profile(),
        );

        let error = crate::agent::test_support::test_runtime()
            .block_on(runner.run("Review"))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "review subagent returned no complete response within its round budget"
        );
    }

    #[test]
    fn runner_uses_caller_profile_and_tool_set() {
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
        let mut runner = SubagentRunner::new(
            runtime(3, provider.clone(), Arc::new(AtomicBool::new(false))),
            profile(),
        );
        runner.register(EchoTool);

        let output = crate::agent::test_support::test_runtime()
            .block_on(runner.run("Inspect the change"))
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
            crate::provider::MessageRole::Tool
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

        async fn execute(&self, input: &Value) -> Result<String> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(input["label"].as_str().unwrap().to_string())
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
        let mut runner = SubagentRunner::new(
            runtime(3, provider.clone(), Arc::new(AtomicBool::new(false))),
            profile(),
        );
        runner.register(ConcurrentProbe {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            active: Arc::new(AtomicUsize::new(0)),
            maximum: maximum.clone(),
        });

        let output = crate::agent::test_support::test_runtime()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(1), runner.run("Compare inputs")).await
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

        async fn execute(&self, _input: &Value) -> Result<String> {
            Ok("unused".to_string())
        }

        async fn execute_output(&self, _input: &Value) -> Result<crate::agent::ToolOutput> {
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
            Ok(crate::agent::ToolOutput {
                text: format!("read {}", self.label),
                images: vec![crate::provider::ImageBlock {
                    source: crate::provider::ImageSource::Url {
                        url: format!("https://example.test/{}.png", self.label),
                    },
                    label: format!("{}.png", self.label),
                    media_type: crate::provider::ImageMediaType::Png,
                    width: 8,
                    height: 4,
                    bytes: Some(Arc::from(vec![0xCDu8; 16])),
                }],
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
        let mut runner = SubagentRunner::new(
            image_support_runtime(3, provider.clone(), Arc::new(AtomicBool::new(false))),
            profile(),
        );
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

        let output = crate::agent::test_support::test_runtime()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(1), runner.run("Compare inputs")).await
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
        assert_eq!(results[2].role, crate::provider::MessageRole::User);
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

        async fn execute(&self, _input: &Value) -> Result<String> {
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
        let mut runner = SubagentRunner::new(
            runtime(3, provider.clone(), Arc::new(AtomicBool::new(false))),
            profile(),
        );
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

        let output = crate::agent::test_support::test_runtime()
            .block_on(runner.run("Run the phases"))
            .unwrap();
        assert_eq!(output, "Phases complete.");
        assert!(after.load(Ordering::SeqCst));
        let requests = provider.requests.lock();
        assert!(requests[1].messages[2..]
            .iter()
            .all(|message| matches!(&message.parts[0], MessagePart::ToolResult(result) if !result.is_error)));
    }
}
