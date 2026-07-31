    #[test]
    fn failed_tool_result_is_checkpointed_as_complete_protocol_history() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let response = json!({
            "content": [{
                "type": "tool_use",
                "id": "bad-daily",
                "name": "add_daily_entry",
                "input": {"date": "2026-07-30"}
            }],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 2}
        });
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap();
                }
            }
            let mut request = vec![0; content_length];
            reader.read_exact(&mut request).unwrap();
            let body = serde_json::to_vec(&response).unwrap();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            reader.get_mut().write_all(&body).unwrap();
            reader.get_mut().flush().unwrap();
        });

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            &storage.ai_config_path,
            format!(
                "api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\nmax_rounds = 1\n"
            ),
        )
        .unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
        user_sender
            .send(AskUserResponse::Answer("Stop".to_string()))
            .unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let agent = Agent::from_config(
            &storage.ai_config_path,
            &storage.root,
            AgentRuntime::new(
                event_sender,
                approval_receiver,
                user_receiver,
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .unwrap();

        assert_eq!(
            run_agent(&agent, "Record this", &mut AgentConversation::default()).unwrap(),
            ""
        );
        server.join().unwrap();
        let checkpoint = drain_events(&mut event_receiver)
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::ConversationUpdated(conversation) => Some(conversation),
                _ => None,
            })
            .next_back()
            .unwrap();
        assert_eq!(checkpoint.messages.len(), 3);
        let MessagePart::ToolResult(result) = &checkpoint.messages[2].parts[0] else {
            panic!("expected tool result");
        };
        assert_eq!(result.tool_use_id, "bad-daily");
        assert!(result.is_error);
        assert!(result.content.contains("missing string field content"));
    }

    #[test]
    fn stopping_at_round_limit_preserves_context_for_a_manual_followup() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responses = [
            json!({
                "content": [{
                    "type": "tool_use", "id": "notify-1", "name": "notify",
                    "input": {"message": "Still working"}
                }],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 10, "output_tokens": 3}
            }),
            json!({
                "content": [{"type": "text", "text": "Finished after follow-up"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 15, "output_tokens": 4}
            }),
        ];
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut request = vec![0; content_length];
                reader.read_exact(&mut request).unwrap();
                requests.push(serde_json::from_slice::<Value>(&request).unwrap());
                let body = serde_json::to_vec(&response).unwrap();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                reader.get_mut().write_all(&body).unwrap();
                reader.get_mut().flush().unwrap();
            }
            requests
        });

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            &storage.ai_config_path,
            format!(
                "api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\nmax_rounds = 1\n"
            ),
        )
        .unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
        user_sender
            .send(AskUserResponse::Answer("Stop".to_string()))
            .unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let agent = Agent::from_config(
            &storage.ai_config_path,
            &storage.root,
            AgentRuntime::new(
                event_sender,
                approval_receiver,
                user_receiver,
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .unwrap();
        let mut conversation = AgentConversation::default();

        assert_eq!(
            run_agent(&agent, "Start the task", &mut conversation).unwrap(),
            ""
        );
        assert_eq!(conversation.messages.len(), 3);
        assert_eq!(
            run_agent(&agent, "Please continue", &mut conversation).unwrap(),
            "Finished after follow-up"
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        let messages = requests[1]["messages"].as_array().unwrap();
        let latest_user_content = messages.last().unwrap()["content"].as_array().unwrap();
        assert!(latest_user_content
            .iter()
            .any(|part| part["type"] == "tool_result"));
        assert!(latest_user_content.iter().any(|part| {
            part["type"] == "text"
                && part["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("Please continue"))
        }));
        let events = drain_events(&mut event_receiver);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::AskUser(AskUserRequest {
                kind: AskUserKind::RoundLimit,
                ..
            })
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::Notification(message) if message.contains("request rounds")
        )));
    }

    #[test]
    fn max_token_thinking_only_response_is_automatically_continued() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responses = [
            json!({
                "content": [{"type": "thinking", "thinking": "working"}],
                "stop_reason": "max_tokens",
                "usage": {"input_tokens": 10, "output_tokens": 4096}
            }),
            json!({
                "content": [{"type": "text", "text": "Recovered final answer"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 20, "output_tokens": 5}
            }),
        ];
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                requests.push(serde_json::from_slice::<Value>(&body).unwrap());
                let body = serde_json::to_vec(&response).unwrap();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                reader.get_mut().write_all(&body).unwrap();
                reader.get_mut().flush().unwrap();
            }
            requests
        });

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            &storage.ai_config_path,
            format!(
                "api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\nmax_rounds = 4\n"
            ),
        )
        .unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, _event_receiver) = event_channel();
        let agent = Agent::from_config(
            &storage.ai_config_path,
            &storage.root,
            AgentRuntime::new(
                event_sender,
                approval_receiver,
                user_receiver,
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .unwrap();
        let mut conversation = AgentConversation::default();

        let output = run_agent(&agent, "Answer the question", &mut conversation).unwrap();
        let requests = server.join().unwrap();

        assert_eq!(output, "Recovered final answer");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_array()
                .unwrap()
                .iter()
                .any(|part| part["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("Continue from the previous response")))
        );
    }

    #[test]
    fn empty_response_diagnostics_include_stop_reason_and_block_types() {
        let diagnostic = empty_response_diagnostic(
            &StopReason::End,
            &[
                MessagePart::Thinking {
                    thinking: "...".to_string(),
                    signature: None,
                },
                MessagePart::RedactedThinking {
                    data: "...".to_string(),
                },
            ],
        );
        assert!(diagnostic.contains("stop_reason: End"));
        assert!(diagnostic.contains("redacted_thinking, thinking"));
    }

    #[test]
    fn buffered_prompts_defer_pending_tools_and_share_one_user_buffer() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(&storage.ai_config_path, TEST_MESSAGES_CONFIG).unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, _event_receiver) = event_channel();
        let input_buffer = Arc::new(Mutex::new(vec![
            "Use the newer file.".to_string(),
            "Also preserve the heading.".to_string(),
        ]));
        let agent = Agent::from_config(
            &storage.ai_config_path,
            &storage.root,
            AgentRuntime::new(
                event_sender,
                approval_receiver,
                user_receiver,
                input_buffer.clone(),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .unwrap();
        let calls = [ToolCall {
            id: "tool-1".to_string(),
            name: "read_file".to_string(),
            input: json!({"path": "data/missing.md"}),
        }];

        let results =
            completed_tool_results(agent.execute_tool_batch(&calls, &HashMap::new()).unwrap());

        assert_eq!(results.len(), 2);
        let MessagePart::ToolResult(result) = &results[0].parts[0] else {
            panic!("expected tool result");
        };
        assert_eq!(result.tool_use_id, "tool-1");
        assert!(result.is_error);
        let buffered = results[1].text();
        assert!(buffered.contains("Use the newer file."));
        assert!(buffered.contains("Also preserve the heading."));
        assert!(buffered.contains("Current local date and time:"));
        assert!(input_buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn buffered_prompt_starts_a_fresh_round_budget() {
        struct BufferFollowup(Arc<Mutex<Vec<String>>>);

        #[async_trait::async_trait]
        impl Tool for BufferFollowup {
            fn name(&self) -> &'static str {
                "buffer_followup"
            }

            fn description(&self) -> &'static str {
                "Test tool"
            }

            fn input_schema(&self) -> Value {
                json!({"type": "object", "additionalProperties": false})
            }

            async fn execute(&self, _input: &Value) -> Result<String> {
                self.0
                    .lock()
                    .unwrap()
                    .push("New user turn".to_string());
                Ok("buffered".to_string())
            }
        }

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responses = [
            json!({
                "content": [{
                    "type": "tool_use", "id": "notify", "name": "notify",
                    "input": {"message": "round one"}
                }],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 10, "output_tokens": 2}
            }),
            json!({
                "content": [{
                    "type": "tool_use", "id": "buffer", "name": "buffer_followup",
                    "input": {}
                }],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 12, "output_tokens": 2}
            }),
            json!({
                "content": [{"type": "text", "text": "Handled the new turn"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 14, "output_tokens": 3}
            }),
        ];
        let server = std::thread::spawn(move || {
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut request = vec![0; content_length];
                reader.read_exact(&mut request).unwrap();
                let body = serde_json::to_vec(&response).unwrap();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                reader.get_mut().write_all(&body).unwrap();
                reader.get_mut().flush().unwrap();
            }
        });

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            &storage.ai_config_path,
            format!(
                "api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\nmax_rounds = 2\n"
            ),
        )
        .unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, mut event_receiver) = event_channel();
        let input_buffer = Arc::new(Mutex::new(Vec::new()));
        let mut agent = Agent::from_config(
            &storage.ai_config_path,
            &storage.root,
            AgentRuntime::new(
                event_sender,
                approval_receiver,
                user_receiver,
                input_buffer.clone(),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .unwrap();
        agent.register(BufferFollowup(input_buffer));

        let output = run_agent(&agent, "Original turn", &mut AgentConversation::default()).unwrap();
        server.join().unwrap();
        assert_eq!(output, "Handled the new turn");
        let rounds = drain_events(&mut event_receiver)
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::Round { current, limit } => Some((current, limit)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(rounds, vec![(1, 2), (2, 2), (1, 2)]);
    }

    #[test]
    fn system_prompt_describes_ask_user_without_runtime_protocol_details() {
        let prompt = system_prompt(Path::new("/tmp/nole"), false, "", "");
        assert!(prompt.contains("Use ask_user"));
        assert!(!prompt.contains("conversation is multi-turn"));
        assert!(!prompt.contains("buffers it and delivers it before pending tool calls"));
        assert!(!prompt.contains("Final text is not saved automatically"));
    }

    #[test]
    fn system_prompt_describes_current_mbdown_and_workspace_behavior() {
        let prompt = system_prompt(Path::new("/tmp/nole"), false, "", "");
        assert!(prompt.contains("#tag"));
        assert!(prompt.contains("[[wikilink]]"));
        assert!(prompt.contains("![[file]]"));
        assert!(prompt.contains("Fenced mermaid code blocks render locally"));
        assert!(prompt.contains("must start a source line or follow whitespace"));
        assert!(prompt.contains("png, jpg, jpeg, gif, and webp"));
        assert!(prompt.contains("public or private-network hosts"));
        assert!(prompt.contains("relative to the containing note"));
        assert!(prompt.contains("Nole root when emitted in the Agent panel"));
        assert!(prompt.contains("absolute paths may point outside Nole"));
        assert!(prompt.contains("A box border only accepts `single` or `none`"));
        assert!(prompt.contains("archived daily and regular Markdown files"));
        assert!(prompt.contains("create them here by default"));
        assert!(prompt.contains("themes/: editable TOML theme definitions"));
        assert!(
            prompt.contains("template.mb: editable content used only by Create note from template")
        );
        assert!(prompt.contains("ordinary New note does not use it"));
        assert!(prompt.contains("config/: application-managed configuration"));
        assert!(prompt.contains("config/settings.toml: read-only application settings"));
        assert!(prompt
            .contains("config/agent-session.json: application-managed persisted Agent session"));
        assert!(prompt.contains("config/ai.toml: private credentials and AI settings"));
        assert!(prompt.contains("never read it or expose its contents"));
        assert!(prompt.contains("Use list_directory on daily/ to discover dates"));
        assert!(prompt.contains("daily/: ordinary Markdown files named YYYY-MM-DD.md"));
        assert!(prompt.contains("Existing daily Markdown files may be read, edited, or deleted"));
        assert!(!prompt.contains("latest unchanged read_file snapshot"));
        assert!(!prompt.contains("approval"));
        assert!(!prompt.contains("bypass"));
        assert!(!prompt.contains("prevalidated"));
        assert!(prompt.contains("Use web_fetch when you already have a URL"));
        assert!(prompt.contains("Delegate broad, multi-step exploration"));
        assert!(prompt.contains("internal work stays out of this conversation"));
        assert!(prompt.contains("Use direct read/search tools only for narrow lookups"));
        assert!(prompt.contains("Use open_file"));
        assert!(!prompt.contains("Generic file tools cannot operate in daily/ or config/"));
    }

    #[test]
    fn system_prompt_appends_project_instructions_then_memory() {
        let prompt = system_prompt(
            Path::new("/tmp/nole"),
            false,
            "PROJECT INSTRUCTION\nsecond line",
            "MEMORY CONTENT\nlast line",
        );
        let project = prompt.find("PROJECT INSTRUCTION").unwrap();
        let memory = prompt.find("MEMORY CONTENT").unwrap();
        assert!(project < memory);
        assert!(prompt.ends_with("MEMORY CONTENT\nlast line"));
    }

    #[test]
    fn system_prompt_cache_sections_isolate_dynamic_memory() {
        let first = system_prompt_sections(
            Path::new("/tmp/nole"),
            false,
            "PROJECT INSTRUCTION",
            &[],
            "first memory",
        );
        let second = system_prompt_sections(
            Path::new("/tmp/nole"),
            false,
            "PROJECT INSTRUCTION",
            &[],
            "second memory",
        );

        assert_eq!(first.len(), 4);
        assert_eq!(first[..3], second[..3]);
        assert_ne!(first[3], second[3]);
    }

    #[test]
    fn tavily_tool_and_prompt_guidance_are_registered_only_with_a_key() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();

        let make_agent = |tavily_api_key: &str| {
            fs::write(
                &storage.ai_config_path,
                format!(
                    "api_format = \"messages\"\napi_key = \"anthropic-test\"\ntavily_api_key = \"{tavily_api_key}\"\nmodel = \"test-model\"\nbase_url = \"https://api.anthropic.com\"\n"
                ),
            )
            .unwrap();
            let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
            let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
            let (event_sender, _event_receiver) = event_channel();
            Agent::from_config(
                &storage.ai_config_path,
                &storage.root,
                AgentRuntime::new(
                    event_sender,
                    approval_receiver,
                    user_receiver,
                    Arc::new(Mutex::new(Vec::new())),
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicBool::new(false)),
                ),
            )
            .unwrap()
        };

        let without_key = make_agent("");
        assert!(!without_key.tools.contains_key("web_search"));
        assert!(!serde_json::to_string(&without_key.system)
            .unwrap()
            .contains("web_search"));
        assert!(without_key.tools.contains_key("create_file"));
        assert!(without_key.tools.contains_key("edit_file"));
        assert!(without_key.tools.contains_key("add_daily_entry"));
        assert!(without_key.tools.contains_key("explore"));
        assert!(without_key.tools.contains_key("load_skill"));
        let serialized_system = serde_json::to_string(&without_key.system).unwrap();
        assert!(serialized_system.contains("Available skills"));
        assert!(serialized_system.contains("create-skill"));
        assert!(!serialized_system.contains("# Create a Skill"));
        for removed in ["update_file", "read_daily", "update_daily", "append_daily"] {
            assert!(!without_key.tools.contains_key(removed));
        }
        assert!(without_key.system.last().unwrap().cache);
        assert_eq!(without_key.system.len(), 4);
        assert!(without_key.system.iter().all(|block| block.cache));
        assert!(without_key.definitions.last().unwrap().cache);
        for definition in &without_key.definitions {
            for detail in ["approval", "bypass", "prevalidated", "source spans", "Tavily"] {
                assert!(
                    !definition.description.contains(detail),
                    "tool {} exposes runtime detail {detail:?}",
                    definition.name
                );
            }
        }
        let second_without_key = make_agent("");
        assert_eq!(without_key.definitions, second_without_key.definitions);

        let with_key = make_agent("tvly-test");
        assert!(with_key.tools.contains_key("web_search"));
        assert!(serde_json::to_string(&with_key.system)
            .unwrap()
            .contains("web_search"));
    }

    #[test]
    fn tavily_response_is_reduced_to_agent_relevant_fields() {
        let response = json!({
            "answer": "A concise answer",
            "request_id": "private-noise",
            "results": [{
                "title": "Result",
                "url": "https://example.test",
                "content": "Useful snippet",
                "score": 0.9,
                "published_date": "2026-07-27",
                "raw_content": "large omitted content"
            }]
        });
        let compact: Value =
            serde_json::from_str(&compact_tavily_response("query", &response).unwrap()).unwrap();
        assert_eq!(compact["query"], "query");
        assert_eq!(compact["answer"], "A concise answer");
        assert_eq!(compact["results"][0]["title"], "Result");
        assert!(compact.get("request_id").is_none());
        assert!(compact["results"][0].get("raw_content").is_none());
    }

    #[test]
    fn tavily_domain_filters_are_optional_and_forwarded_when_present() {
        let (_, unfiltered) = tavily_search_request(
            "tvly-test",
            &json!({
                "query": "Rust terminal UI"
            }),
        )
        .unwrap();
        assert!(unfiltered.get("include_domains").is_none());
        assert!(unfiltered.get("exclude_domains").is_none());

        let (_, filtered) = tavily_search_request(
            "tvly-test",
            &json!({
                "query": "Rust terminal UI",
                "include_domains": [" docs.rs ", "ratatui.rs"],
                "exclude_domains": ["example.com"]
            }),
        )
        .unwrap();
        assert_eq!(
            filtered["include_domains"],
            json!(["docs.rs", "ratatui.rs"])
        );
        assert_eq!(filtered["exclude_domains"], json!(["example.com"]));

        let schema = WebSearch {
            client: Client::new(),
            api_key: "tvly-test".to_string(),
        }
        .input_schema();
        assert_eq!(schema["required"], json!(["query"]));
        assert_eq!(
            schema["properties"]["include_domains"]["maxItems"],
            MAX_WEB_SEARCH_DOMAINS
        );
        assert_eq!(
            schema["properties"]["exclude_domains"]["maxItems"],
            MAX_WEB_SEARCH_DOMAINS
        );
    }

    #[test]
    fn tavily_domain_filters_reject_invalid_arrays() {
        for input in [
            json!({"query": "query", "include_domains": []}),
            json!({"query": "query", "include_domains": [""]}),
            json!({"query": "query", "exclude_domains": "example.com"}),
            json!({"query": "query", "exclude_domains": [42]}),
        ] {
            assert!(tavily_search_request("tvly-test", &input).is_err());
        }

        let too_many = vec!["example.com"; MAX_WEB_SEARCH_DOMAINS + 1];
        assert!(tavily_search_request(
            "tvly-test",
            &json!({"query": "query", "include_domains": too_many})
        )
        .is_err());
    }

    #[test]
    fn web_fetch_converts_html_to_markdown_and_preserves_plain_text() {
        let html = br#"<!doctype html>
            <html><head><style>body { color: red; }</style></head>
            <body>
                <h1>Hello</h1>
                <p>A <strong>small</strong> page with <a href="/docs">docs</a>.</p>
                <script>alert('ignored')</script>
            </body></html>"#
            .to_vec();
        let markdown = web_fetch_content(Some("text/html; charset=utf-8"), html).unwrap();
        assert!(markdown.contains("# Hello"));
        assert!(markdown.contains("A **small** page with [docs](/docs)."));
        assert!(!markdown.contains("color: red"));
        assert!(!markdown.contains("alert"));

        let plain = b"literal <strong>text</strong>".to_vec();
        assert_eq!(
            web_fetch_content(Some("text/plain"), plain).unwrap(),
            "literal <strong>text</strong>"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn web_fetch_runs_inside_the_agent_runtime_without_nested_runtime_panics() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            let body = "runtime-safe response";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let tool = WebFetch {
            client: build_http_client().unwrap(),
        };
        let output = tool
            .execute(&json!({"url": format!("http://{address}")}))
            .await
            .unwrap();

        server.join().unwrap();
        assert_eq!(output, "runtime-safe response");
    }

    #[test]
    fn file_edit_requires_read_and_create_file_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::write(directory.path().join("data/note.md"), "old\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let edit = EditFile::new(directory.path(), bypass_gate(), reads.clone()).unwrap();
        let input = json!({
            "path": "data/note.md",
            "edits": [{"operation": "replace", "start_line": 0, "end_line": 0, "lines": ["new"]}]
        });
        assert!(edit.execute(&input).returns_err());

        let read = ReadFile::new(directory.path(), reads).unwrap();
        read.execute(&json!({"path": "data/note.md"})).unwrap();
        edit.execute(&input).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("data/note.md")).unwrap(),
            "new\n"
        );
        assert!(edit
            .execute(&json!({
                "path": "data/note.md",
                "edits": [{"operation": "replace", "start_line": 0, "end_line": 0, "lines": ["again"]}]
            }))
            .returns_err());

        let create = CreateFile::new(directory.path()).unwrap();
        assert!(create
            .execute(&json!({"path": "data/note.md", "content": "overwrite"}))
            .returns_err());
    }

    #[test]
    fn markdown_writes_fail_mbdown_validation_before_mutation_or_approval() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let invalid = "[box border=double]\nbody\n[/box]\n";

        let create = CreateFile::new(directory.path()).unwrap();
        let error = create
            .execute(&json!({"path": "data/invalid.md", "content": invalid}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("box border must be single or none"));
        assert!(!storage.data_dir.join("invalid.md").exists());

        let mermaid_error = create
            .execute(&json!({
                "path": "data/invalid-mermaid.md",
                "content": "# Diagram\n\n```mermaid\nclassDiagram\nclass Agent~T~ {}\n```\n"
            }))
            .unwrap_err()
            .to_string();
        assert!(mermaid_error.contains("Mermaid validation failed at line 3"));
        assert!(mermaid_error.contains("generics not yet supported"));
        assert!(!storage.data_dir.join("invalid-mermaid.md").exists());

        create
            .execute(&json!({"path": "data/plain.txt", "content": invalid}))
            .unwrap();
        assert_eq!(fs::read_to_string(storage.data_dir.join("plain.txt")).unwrap(), invalid);

        let path = storage.data_dir.join("note.md");
        fs::write(&path, "valid\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "data/note.md"}))
            .unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let (_decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let edit = EditFile::new(
            directory.path(),
            ApprovalGate {
                bypass: Arc::new(AtomicBool::new(false)),
                cancelled: Arc::new(AtomicBool::new(false)),
                events: event_sender,
                decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
            },
            reads,
        )
        .unwrap();
        let error = test_runtime()
            .block_on(edit.execute(&json!({
                "path": "data/note.md",
                "edits": [{
                    "operation": "replace",
                    "start_line": 0,
                    "end_line": 0,
                    "lines": ["[box border=double]", "body", "[/box]"]
                }]
            })))
            .unwrap_err()
            .to_string();
        assert!(error.contains("box border must be single or none"));
        assert!(event_receiver.try_recv().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "valid\n");

        let error = test_runtime()
            .block_on(edit.execute(&json!({
                "path": "data/note.md",
                "edits": [{
                    "operation": "replace",
                    "start_line": 0,
                    "end_line": 0,
                    "lines": ["```mermaid", "classDiagram", "class Agent~T~ {}", "```"]
                }]
            })))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Mermaid validation failed at line 1"));
        assert!(error.contains("generics not yet supported"));
        assert!(event_receiver.try_recv().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "valid\n");
    }

    #[test]
    fn skill_writes_validate_complete_documents_before_any_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let valid = "---\ndescription: Original workflow\n---\n\n# Steps\n";
        let invalid = "# Missing front matter\n";

        let create = CreateFile::new(directory.path()).unwrap();
        let create_error = create
            .execute(&json!({
                "path": "skills/broken.md",
                "content": invalid
            }))
            .unwrap_err()
            .to_string();
        assert!(create_error.contains("front matter"));
        assert!(!storage.skills_dir.join("broken.md").exists());

        create
            .execute(&json!({
                "path": "skills/existing.md",
                "content": valid
            }))
            .unwrap();
        let existing = storage.skills_dir.join("existing.md");
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "skills/existing.md"}))
            .unwrap();
        let edit = EditFile::new(directory.path(), bypass_gate(), reads).unwrap();
        let edit_error = edit
            .execute(&json!({
                "path": "skills/existing.md",
                "edits": [{
                    "operation": "replace",
                    "start_line": 1,
                    "end_line": 1,
                    "lines": ["desc: Wrong field"]
                }]
            }))
            .unwrap_err()
            .to_string();
        assert!(edit_error.contains("description"));
        assert_eq!(fs::read_to_string(&existing).unwrap(), valid);

        let outside = tempfile::tempdir().unwrap();
        let copy_source = outside.path().join("copied.md");
        let move_source = outside.path().join("moved.md");
        fs::write(&copy_source, invalid).unwrap();
        fs::write(&move_source, invalid).unwrap();
        assert!(CopyFile::new(directory.path())
            .unwrap()
            .execute(&json!({
                "source": copy_source,
                "destination": "skills/copied.md"
            }))
            .returns_err());
        assert!(copy_source.exists());
        assert!(!storage.skills_dir.join("copied.md").exists());

        let (event_sender, _) = event_channel();
        assert!(MoveFile::new(directory.path(), event_sender.clone())
            .unwrap()
            .execute(&json!({
                "source": move_source,
                "destination": "skills/moved.md"
            }))
            .returns_err());
        assert!(move_source.exists());
        assert!(!storage.skills_dir.join("moved.md").exists());

        let batch_valid = outside.path().join("batch-valid.md");
        let batch_invalid = outside.path().join("batch-invalid.md");
        fs::write(&batch_valid, valid).unwrap();
        fs::write(&batch_invalid, invalid).unwrap();
        let (batch_events, _) = event_channel();
        assert!(MoveFiles::new(directory.path(), batch_events)
            .unwrap()
            .execute(&json!({
                "sources": [batch_valid, batch_invalid],
                "destination_directory": "skills"
            }))
            .returns_err());
        assert!(batch_valid.exists());
        assert!(batch_invalid.exists());
        assert!(!storage.skills_dir.join("batch-valid.md").exists());
        assert!(!storage.skills_dir.join("batch-invalid.md").exists());

        assert!(RenameFile::new(directory.path(), event_sender)
            .unwrap()
            .execute(&json!({
                "path": "skills/existing.md",
                "new_name": "Invalid.md"
            }))
            .returns_err());
        assert!(existing.exists());
    }

    #[test]
    fn file_edit_replace_uses_an_inclusive_end_line() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("range.md");
        fs::write(&path, "zero\none\ntwo\nthree\nfour\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "data/range.md"}))
            .unwrap();
        let edit = EditFile::new(directory.path(), bypass_gate(), reads).unwrap();

        edit.execute(&json!({
            "path": "data/range.md",
            "edits": [{
                "operation": "replace",
                "start_line": 1,
                "end_line": 3,
                "lines": ["replacement"]
            }]
        }))
        .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "zero\nreplacement\nfour\n"
        );
        assert!(parse_line_edits(&json!({
            "edits": [{"start_line": 1, "end_line": 4, "lines": ["legacy"]}]
        }))
        .unwrap_err()
        .to_string()
        .contains("operation must be 'replace' or 'insert'"));
    }

    #[test]
    fn file_edit_requires_only_changed_lines_and_insertion_anchors() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        let path = directory.path().join("data/large.md");
        let original = (0..20)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(&path, &original).unwrap();

        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let edit = EditFile::new(directory.path(), bypass_gate(), reads).unwrap();
        read.execute(&json!({"path": "data/large.md", "offset": 10, "limit": 1}))
            .unwrap();
        edit.execute(&json!({
            "path": "data/large.md",
            "edits": [{"operation": "replace", "start_line": 10, "end_line": 10, "lines": ["changed 10"]}]
        }))
        .unwrap();

        fs::write(&path, &original).unwrap();
        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let edit = EditFile::new(directory.path(), bypass_gate(), reads).unwrap();
        read.execute(&json!({"path": "data/large.md", "offset": 10, "limit": 1}))
            .unwrap();
        let error = edit
            .execute(&json!({
                "path": "data/large.md",
                "edits": [{"operation": "replace", "start_line": 2, "end_line": 2, "lines": ["changed 2"]}]
            }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("changed zero-based lines 2 through 2"));

        fs::write(&path, "first\nsecond\nthird\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let edit = EditFile::new(directory.path(), bypass_gate(), reads).unwrap();
        read.execute(&json!({"path": "data/large.md", "offset": 1, "limit": 1}))
            .unwrap();
        let error = edit
            .execute(&json!({
                "path": "data/large.md",
                "edits": [{"operation": "insert", "line": 1, "lines": ["inserted"]}]
            }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("insertion anchor lines 0..2"));
    }

    #[test]
    fn file_edit_rejects_lines_with_embedded_newlines() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("table.md");
        let original = "| model | score |\n| --- | --- |\n| MM26 | 0.65 |\n";
        fs::write(&path, original).unwrap();
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "data/table.md"}))
            .unwrap();
        let edit = EditFile::new(directory.path(), bypass_gate(), reads).unwrap();

        let error = edit
            .execute(&json!({
                "path": "data/table.md",
                "edits": [{
                    "operation": "insert",
                    "line": 2,
                    "lines": ["| SAC | 0.639 |\n| MM26"]
                }]
            }))
            .unwrap_err()
            .to_string();

        assert!(error.contains("edit lines must not contain line-ending characters"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);

        edit.execute(&json!({
                "path": "data/table.md",
                "edits": [{
                    "operation": "insert",
                    "line": 2,
                    "lines": ["| SAC | 0.639 |"]
                }]
        }))
        .unwrap();
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "| model | score |\n| --- | --- |\n| SAC | 0.639 |\n| MM26 | 0.65 |\n"
        );

        let eof_path = storage.data_dir.join("eof.md");
        fs::write(&eof_path, "old").unwrap();
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "data/eof.md"}))
            .unwrap();
        EditFile::new(directory.path(), bypass_gate(), reads)
            .unwrap()
            .execute(&json!({
                "path": "data/eof.md",
                "edits": [{"operation": "insert", "line": 1, "lines": ["new"]}]
            }))
            .unwrap();
        assert_eq!(fs::read_to_string(&eof_path).unwrap(), "old\nnew\n");

        let crlf_path = storage.data_dir.join("crlf.md");
        fs::write(&crlf_path, "old\r\nvalue\r\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "data/crlf.md"}))
            .unwrap();
        EditFile::new(directory.path(), bypass_gate(), reads)
            .unwrap()
            .execute(&json!({
                "path": "data/crlf.md",
                "edits": [{"operation": "replace", "start_line": 1, "end_line": 1, "lines": ["changed"]}]
            }))
            .unwrap();
        assert_eq!(fs::read_to_string(crlf_path).unwrap(), "old\r\nchanged\r\n");
    }

    #[test]
    fn daily_markdown_uses_generic_read_edit_and_delete_tools() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage.append_daily("2026-07-27", "old").unwrap();
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "daily/2026-07-27.md"}))
            .unwrap();
        EditFile::new(directory.path(), bypass_gate(), reads)
            .unwrap()
            .execute(&json!({
                "path": "daily/2026-07-27.md",
                "edits": [{"operation": "replace", "start_line": 0, "end_line": 0, "lines": ["edited"]}]
            }))
            .unwrap();
        assert_eq!(storage.load_daily_notes().unwrap()[0].body, "edited");

        DeleteFile::new(directory.path(), bypass_gate())
            .unwrap()
            .execute(&json!({"path": "daily/2026-07-27.md"}))
            .unwrap();
        assert!(!storage.daily_dir.join("2026-07-27.md").exists());
        assert!(CreateFile::new(directory.path())
            .unwrap()
            .execute(&json!({
                "path": "daily/2026-07-27.md",
                "content": "must use add_daily_entry"
            }))
            .returns_err());
    }

    #[test]
    fn add_daily_entry_creates_and_appends_without_approval() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let add = AddDailyEntry::new(directory.path()).unwrap();

        add.execute(&json!({"date": "2026-07-27", "content": "first"}))
            .unwrap();
        add.execute(&json!({"date": "2026-07-27", "content": "second"}))
            .unwrap();

        assert_eq!(
            storage.load_daily_notes().unwrap()[0].body,
            "first\n\nsecond"
        );

        add.execute(&json!({"content": "today"})).unwrap();
        assert_eq!(
            storage
                .read_daily_by_date(&Local::now().date_naive().to_string())
                .unwrap()
                .body,
            "today"
        );

        let schema = add.input_schema();
        assert_eq!(schema["required"], json!(["content"]));
        assert!(schema["properties"]["date"]["description"]
            .as_str()
            .unwrap()
            .contains("YYYY-MM-DD"));

        add.execute(&json!({"date": "2026-07-28", "content": "existing"}))
            .unwrap();
        let path = storage.daily_file_path("2026-07-28").unwrap();
        let error = add
            .execute(&json!({
                "date": "2026-07-28",
                "content": "[box border=double]\ninvalid\n[/box]"
            }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("box border must be single or none"));
        assert_eq!(fs::read_to_string(path).unwrap(), "existing\n");
    }

    #[test]
    fn edit_waits_for_diff_approval() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::write(directory.path().join("data/note.md"), "old\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        ReadFile::new(directory.path(), reads.clone())
            .unwrap()
            .execute(&json!({"path": "data/note.md"}))
            .unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let edit = EditFile::new(directory.path(), gate, reads).unwrap();
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(edit.execute(&json!({
                "path": "data/note.md",
                "edits": [{"operation": "replace", "start_line": 0, "end_line": 0, "lines": ["new"]}]
            })))
        });

        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert!(request.diff.contains("-old"));
        assert!(request.diff.contains("+new"));
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("data/note.md")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn notify_tool_emits_a_tui_notification_event() {
        let (sender, mut receiver) = event_channel();
        let tool = Notify { events: sender };
        tool.execute(&json!({"message": "Work complete"})).unwrap();
        let AgentEvent::Notification(message) = receiver.blocking_recv().unwrap() else {
            panic!("expected notification event");
        };
        assert_eq!(message, "Work complete");
    }

    #[test]
    fn open_file_tool_emits_a_managed_note_request() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let note = storage.data_dir.join("Guide.md");
        fs::write(&note, "# Guide\n").unwrap();
        let unsupported = storage.data_dir.join("raw.txt");
        fs::write(&unsupported, "raw\n").unwrap();
        let (sender, mut receiver) = event_channel();
        let tool = OpenFile::new(directory.path(), sender).unwrap();

        let result = tool.execute(&json!({"path": "data/Guide.md"})).unwrap();
        assert!(result.contains("Guide.md"));
        let AgentEvent::OpenFile(opened) = receiver.blocking_recv().unwrap() else {
            panic!("expected open-file event");
        };
        assert_eq!(opened, fs::canonicalize(note).unwrap());
        let daily = storage.daily_dir.join("2026-07-27.md");
        fs::write(&daily, "daily\n").unwrap();
        tool.execute(&json!({"path": "daily/2026-07-27.md"}))
            .unwrap();
        let AgentEvent::OpenFile(opened) = receiver.blocking_recv().unwrap() else {
            panic!("expected daily open-file event");
        };
        assert_eq!(opened, fs::canonicalize(daily).unwrap());
        assert!(tool
            .execute(&json!({"path": "data/raw.txt"}))
            .unwrap_err()
            .to_string()
            .contains("managed .md or .mb"));
    }

    #[test]
    fn ask_user_waits_for_and_returns_the_tui_answer() {
        let (event_sender, mut event_receiver) = event_channel();
        let (response_sender, response_receiver) = tokio::sync::mpsc::unbounded_channel();
        let tool = AskUser {
            events: event_sender,
            responses: Arc::new(tokio::sync::Mutex::new(response_receiver)),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(tool.execute(&json!({
                "question": "Which format?",
                "options": ["Markdown", "MBDown"]
            })))
        });

        let AgentEvent::AskUser(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected user question");
        };
        assert_eq!(request.question, "Which format?");
        assert_eq!(request.options, ["Markdown", "MBDown"]);
        response_sender
            .send(AskUserResponse::Answer("MBDown".to_string()))
            .unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), "MBDown");
    }

    #[test]
    fn tag_tools_query_the_shared_index_with_exact_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(storage.data_dir.join("Tags.md"), "#rust #rust\n#rustlang\n").unwrap();
        let handle = WorkspaceIndexHandle::default();
        handle.replace(crate::workspace_index::WorkspaceIndex::build(&storage));

        let listed: Value = serde_json::from_str(
            &ListTags::new(handle.clone())
                .execute(&json!({"query": "rust"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(listed["total"], 2);
        assert_eq!(listed["entries"][0]["tag"], "rust");
        assert_eq!(listed["entries"][0]["mentions"], 2);

        let searched: Value = serde_json::from_str(
            &SearchTag::new(directory.path(), handle)
                .unwrap()
                .execute(&json!({"tag": "#rust"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(searched["total"], 1);
        assert_eq!(searched["entries"][0]["line"], 0);
        assert!(searched["entries"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("#rust #rust"));
    }

    #[test]
    fn rename_tag_tool_requires_approval_and_updates_the_shared_index() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("Tags.md");
        fs::write(&path, "#old\n").unwrap();
        let handle = WorkspaceIndexHandle::default();
        handle.replace(crate::workspace_index::WorkspaceIndex::build(&storage));
        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let tool = RenameTag::new(directory.path(), handle.clone(), gate).unwrap();
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(tool.execute(&json!({"from": "old", "to": "new"})))
        });

        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert!(request.diff.contains("-#old"));
        assert!(request.diff.contains("+#new"));
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        let result: Value = serde_json::from_str(&worker.join().unwrap().unwrap()).unwrap();
        assert_eq!(result["documents"], 1);
        assert_eq!(fs::read_to_string(path).unwrap(), "#new\n");
        assert!(handle
            .with_index(|index| index.exact_tag_hits("old", None).is_empty())
            .unwrap());
        assert_eq!(
            handle
                .with_index(|index| index.exact_tag_hits("new", None).len())
                .unwrap(),
            1
        );
    }
