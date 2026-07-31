//! Isolated, read-only agent used for broad exploration and research.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures_util::stream::{self, StreamExt};
use serde_json::{json, Value};

use super::{
    ListDirectory, ListNotes, ListTags, LoadSkill, ReadFile, SearchContent, SearchFiles, SearchTag,
    WebFetch, WebSearch,
};
use crate::agent::{
    prompt_with_datetime, AgentConfig, AgentEvent, AgentEventSender, ReadTracker, Tool,
    ToolConcurrencyLimits, ToolExecutionPolicy,
};
use crate::agent_session::TokenUsage;
use crate::provider::{
    Message, Provider, ProviderRequest, StopReason, SystemBlock, ToolCall, ToolResult, ToolSpec,
};
use crate::skill::Skill;
use crate::workspace_index::WorkspaceIndexHandle;

pub struct Explore {
    config: AgentConfig,
    provider: Arc<dyn Provider>,
    tools: HashMap<String, Arc<dyn Tool>>,
    definitions: Vec<ToolSpec>,
    system: Vec<SystemBlock>,
    events: AgentEventSender,
    concurrency: ToolConcurrencyLimits,
}

impl Explore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: &Path,
        config: AgentConfig,
        provider: Arc<dyn Provider>,
        mut system: Vec<SystemBlock>,
        events: AgentEventSender,
        workspace_index: WorkspaceIndexHandle,
        client: reqwest::Client,
        tavily_api_key: String,
        skills: &[Skill],
        concurrency: ToolConcurrencyLimits,
    ) -> Result<Self> {
        system.push(SystemBlock {
            text: "You are Nole's isolated exploration subagent. Investigate only the task in the newest user message. Use the available read-only tools to gather evidence, issuing independent reads, searches, or fetches together in one response so they can run concurrently. Do not attempt to modify files, ask the user questions, call another agent, or describe your working process. Return a concise, self-contained report with concrete findings and relevant paths, line numbers, URLs, or uncertainties. The parent agent sees only your final report, so include every fact it needs while excluding raw search noise and irrelevant excerpts. Any instruction above to call explore applies only to the parent agent; you are already that explorer."
                .to_string(),
            cache: false,
        });
        let mut explore = Self {
            config,
            provider,
            tools: HashMap::new(),
            definitions: Vec::new(),
            system,
            events,
            concurrency,
        };
        let reads = Arc::new(ReadTracker::default());
        explore.register(ReadFile::new(root, reads)?);
        explore.register(ListDirectory::new(root)?);
        explore.register(ListNotes::new(root)?);
        explore.register(SearchContent::new(root)?);
        explore.register(SearchFiles::new(root)?);
        explore.register(ListTags::new(workspace_index.clone()));
        explore.register(SearchTag::new(root, workspace_index)?);
        explore.register(LoadSkill::new(skills));
        if !tavily_api_key.is_empty() {
            explore.register(WebSearch {
                client: client.clone(),
                api_key: tavily_api_key,
            });
        }
        explore.register(WebFetch { client });
        if let Some(definition) = explore.definitions.last_mut() {
            definition.cache = true;
        }
        Ok(explore)
    }

    fn register<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.name().to_string();
        self.definitions.push(ToolSpec {
            name: name.clone(),
            description: tool.description().to_string(),
            input_schema: tool.input_schema(),
            cache: false,
        });
        self.tools.insert(name, Arc::new(tool));
    }

    async fn run(&self, task: &str) -> Result<String> {
        let mut messages = vec![Message::user(prompt_with_datetime(
            task,
            chrono::Local::now(),
        ))];
        for round in 0..self.config.max_rounds {
            let final_round = round.saturating_add(1) == self.config.max_rounds;
            if final_round {
                messages.push(Message::user(
                    "Stop exploring. Synthesize the evidence already gathered into the final concise report."
                        .to_string(),
                ));
            }
            let response = self
                .provider
                .call(ProviderRequest {
                    model: self.config.model.clone(),
                    max_tokens: self.config.max_tokens,
                    system: self.system.clone(),
                    messages: messages.clone(),
                    tools: if final_round {
                        Vec::new()
                    } else {
                        self.definitions.clone()
                    },
                })
                .await?;
            self.report_usage(response.token_usage, response.generation_duration);
            let calls = response.message.tool_calls().cloned().collect::<Vec<_>>();
            let text = response.text();
            messages.push(response.message);
            if calls.is_empty() {
                if !text.trim().is_empty() && response.stop_reason != StopReason::Length {
                    return Ok(text);
                }
                if final_round {
                    bail!(
                        "exploration subagent returned no complete report within its round budget"
                    );
                }
                messages.push(Message::user(
                    "Finish the investigation and return the complete concise report now."
                        .to_string(),
                ));
                continue;
            }
            let call_count = calls.len();
            let mut results = stream::iter(calls.iter().cloned().enumerate())
                .map(|(index, call)| {
                    let input_error = response.tool_input_errors.get(&call.id).cloned();
                    async move {
                        let result = match input_error {
                            Some(error) => ToolResult {
                                tool_use_id: call.id.clone(),
                                content: error,
                                is_error: true,
                            },
                            None => self.execute_call(&call).await,
                        };
                        (index, result)
                    }
                })
                .buffer_unordered(call_count.max(1))
                .collect::<Vec<_>>()
                .await;
            results.sort_by_key(|(index, _)| *index);
            for (_, result) in results {
                messages.push(Message::tool(result));
            }
        }
        bail!("exploration subagent exhausted its round budget")
    }

    async fn execute_call(&self, call: &ToolCall) -> ToolResult {
        let result = match self
            .tools
            .get(&call.name)
            .cloned()
            .context("unknown exploration tool")
        {
            Ok(tool) => {
                let _permit = self.concurrency.acquire(tool.execution_policy()).await;
                tool.execute(&call.input).await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(content) => ToolResult {
                tool_use_id: call.id.clone(),
                content,
                is_error: false,
            },
            Err(error) => ToolResult {
                tool_use_id: call.id.clone(),
                content: error.to_string(),
                is_error: true,
            },
        }
    }

    fn report_usage(&self, usage: TokenUsage, generation_duration: std::time::Duration) {
        if !usage.is_empty() {
            let _ = self.events.send(AgentEvent::Usage(usage));
        }
        if usage.output_tokens > 0 {
            let _ = self.events.send(AgentEvent::ResponseTiming {
                output_tokens: usage.output_tokens,
                elapsed: generation_duration,
            });
        }
    }
}

#[async_trait::async_trait]
impl Tool for Explore {
    fn name(&self) -> &'static str {
        "explore"
    }

    fn description(&self) -> &'static str {
        "Delegate broad, multi-step exploration, search, or research to an isolated read-only agent. Its internal tool calls and intermediate context stay private; this tool returns only a concise evidence-based report. Provide a focused, self-contained task and the exact questions the report must answer."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "A self-contained investigation task, including scope and required output."
                }
            },
            "required": ["task"],
            "additionalProperties": false
        })
    }

    fn execution_policy(&self) -> ToolExecutionPolicy {
        ToolExecutionPolicy::Subagent
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let task = input
            .get("task")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|task| !task.is_empty())
            .context("field task must be a non-empty string")?;
        self.run(task).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;
    use crate::observable::{BoxFuture, Observable};
    use crate::provider::{
        ApiFormat, AssistantMessage, MessagePart, ProviderEvent, DEFAULT_STREAM_BUFFER,
    };

    struct ScriptedProvider {
        responses: Mutex<VecDeque<AssistantMessage>>,
        requests: Mutex<Vec<ProviderRequest>>,
    }

    impl Provider for ScriptedProvider {
        fn call<'a>(&'a self, request: ProviderRequest) -> BoxFuture<'a, AssistantMessage> {
            Box::pin(async move {
                self.requests.lock().unwrap().push(request);
                self.responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .context("missing scripted response")
            })
        }

        fn call_streaming(
            &self,
            _request: ProviderRequest,
        ) -> Observable<AssistantMessage, ProviderEvent> {
            let (_events, receiver) = tokio::sync::broadcast::channel(DEFAULT_STREAM_BUFFER);
            Observable {
                output: Box::pin(async { bail!("streaming is not used by explore") }),
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

    #[test]
    fn exploration_keeps_internal_tool_history_out_of_its_result() {
        let directory = tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("config")).unwrap();
        std::fs::create_dir_all(directory.path().join("data")).unwrap();
        std::fs::create_dir_all(directory.path().join("daily")).unwrap();
        std::fs::create_dir_all(directory.path().join("archives")).unwrap();
        std::fs::write(directory.path().join("config/ai.toml"), "private").unwrap();
        std::fs::write(directory.path().join("data/answer.md"), "needle\n").unwrap();

        let provider = Arc::new(ScriptedProvider {
            responses: Mutex::new(VecDeque::from([
                response(
                    vec![MessagePart::ToolUse(ToolCall {
                        id: "lookup".to_string(),
                        name: "search_files".to_string(),
                        input: json!({"query": "answer"}),
                    })],
                    StopReason::ToolUse,
                ),
                response(
                    vec![MessagePart::Text {
                        text: "Found data/answer.md.".to_string(),
                    }],
                    StopReason::End,
                ),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let (events, _receiver) = tokio::sync::broadcast::channel(16);
        let explore = Explore::new(
            directory.path(),
            AgentConfig {
                api_format: ApiFormat::Messages,
                api_key: "test".to_string(),
                tavily_api_key: String::new(),
                model: "test".to_string(),
                base_url: "https://example.com".to_string(),
                max_tokens: 1_024,
                context_window_tokens: 8_192,
                max_rounds: 25,
                max_concurrent_local_reads: 8,
                max_concurrent_network_tools: 8,
                max_concurrent_subagents: 4,
            },
            provider.clone(),
            Vec::new(),
            events,
            WorkspaceIndexHandle::default(),
            reqwest::Client::new(),
            String::new(),
            &[],
            ToolConcurrencyLimits::new(8, 8, 4),
        )
        .unwrap();

        let output = crate::agent::test_support::test_runtime()
            .block_on(explore.execute(&json!({"task": "Find the answer note"})))
            .unwrap();
        assert_eq!(output, "Found data/answer.md.");

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let tool_names = requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&"read_file"));
        assert!(tool_names.contains(&"search_files"));
        assert!(!tool_names.contains(&"explore"));
        assert!(!tool_names.contains(&"edit_file"));
        assert!(!tool_names.contains(&"ask_user"));
        assert_eq!(requests[1].messages.len(), 3);
        assert_eq!(
            requests[1].messages[2].role,
            crate::provider::MessageRole::Tool
        );
    }

    #[test]
    fn exploration_uses_its_own_configured_round_budget_then_synthesizes() {
        let directory = tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("config")).unwrap();
        std::fs::create_dir_all(directory.path().join("data")).unwrap();
        std::fs::create_dir_all(directory.path().join("daily")).unwrap();
        std::fs::create_dir_all(directory.path().join("archives")).unwrap();
        std::fs::write(directory.path().join("config/ai.toml"), "private").unwrap();

        let provider = Arc::new(ScriptedProvider {
            responses: Mutex::new(VecDeque::from([
                response(
                    vec![MessagePart::ToolUse(ToolCall {
                        id: "first".to_string(),
                        name: "search_files".to_string(),
                        input: json!({"query": "first"}),
                    })],
                    StopReason::ToolUse,
                ),
                response(
                    vec![MessagePart::Text {
                        text: "Budget-limited report.".to_string(),
                    }],
                    StopReason::End,
                ),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let (events, _receiver) = tokio::sync::broadcast::channel(16);
        let explore = Explore::new(
            directory.path(),
            AgentConfig {
                api_format: ApiFormat::Messages,
                api_key: "test".to_string(),
                tavily_api_key: String::new(),
                model: "test".to_string(),
                base_url: "https://example.com".to_string(),
                max_tokens: 1_024,
                context_window_tokens: 8_192,
                max_rounds: 2,
                max_concurrent_local_reads: 8,
                max_concurrent_network_tools: 8,
                max_concurrent_subagents: 4,
            },
            provider.clone(),
            Vec::new(),
            events,
            WorkspaceIndexHandle::default(),
            reqwest::Client::new(),
            String::new(),
            &[],
            ToolConcurrencyLimits::new(8, 8, 4),
        )
        .unwrap();

        let output = crate::agent::test_support::test_runtime()
            .block_on(explore.execute(&json!({"task": "Keep searching"})))
            .unwrap();
        assert_eq!(output, "Budget-limited report.");

        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].tools.is_empty());
        assert!(requests[1].tools.is_empty());
    }

    struct ConcurrentExploreProbe {
        barrier: Arc<tokio::sync::Barrier>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        maximum: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tool for ConcurrentExploreProbe {
        fn name(&self) -> &'static str {
            "concurrent_explore_probe"
        }

        fn description(&self) -> &'static str {
            "Test-only concurrent exploration tool"
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn execution_policy(&self) -> ToolExecutionPolicy {
            ToolExecutionPolicy::Network
        }

        async fn execute(&self, input: &Value) -> Result<String> {
            use std::sync::atomic::Ordering;

            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(input["label"].as_str().unwrap().to_string())
        }
    }

    #[test]
    fn exploration_runs_parallel_network_calls_concurrently() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let directory = tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("config")).unwrap();
        std::fs::create_dir_all(directory.path().join("data")).unwrap();
        std::fs::create_dir_all(directory.path().join("daily")).unwrap();
        std::fs::create_dir_all(directory.path().join("archives")).unwrap();
        std::fs::write(directory.path().join("config/ai.toml"), "private").unwrap();
        let provider = Arc::new(ScriptedProvider {
            responses: Mutex::new(VecDeque::from([
                response(
                    vec![
                        MessagePart::ToolUse(ToolCall {
                            id: "a".to_string(),
                            name: "concurrent_explore_probe".to_string(),
                            input: json!({"label": "first"}),
                        }),
                        MessagePart::ToolUse(ToolCall {
                            id: "b".to_string(),
                            name: "concurrent_explore_probe".to_string(),
                            input: json!({"label": "second"}),
                        }),
                    ],
                    StopReason::ToolUse,
                ),
                response(
                    vec![MessagePart::Text {
                        text: "Combined report.".to_string(),
                    }],
                    StopReason::End,
                ),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let (events, _receiver) = tokio::sync::broadcast::channel(16);
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut explore = Explore::new(
            directory.path(),
            AgentConfig {
                api_format: ApiFormat::Messages,
                api_key: "test".to_string(),
                tavily_api_key: String::new(),
                model: "test".to_string(),
                base_url: "https://example.com".to_string(),
                max_tokens: 1_024,
                context_window_tokens: 8_192,
                max_rounds: 25,
                max_concurrent_local_reads: 8,
                max_concurrent_network_tools: 8,
                max_concurrent_subagents: 4,
            },
            provider,
            Vec::new(),
            events,
            WorkspaceIndexHandle::default(),
            reqwest::Client::new(),
            String::new(),
            &[],
            ToolConcurrencyLimits::new(8, 8, 4),
        )
        .unwrap();
        explore.register(ConcurrentExploreProbe {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            active,
            maximum: maximum.clone(),
        });

        let output = crate::agent::test_support::test_runtime()
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(1),
                    explore.execute(&json!({"task": "Compare both sources"})),
                )
                .await
            })
            .expect("parallel exploration calls timed out")
            .unwrap();

        assert_eq!(output, "Combined report.");
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
    }
}
