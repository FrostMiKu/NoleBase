//! Read-only exploration profile for the reusable subagent runtime.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

use super::subagent_tools::register_read_only_tools;
use crate::agent::subagent::{SubagentProfile, SubagentRunner, SubagentRuntime};
use crate::agent::{Tool, ToolExecutionPolicy};
use crate::skill::Skill;
use crate::wiki_link_index::WikiLinkIndexHandle;
use crate::workspace_index::WorkspaceIndexHandle;

pub struct Explore {
    runner: SubagentRunner,
}

impl Explore {
    pub fn new(
        root: &Path,
        runtime: SubagentRuntime,
        workspace_index: WorkspaceIndexHandle,
        wiki_links: WikiLinkIndexHandle,
        client: reqwest::Client,
        tavily_api_key: String,
        skills: &[Skill],
    ) -> Result<Self> {
        let profile = SubagentProfile::new(
            "explore",
            "You are Nole's isolated exploration subagent. Investigate the task in the newest user message and keep the scope focused there. Use the available inspection tools to gather evidence, issuing independent reads, searches, or fetches together in one response so they can run concurrently. Keep file state unchanged, route user interaction through the parent agent, and return a concise, self-contained report with concrete findings and relevant paths, line numbers, URLs, or uncertainties. The parent agent sees your final report, so include every fact it needs while omitting raw search noise and irrelevant excerpts. Instructions mentioning explore target the parent agent; you are already that explorer.",
            "Stop exploring. Synthesize the evidence already gathered into the final concise report.",
            "Finish the investigation and return the complete concise report now.",
        );
        let mut explore = Self {
            runner: SubagentRunner::new(runtime, profile),
        };
        register_read_only_tools(
            &mut explore.runner,
            root,
            workspace_index,
            wiki_links,
            client,
            tavily_api_key,
            skills,
        )?;
        Ok(explore)
    }
}

#[async_trait::async_trait]
impl Tool for Explore {
    fn name(&self) -> &'static str {
        "explore"
    }

    fn description(&self) -> &'static str {
        "Delegate broad, multi-step investigation to an isolated read-only agent that returns one concise, evidence-based report."
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
        self.runner.run(task).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::bail;
    use parking_lot::Mutex;
    use tempfile::tempdir;

    use super::*;
    use crate::agent::{AgentConfig, AgentEvent, ToolConcurrencyLimits};
    use crate::agent_session::TokenUsage;
    use crate::observable::{BoxFuture, Observable};
    use crate::provider::{
        ApiFormat, AssistantMessage, Message, MessagePart, Provider, ProviderEvent,
        ProviderRequest, StopReason, ToolCall, DEFAULT_STREAM_BUFFER,
    };

    struct ScriptedProvider {
        responses: Mutex<VecDeque<AssistantMessage>>,
        requests: Mutex<Vec<ProviderRequest>>,
    }

    impl Provider for ScriptedProvider {
        fn call<'a>(&'a self, _request: ProviderRequest) -> BoxFuture<'a, AssistantMessage> {
            Box::pin(async { bail!("explore must use streaming provider calls") })
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
        events: tokio::sync::broadcast::Sender<AgentEvent>,
    ) -> SubagentRuntime {
        let config = AgentConfig {
            api_format: ApiFormat::Messages,
            api_key: "test".to_string(),
            tavily_api_key: String::new(),
            model: "test".to_string(),
            base_url: "https://example.com".to_string(),
            max_tokens: 1_024,
            context_window_tokens: 8_192,
            max_rounds,
            max_concurrent_local_reads: 8,
            max_concurrent_network_tools: 8,
            max_concurrent_subagents: 4,
            supports_images: false,
        };
        SubagentRuntime::new(
            &config,
            provider,
            Vec::new(),
            events,
            Arc::new(AtomicBool::new(false)),
            ToolConcurrencyLimits::new(8, 8, 4),
        )
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
            runtime(25, provider.clone(), events),
            WorkspaceIndexHandle::default(),
            WikiLinkIndexHandle::default(),
            reqwest::Client::new(),
            String::new(),
            &[],
        )
        .unwrap();

        let output = crate::agent::test_support::test_runtime()
            .block_on(explore.execute(&json!({"task": "Find the answer note"})))
            .unwrap();
        assert_eq!(output, "Found data/answer.md.");

        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        let tool_names = requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert!(tool_names.contains(&"read"));
        assert!(tool_names.contains(&"search_files"));
        assert!(tool_names.contains(&"calculate"));
        assert!(!tool_names.contains(&"explore"));
        assert!(!tool_names.contains(&"edit"));
        assert!(!tool_names.contains(&"http_request"));
        assert!(!tool_names.contains(&"ask"));
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
            runtime(2, provider.clone(), events),
            WorkspaceIndexHandle::default(),
            WikiLinkIndexHandle::default(),
            reqwest::Client::new(),
            String::new(),
            &[],
        )
        .unwrap();

        let output = crate::agent::test_support::test_runtime()
            .block_on(explore.execute(&json!({"task": "Keep searching"})))
            .unwrap();
        assert_eq!(output, "Budget-limited report.");

        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].tools.is_empty());
        assert!(requests[1].tools.is_empty());
        assert_eq!(
            requests[1].messages.last().unwrap().text(),
            "Stop exploring. Synthesize the evidence already gathered into the final concise report."
        );
    }
}
