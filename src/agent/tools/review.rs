//! Domain-neutral review profile for the reusable subagent runtime.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{
    Backlinks, Calculate, Grep, ListNotes, ListTags, LoadSkill, Read, ResolveWikilink, SearchFiles,
    SearchTag, SearchWeb,
};
use crate::agent::subagent::{SubagentProfile, SubagentRunner, SubagentRuntime};
use crate::agent::{SnapshotStore, Tool, ToolExecutionPolicy};
use crate::skill::Skill;
use crate::wiki_link_index::WikiLinkIndexHandle;
use crate::workspace_index::WorkspaceIndexHandle;

pub struct Review {
    runner: SubagentRunner,
}

impl Review {
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
            "review",
            "You are Nole's isolated review subagent. Independently and critically evaluate only the artifact or output in the newest user message, using the goals, constraints, standards, and concerns stated there as the authoritative review scope. Do not substitute a preset domain checklist, impose unstated preferences, or broaden the assignment. Use the available read-only tools to inspect referenced sources or context; issue independent reads, searches, or fetches together in one response so they can run concurrently. Judge only what you can observe or reasonably infer; never invent findings, and never cite evidence you have not seen. Distinguish observed defects from inference and uncertainty, prioritize findings by their impact on the stated task, and explain why each finding matters. If the evidence supports no material findings, say so directly. Do not attempt to modify anything, ask the user questions, call another agent, or describe your working process. Return one concise, self-contained review; the parent agent sees only that final review, so include the evidence and qualifications it needs while excluding raw search noise. Any instruction above to call explore or review applies only to the parent agent; you are already the reviewer.",
            "Stop gathering evidence. Synthesize the issues already identified into the final self-contained review.",
            "Finish the review and return the complete self-contained review now.",
        );
        let mut review = Self {
            runner: SubagentRunner::new(runtime, profile),
        };
        let reads = Arc::new(SnapshotStore::default());
        review.register(Read::new(root, reads, client.clone())?);
        review.register(ListNotes::new(root)?);
        review.register(Grep::new(root)?);
        review.register(SearchFiles::new(root)?);
        review.register(ListTags::new(workspace_index.clone()));
        review.register(SearchTag::new(root, workspace_index)?);
        review.register(ResolveWikilink::new(root, wiki_links.clone())?);
        review.register(Backlinks::new(root, wiki_links)?);
        review.register(Calculate);
        review.register(LoadSkill::new(skills));
        if !tavily_api_key.is_empty() {
            review.register(SearchWeb {
                client: client.clone(),
                api_key: tavily_api_key,
            });
        }
        Ok(review)
    }

    fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.runner.register(tool);
    }
}

#[async_trait::async_trait]
impl Tool for Review {
    fn name(&self) -> &'static str {
        "review"
    }

    fn description(&self) -> &'static str {
        "Delegate task-scoped critical evaluation to an isolated read-only agent that returns one concise, evidence-based review without changing anything."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "A self-contained review task, including the artifact under review (or its location) and the concerns to evaluate."
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

    use parking_lot::Mutex;

    use anyhow::bail;
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
        fn call<'a>(&'a self, request: ProviderRequest) -> BoxFuture<'a, AssistantMessage> {
            Box::pin(async move {
                self.requests.lock().push(request);
                self.responses
                    .lock()
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
                output: Box::pin(async { bail!("streaming is not used by review") }),
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

    fn review(provider: Arc<dyn Provider>, max_rounds: u32) -> Review {
        let directory = tempdir().unwrap();
        let (events, _receiver) = tokio::sync::broadcast::channel(16);
        Review::new(
            directory.path(),
            runtime(max_rounds, provider, events),
            WorkspaceIndexHandle::default(),
            WikiLinkIndexHandle::default(),
            reqwest::Client::new(),
            String::new(),
            &[],
        )
        .unwrap()
    }

    #[test]
    fn review_keeps_internal_tool_history_out_of_its_result() {
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
                        text: "Review: the draft is solid with two minor gaps.".to_string(),
                    }],
                    StopReason::End,
                ),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let review = review(provider.clone(), 25);

        let output = crate::agent::test_support::test_runtime()
            .block_on(review.execute(&json!({"task": "Review the draft note"})))
            .unwrap();
        assert_eq!(output, "Review: the draft is solid with two minor gaps.");

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
        assert!(!tool_names.contains(&"review"));
        assert!(!tool_names.contains(&"explore"));
        assert!(!tool_names.contains(&"edit"));
        assert!(!tool_names.contains(&"ask"));
        assert!(!tool_names.contains(&"notify"));
        assert!(!tool_names.contains(&"import_attachment"));
        assert!(!tool_names.contains(&"search_web"));
        assert_eq!(requests[1].messages.len(), 3);
        assert_eq!(
            requests[1].messages[2].role,
            crate::provider::MessageRole::Tool
        );
    }

    #[test]
    fn review_uses_its_own_configured_round_budget_then_finalizes() {
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
                        text: "Budget-limited review.".to_string(),
                    }],
                    StopReason::End,
                ),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let review = review(provider.clone(), 2);

        let output = crate::agent::test_support::test_runtime()
            .block_on(review.execute(&json!({"task": "Keep reviewing"})))
            .unwrap();
        assert_eq!(output, "Budget-limited review.");

        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].tools.is_empty());
        assert!(requests[1].tools.is_empty());
        assert_eq!(
            requests[1].messages.last().unwrap().text(),
            "Stop gathering evidence. Synthesize the issues already identified into the final self-contained review."
        );
    }

    #[test]
    fn review_requires_a_non_empty_self_contained_task() {
        let provider = Arc::new(ScriptedProvider {
            responses: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        });
        let review = review(provider.clone(), 25);

        for input in [
            json!({}),
            json!({"task": ""}),
            json!({"task": "   \n "}),
            json!({"task": 42}),
            json!({"task": null}),
        ] {
            let error = crate::agent::test_support::test_runtime()
                .block_on(review.execute(&input))
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("field task must be a non-empty string"),
                "unexpected error for {input}: {error}"
            );
        }
        assert!(provider.requests.lock().is_empty());
    }

    #[test]
    fn review_profile_follows_parent_scope_and_exposes_only_read_only_tools() {
        let provider = Arc::new(ScriptedProvider {
            responses: Mutex::new(VecDeque::from([response(
                vec![MessagePart::Text {
                    text: "Assessment: the plan covers the stated requirements.".to_string(),
                }],
                StopReason::End,
            )])),
            requests: Mutex::new(Vec::new()),
        });
        let review = review(provider.clone(), 25);

        let output = crate::agent::test_support::test_runtime()
            .block_on(review.execute(&json!({"task": "Review the migration plan"})))
            .unwrap();
        assert_eq!(
            output,
            "Assessment: the plan covers the stated requirements."
        );

        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 1);
        let system = &requests[0].system;
        assert_eq!(system.len(), 1);
        let instructions = &system[0].text;
        for contract in [
            "authoritative review scope",
            "Do not substitute a preset domain checklist",
            "never invent findings",
            "observed defects from inference and uncertainty",
            "impact on the stated task",
            "no material findings",
            "self-contained review",
        ] {
            assert!(
                instructions.contains(contract),
                "review contract missing {contract:?}"
            );
        }
        assert!(!instructions.contains("prose wording"));
        assert!(!instructions.contains("software correctness"));
        assert!(requests[0].messages[0]
            .text()
            .ends_with("\n\nReview the migration plan"));

        let tool_names = requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        for name in [
            "read",
            "list_notes",
            "grep",
            "search_files",
            "list_tags",
            "search_tag",
            "resolve_wikilink",
            "backlinks",
            "calculate",
            "load_skill",
        ] {
            assert!(tool_names.contains(&name), "missing read-only tool {name}");
        }
        for name in [
            "write",
            "edit",
            "copy",
            "move",
            "move_many",
            "rename",
            "delete",
            "mkdir",
            "remove_dir",
            "rename_tag",
            "rename_wikilink",
            "add_daily_entry",
            "open",
            "notify",
            "ask",
            "import_attachment",
            "checkout_attachment",
            "update_attachment",
            "delete_attachment",
            "http_request",
            "explore",
            "review",
        ] {
            assert!(
                !tool_names.contains(&name),
                "mutation/interaction/recursive tool {name} leaked into review"
            );
        }
        assert!(!requests[0].tools.is_empty());
        assert!(requests[0].tools.last().unwrap().cache);
    }
}
