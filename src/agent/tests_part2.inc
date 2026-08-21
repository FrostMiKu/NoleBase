    #[test]
    fn post_write_diagnostic_is_returned_before_the_round_limit_can_stop_repair() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responses = [
            json!({
                "content": [{
                    "type": "tool_use",
                    "id": "bad-write",
                    "name": "write",
                    "input": {
                        "path": "data/invalid.md",
                        "content": "[box border=double]\nbody\n[/box]\n"
                    }
                }],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 10, "output_tokens": 2}
            }),
            json!({
                "content": [{"type": "text", "text": "Received the persisted-file diagnostic"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 14, "output_tokens": 6}
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
                Arc::new(AtomicU8::new(PermissionMode::Auto.code())),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .unwrap();

        assert_eq!(
            run_agent(&agent, "Write the note", &mut AgentConversation::default()).unwrap(),
            "Received the persisted-file diagnostic"
        );
        let requests = server.join().unwrap();
        let messages = requests[1]["messages"].as_array().unwrap();
        let result = &messages[2]["content"][0];
        assert_eq!(result["tool_use_id"], "bad-write");
        assert_eq!(result["is_error"], false);
        assert!(result["content"]
            .as_str()
            .unwrap()
            .contains("MBDown validation warning"));
        assert_eq!(
            fs::read_to_string(storage.data_dir.join("invalid.md")).unwrap(),
            "[box border=double]\nbody\n[/box]\n"
        );
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
                Arc::new(AtomicU8::new(PermissionMode::Approve.code())),
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
                Arc::new(AtomicU8::new(PermissionMode::Approve.code())),
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
    fn end_turn_thinking_only_response_is_automatically_retried() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responses = [
            json!({
                "content": [{"type": "thinking", "thinking": "working"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 8}
            }),
            json!({
                "content": [{"type": "text", "text": "Recovered without user intervention"}],
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
                Arc::new(AtomicU8::new(PermissionMode::Approve.code())),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .unwrap();
        let mut conversation = AgentConversation::default();

        let output = run_agent(&agent, "Answer the question", &mut conversation).unwrap();
        let requests = server.join().unwrap();

        assert_eq!(output, "Recovered without user intervention");
        assert_eq!(requests.len(), 2);
        let retry_messages = requests[1]["messages"].as_array().unwrap();
        assert!(retry_messages.iter().all(|message| {
            message["role"] != "assistant"
                || message["content"].as_array().is_some_and(|content| {
                    content.iter().any(|part| {
                        part["type"] == "text" || part["type"] == "tool_use"
                    })
                })
        }));
        assert!(retry_messages.last().unwrap()["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["text"]
                .as_str()
                .is_some_and(|text| text.contains("Provide a non-empty final answer"))));
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
                Arc::new(AtomicU8::new(PermissionMode::Approve.code())),
                Arc::new(AtomicBool::new(false)),
            ),
        )
        .unwrap();
        let calls = [ToolCall {
            id: "tool-1".to_string(),
            name: "read".to_string(),
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
                Arc::new(AtomicU8::new(PermissionMode::Approve.code())),
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
    fn system_prompt_stays_compact() {
        let prompt = system_prompt(Path::new("/tmp/nole"), "", "");
        assert!(prompt.len() < 1_750, "system prompt grew to {} bytes", prompt.len());
    }

    #[test]
    fn system_prompt_appends_project_instructions_then_memory() {
        let prompt = system_prompt(
            Path::new("/tmp/nole"),
            "PROJECT INSTRUCTION\nsecond line",
            "MEMORY CONTENT\nlast line",
        );
        let project = prompt.find("PROJECT INSTRUCTION").unwrap();
        let memory = prompt.find("MEMORY CONTENT").unwrap();
        assert!(project < memory);
        assert!(prompt.ends_with("MEMORY CONTENT\nlast line"));
    }

    #[test]
    fn system_prompt_explains_persistent_memory_maintenance() {
        let prompt = system_prompt(Path::new("/tmp/nole"), "", "");
        assert!(prompt.contains("MEMORY.md"));
        assert!(prompt.contains("persistent Agent memory"));
        assert!(prompt.contains("durable user preferences"));
        assert!(prompt.contains("Do not store secrets"));
    }

    #[test]
    fn system_prompt_prefers_built_in_tools_before_shells() {
        let prompt = system_prompt(Path::new("/tmp/nole"), "", "");
        assert!(prompt.contains(
            "Prefer purpose-built tools because they provide structured inputs, path protections, and change previews. Use `shell` or `terminal` only when the built-in tools cannot complete the task effectively."
        ));
        assert!(!prompt.contains("every shell command"));
        assert!(!prompt.contains("terminal input"));
    }

    #[test]
    fn system_prompt_cache_sections_isolate_dynamic_memory() {
        let first = system_prompt_sections(
            Path::new("/tmp/nole"),
            "PROJECT INSTRUCTION",
            &[],
            "first memory",
        );
        let second = system_prompt_sections(
            Path::new("/tmp/nole"),
            "PROJECT INSTRUCTION",
            &[],
            "second memory",
        );

        assert_eq!(first.len(), 4);
        assert_eq!(first[..3], second[..3]);
        assert_ne!(first[3], second[3]);
        assert_eq!(
            first.iter().map(|block| block.cache).collect::<Vec<_>>(),
            vec![true, false, true, false]
        );
    }

    #[test]
    fn tavily_tool_is_registered_only_with_a_key_without_prompt_duplication() {
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
                    Arc::new(AtomicU8::new(PermissionMode::Approve.code())),
                    Arc::new(AtomicBool::new(false)),
                ),
            )
            .unwrap()
        };

        let without_key = make_agent("");
        assert!(!without_key.tools.contains_key("search_web"));
        assert!(!serde_json::to_string(&without_key.system)
            .unwrap()
            .contains("search_web"));
        for name in [
            "http_request",
            "write",
            "edit",
            "copy",
            "export_file",
            "move",
            "move_many",
            "rename",
            "delete",
            "open",
            "ask",
            "notify",
            "add_daily_entry",
            "explore",
            "review",
            "calculate",
            "load_skill",
        ] {
            assert!(without_key.tools.contains_key(name), "missing tool {name}");
        }
        let serialized_system = serde_json::to_string(&without_key.system).unwrap();
        assert!(serialized_system.contains("Available skills"));
        assert!(serialized_system.contains("create-skill"));
        assert!(!serialized_system.contains("# Create a Skill"));
        for removed in [
            "create_file",
            "edit_file",
            "copy_file",
            "move_file",
            "move_files",
            "rename_file",
            "delete_file",
            "open_file",
            "ask_user",
            "web_search",
            "update_file",
            "read_daily",
            "update_daily",
            "append_daily",
        ] {
            assert!(!without_key.tools.contains_key(removed));
        }
        assert!(!without_key.system.last().unwrap().cache);
        assert_eq!(without_key.system.len(), 4);
        assert_eq!(
            without_key
                .system
                .iter()
                .map(|block| block.cache)
                .collect::<Vec<_>>(),
            vec![true, false, true, false]
        );
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
        assert!(with_key.tools.contains_key("search_web"));
        assert_eq!(with_key.system, without_key.system);
        let search = with_key
            .definitions
            .iter()
            .find(|definition| definition.name == "search_web")
            .unwrap();
        assert!(search.description.contains("current information"));
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

        let schema = SearchWeb {
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

        let tool = Read::new(
            tempfile::tempdir().unwrap().path(),
            Arc::new(SnapshotStore::default()),
            build_http_client().unwrap(),
        )
        .unwrap();
        let output = tool
            .execute(&json!({ "path": format!("http://{address}") }))
            .await
            .unwrap();

        server.join().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["kind"], "web");
        assert_eq!(parsed["format"], "text");
        assert_eq!(parsed["items"], json!(["runtime-safe response"]));
    }

    #[test]
    fn edit_requires_read_and_write_is_create_only() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::write(directory.path().join("data/note.md"), "old\n").unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let edit = Edit::new(
            directory.path(),
            bypass_gate(),
            reads.clone(),
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        let tag = file_snapshot_tag(directory.path().join("data/note.md"));
        let input = json!({
            "patch": format!("[data/note.md#{tag}]\nPUT 1.=1:\n+new\n")
        });
        // Editing a file that was never read is rejected outright.
        assert!(edit.execute(&input).returns_err());

        let read = Read::new(directory.path(), reads, build_http_client().unwrap()).unwrap();
        read.execute(&json!({"path": "data/note.md"})).unwrap();
        let mut wrong_tag = input.clone();
        wrong_tag["patch"] = json!(format!("[data/note.md#0000]\nPUT 1.=1:\n+new\n"));
        assert!(edit
            .execute(&wrong_tag)
            .unwrap_err()
            .to_string()
            .contains("snapshot tag mismatch"));
        edit.execute(&input).unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("data/note.md")).unwrap(),
            "new\n"
        );
        // The snapshot the first edit was anchored on is now stale: the very
        // line the patch touches changed, so re-applying it must fail until a
        // re-read.
        assert!(edit
            .execute(&input)
            .unwrap_err()
            .to_string()
            .contains("file changed since read at line 1"));

        let write = Write::new(directory.path(), bypass_gate()).unwrap();
        let schema = write.input_schema();
        assert!(schema["properties"].get("overwrite").is_none());
        let existing_error = write
            .execute(&json!({"path": "data/note.md", "content": "overwrite"}))
            .unwrap_err()
            .to_string();
        assert!(existing_error.contains("write only creates new files"));
        let explicit_overwrite_error = write
            .execute(&json!({
                "path": "data/note.md",
                "content": "overwritten\n",
                "overwrite": true
            }))
            .unwrap_err()
            .to_string();
        assert!(explicit_overwrite_error.contains("write only creates new files"));
        assert_eq!(
            fs::read_to_string(directory.path().join("data/note.md")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn file_edit_rejects_a_hashline_snapshot_after_external_change() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        let path = directory.path().join("data/note.md");
        fs::write(&path, "old\n").unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let read = Read::new(directory.path(), reads.clone(), build_http_client().unwrap()).unwrap();
        let output = read
            .execute(&json!({"path": "data/note.md"}))
            .unwrap();
        assert!(output.starts_with(&format!("[data/note.md#{}]", snapshot_tag("old\n"))));

        // The external change rewrote the very line the patch targets, so the
        // rebase must fail: the retained snapshot can no longer prove that
        // line of the edit still exists unchanged in the live file.
        fs::write(&path, "changed elsewhere\n").unwrap();
        let error = Edit::new(
            directory.path(),
            bypass_gate(),
            reads.clone(),
            Arc::new(RegisterBank::default()),
        )
        .unwrap()
        .execute(&json!({
            "patch": format!("[data/note.md#{}]\nPUT 1.=1:\n+agent change\n", snapshot_tag("old\n"))
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains("file changed since read at line 1"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "changed elsewhere\n");

        // When the external change hit an unrelated line only, the edit REBASES
        // onto the live file (from the retained pre-change text) and lands.
        fs::write(&path, "old\nnew unrelated line\n").unwrap();
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "data/note.md"}))
            .unwrap();
        fs::write(&path, "old\nnew unrelated line\nappended externally\n").unwrap();
        let result = Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap()
        .execute(&json!({
            "patch": format!(
                "[data/note.md#{}]\nPUT 1.=1:\n+agent change\n",
                snapshot_tag("old\nnew unrelated line\n")
            )
        }))
        .unwrap();
        assert!(result.contains("edited data/note.md"));
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "agent change\nnew unrelated line\nappended externally\n"
        );
    }

    #[test]
    fn markdown_writes_persist_and_return_diagnostics_for_focused_repairs() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let invalid = "[box border=double]\nbody\n[/box]\n";

        let write = Write::new(directory.path(), bypass_gate()).unwrap();
        let result = write
            .execute(&json!({"path": "data/invalid.md", "content": invalid}))
            .unwrap();
        assert!(result.contains("wrote"));
        assert!(result.contains("MBDown validation warning"));
        assert!(result.contains("box border must be single or none"));
        assert!(result.contains("file change succeeded"));
        assert!(result.contains("Read it and use edit"));
        assert_eq!(
            fs::read_to_string(storage.data_dir.join("invalid.md")).unwrap(),
            invalid
        );

        let mermaid_result = write
            .execute(&json!({
                "path": "data/invalid-mermaid.md",
                "content": "# Diagram\n\n```mermaid\nclassDiagram\nclass Agent~T~ {}\n```\n"
            }))
            .unwrap();
        assert!(mermaid_result.contains("Mermaid validation failed at line 3"));
        assert!(mermaid_result.contains("generics not yet supported"));
        assert!(storage.data_dir.join("invalid-mermaid.md").exists());

        let plain_result = write
            .execute(&json!({"path": "data/plain.txt", "content": invalid}))
            .unwrap();
        assert!(!plain_result.contains("MBDown validation warning"));
        assert_eq!(fs::read_to_string(storage.data_dir.join("plain.txt")).unwrap(), invalid);

        let path = storage.data_dir.join("note.md");
        fs::write(&path, "valid\n").unwrap();
        let overwrite_error = write
            .execute(&json!({
                "path": "data/note.md",
                "content": invalid,
                "overwrite": true
            }))
            .unwrap_err()
            .to_string();
        assert!(overwrite_error.contains("write only creates new files"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "valid\n");
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "data/note.md"}))
            .unwrap();
        let edit = Edit::new(
            directory.path(),
            bypass_gate(),
            reads.clone(),
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        let result = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!(
                    "[data/note.md#{}]\nPUT 1.=1:\n+[box border=double]\n+body\n+[/box]\n",
                    file_snapshot_tag(&path)
                )
            })))
            .unwrap();
        assert!(result.contains("edited data/note.md"));
        assert!(result.contains("box border must be single or none"));
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);

        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "data/note.md"}))
            .unwrap();
        let repaired = test_runtime()
            .block_on(edit.execute(&json!({
                "patch": format!(
                    "[data/note.md#{}]\nPUT 1.=1:\n+[box border=single]\n",
                    file_snapshot_tag(&path)
                )
            })))
            .unwrap();
        assert!(repaired.contains("edited data/note.md"));
        assert!(mbdown::validate(&fs::read_to_string(&path).unwrap()).is_ok());
    }

    #[test]
    fn skill_writes_validate_complete_documents_before_any_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let valid = "---\ndescription: Original workflow\n---\n\n# Steps\n";
        let invalid = "# Missing front matter\n";

        let write = Write::new(directory.path(), bypass_gate()).unwrap();
        let create_error = write
            .execute(&json!({
                "path": "skills/broken.md",
                "content": invalid
            }))
            .unwrap_err()
            .to_string();
        assert!(create_error.contains("front matter"));
        assert!(!storage.skills_dir.join("broken.md").exists());

        write
            .execute(&json!({
                "path": "skills/existing.md",
                "content": valid
            }))
            .unwrap();
        let existing = storage.skills_dir.join("existing.md");
        let overwrite_error = write
            .execute(&json!({
                "path": "skills/existing.md",
                "content": invalid,
                "overwrite": true
            }))
            .unwrap_err()
            .to_string();
        assert!(overwrite_error.contains("write only creates new files"));
        assert_eq!(fs::read_to_string(&existing).unwrap(), valid);
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "skills/existing.md"}))
            .unwrap();
        let edit = Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        let edit_error = edit
            .execute(&json!({
                "patch": format!(
                    "[skills/existing.md#{}]\nPUT 2.=2:\n+desc: Wrong field\n",
                    file_snapshot_tag(&existing)
                )
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
        assert!(Copy::new(directory.path(), bypass_gate())
            .unwrap()
            .execute(&json!({
                "source": copy_source,
                "destination": "skills/copied.md"
            }))
            .returns_err());
        assert!(copy_source.exists());
        assert!(!storage.skills_dir.join("copied.md").exists());

        let (event_sender, _) = event_channel();
        assert!(Move::new(directory.path(), event_sender.clone(), bypass_gate())
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
        assert!(MoveMany::new(directory.path(), batch_events, bypass_gate())
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

        assert!(Rename::new(directory.path(), event_sender, bypass_gate())
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
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "data/range.md"}))
            .unwrap();
        let edit = Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        let schema = edit.input_schema();
        assert_eq!(schema["required"], json!(["patch"]));
        assert_eq!(schema["properties"]["patch"], json!({"type": "string"}));
        assert_eq!(schema["additionalProperties"], json!(false));

        // `PUT N.=M:` replaces every original line from N through M inclusive.
        edit.execute(&json!({
            "patch": format!(
                "[data/range.md#{}]\nPUT 2.=4:\n+replacement\n",
                file_snapshot_tag(&path)
            )
        }))
        .unwrap();

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "zero\nreplacement\nfour\n"
        );
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

        let reads = Arc::new(SnapshotStore::default());
        let read = Read::new(directory.path(), reads.clone(), build_http_client().unwrap()).unwrap();
        let edit = Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        read.execute(&json!({"path": "data/large.md", "range": "11-11"}))
            .unwrap();
        edit.execute(&json!({
            "patch": format!(
                "[data/large.md#{}]\nPUT 11.=11:\n+changed 10\n",
                file_snapshot_tag(&path)
            )
        }))
        .unwrap();

        fs::write(&path, &original).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let read = Read::new(directory.path(), reads.clone(), build_http_client().unwrap()).unwrap();
        let edit = Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        read.execute(&json!({"path": "data/large.md", "range": "11-11"}))
            .unwrap();
        let error = edit
            .execute(&json!({
                "patch": format!(
                    "[data/large.md#{}]\nPUT 3.=3:\n+changed 2\n",
                    file_snapshot_tag(&path)
                )
            }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("read lines 3 through 3"));

        fs::write(&path, "first\nsecond\nthird\n").unwrap();
        let reads = Arc::new(SnapshotStore::default());
        let read = Read::new(directory.path(), reads.clone(), build_http_client().unwrap()).unwrap();
        let edit = Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        read.execute(&json!({"path": "data/large.md", "range": "2-2"}))
            .unwrap();
        let error = edit
            .execute(&json!({
                "patch": format!(
                    "[data/large.md#{}]\nPUT <2:\n+inserted\n",
                    file_snapshot_tag(&path)
                )
            }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("read lines 1 through 2"));
    }

    #[test]
    fn file_edit_body_rows_are_single_lines_and_preserve_line_endings() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("table.md");
        let original = "| model | score |\n| --- | --- |\n| MM26 | 0.65 |\n";
        fs::write(&path, original).unwrap();
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "data/table.md"}))
            .unwrap();
        let edit = Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();

        // A hashline `+` body row is exactly one line of the new file: one row
        // inserted before line 3 lands as a single line, never split.
        edit.execute(&json!({
            "patch": format!(
                "[data/table.md#{}]\nPUT <3:\n+| SAC | 0.639 |\n",
                file_snapshot_tag(&path)
            )
        }))
        .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "| model | score |\n| --- | --- |\n| SAC | 0.639 |\n| MM26 | 0.65 |\n"
        );

        // An EOF insert onto a file with no trailing newline still works.
        let eof_path = storage.data_dir.join("eof.md");
        fs::write(&eof_path, "old").unwrap();
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "data/eof.md"}))
            .unwrap();
        Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap()
        .execute(&json!({
            "patch": format!("[data/eof.md#{}]\nPUT >$:\n+new\n", file_snapshot_tag(&eof_path))
        }))
        .unwrap();
        assert_eq!(fs::read_to_string(&eof_path).unwrap(), "old\nnew\n");

        // A CRLF file stays CRLF after an edit.
        let crlf_path = storage.data_dir.join("crlf.md");
        fs::write(&crlf_path, "old\r\nvalue\r\n").unwrap();
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "data/crlf.md"}))
            .unwrap();
        Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap()
        .execute(&json!({
            "patch": format!(
                "[data/crlf.md#{}]\nPUT 2.=2:\n+changed\n",
                file_snapshot_tag(&crlf_path)
            )
        }))
        .unwrap();
        assert_eq!(fs::read_to_string(&crlf_path).unwrap(), "old\r\nchanged\r\n");
    }

    #[test]
    fn daily_markdown_uses_generic_read_edit_and_delete_tools() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage.append_daily("2026-07-27", "old").unwrap();
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "daily/2026-07-27.md"}))
            .unwrap();
        Edit::new(
            directory.path(),
            bypass_gate(),
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap()
        .execute(&json!({
            "patch": format!(
                "[daily/2026-07-27.md#{}]\nPUT 1.=1:\n+edited\n",
                file_snapshot_tag(storage.daily_file_path("2026-07-27").unwrap())
            )
        }))
        .unwrap();
        assert_eq!(storage.load_daily_notes().unwrap()[0].body, "edited");

        Delete::new(directory.path(), bypass_gate())
            .unwrap()
            .execute(&json!({"path": "daily/2026-07-27.md"}))
            .unwrap();
        assert!(!storage.daily_dir.join("2026-07-27.md").exists());
        assert!(Write::new(directory.path(), bypass_gate())
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
        let result = add
            .execute(&json!({
                "date": "2026-07-28",
                "content": "[box border=double]\ninvalid\n[/box]"
            }))
            .unwrap();
        assert!(result.contains("MBDown validation warning"));
        assert!(result.contains("box border must be single or none"));
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "existing\n\n[box border=double]\ninvalid\n[/box]\n"
        );
    }

    #[test]
    fn edit_waits_for_diff_approval() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::write(directory.path().join("data/note.md"), "old\n").unwrap();
        let reads = Arc::new(SnapshotStore::default());
        Read::new(directory.path(), reads.clone(), build_http_client().unwrap())
            .unwrap()
            .execute(&json!({"path": "data/note.md"}))
            .unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = gate(PermissionMode::Approve, directory.path(), event_sender, decision_receiver);
        let edit = Edit::new(
            directory.path(),
            gate,
            reads,
            Arc::new(RegisterBank::default()),
        )
        .unwrap();
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(edit.execute(&json!({
                "patch": format!(
                    "[data/note.md#{}]\nPUT 1.=1:\n+new\n",
                    snapshot_tag("old\n")
                )
            })))
        });

        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert!(request.message.contains("-old"));
        assert!(request.message.contains("+new"));
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
        let tool = Open::new(directory.path(), sender).unwrap();

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
        let tool = Ask {
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
        assert_eq!(listed["items"][0]["tag"], "rust");
        assert_eq!(listed["items"][0]["mentions"], 2);

        let searched: Value = serde_json::from_str(
            &SearchTag::new(directory.path(), handle)
                .unwrap()
                .execute(&json!({"tag": "#rust"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(searched["total"], 1);
        assert_eq!(searched["items"][0]["line"], 1);
        assert!(searched["items"][0]["snippet"]
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
        let gate = gate(PermissionMode::Approve, directory.path(), event_sender, decision_receiver);
        let tool = RenameTag::new(directory.path(), handle.clone(), gate).unwrap();
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(tool.execute(&json!({"from": "old", "to": "new"})))
        });

        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert!(request.message.contains("-#old"));
        assert!(request.message.contains("+#new"));
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


    #[test]
    fn wiki_tools_query_the_shared_index_with_exact_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            storage.data_dir.join("Project.md"),
            "project body [[Other]]\n",
        )
        .unwrap();
        fs::write(
            storage.data_dir.join("Referrer.md"),
            "see [[Project]] and [[Project]]\n",
        )
        .unwrap();
        fs::write(
            storage.daily_dir.join("2026-07-28.md"),
            "daily [[project]]\n",
        )
        .unwrap();
        let handle = WikiLinkIndexHandle::default();
        handle.replace(crate::wiki_link_index::WikiLinkIndex::build(&storage));

        let resolved: Value = serde_json::from_str(
            &ResolveWikilink::new(directory.path(), handle.clone())
                .unwrap()
                .execute(&json!({"target": "Project"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(resolved["status"], "unique");
        assert_eq!(resolved["candidates"].as_array().unwrap().len(), 1);
        assert!(resolved["candidates"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("data/Project.md"));

        let missing: Value = serde_json::from_str(
            &ResolveWikilink::new(directory.path(), handle.clone())
                .unwrap()
                .execute(&json!({"target": "Nope"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(missing["status"], "missing");

        let backlinks: Value = serde_json::from_str(
            &Backlinks::new(directory.path(), handle.clone())
                .unwrap()
                .execute(&json!({"target": "Project.md"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(backlinks["count"], 2, "one note per distinct referrer");
        assert_eq!(backlinks["backlinks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn rename_wikilink_requires_approval_and_updates_only_real_links() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("Links.md");
        fs::write(
            &path,
            "[[Old]] and [[old]] and ```[[Old]]``` and <!-- [[Old]] -->\n",
        )
        .unwrap();
        let handle = WikiLinkIndexHandle::default();
        handle.replace(crate::wiki_link_index::WikiLinkIndex::build(&storage));
        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = gate(PermissionMode::Approve, directory.path(), event_sender, decision_receiver);
        let tool = RenameWikilink::new(directory.path(), handle.clone(), gate).unwrap();
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(tool.execute(&json!({"from": "Old", "to": "New"})))
        });

        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert!(request.message.contains("Rename [[Old]] to [[New]]"));
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        let result: Value = serde_json::from_str(&worker.join().unwrap().unwrap()).unwrap();
        assert_eq!(result["documents"], 1);
        assert_eq!(result["mentions"], 2);
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "[[New]] and [[New]] and ```[[Old]]``` and <!-- [[Old]] -->\n",
            "code fence and comment targets are untouched"
        );
    }

    #[test]
    fn rename_wikilink_rejects_illegal_targets_before_diff_or_write() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("Links.md");
        fs::write(&path, "[[Old]]\n").unwrap();
        let handle = WikiLinkIndexHandle::default();
        handle.replace(crate::wiki_link_index::WikiLinkIndex::build(&storage));
        let (event_sender, mut event_receiver) = event_channel();
        let (_decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = gate(PermissionMode::Approve, directory.path(), event_sender, decision_receiver);
        let tool = RenameWikilink::new(directory.path(), handle, gate).unwrap();
        let run = |input: Value| {
            test_runtime()
                .block_on(tool.execute(&input))
                .map(|_| ())
                .unwrap_err()
                .to_string()
        };

        for (input, needle) in [
            (json!({"from": "", "to": "New"}), "must not be empty"),
            (json!({"from": "Old", "to": "Ne[w"}), "brackets or newlines"),
            (json!({"from": "Old", "to": "a]]b"}), "brackets or newlines"),
            (json!({"from": "Old", "to": "New\nName"}), "brackets or newlines"),
            (json!({"from": "O[ld", "to": "New"}), "brackets or newlines"),
            (json!({"from": "Old", "to": "OLD"}), "same"),
        ] {
            let error = run(input);
            assert!(error.contains(needle), "unexpected error: {error}");
            assert!(
                event_receiver.try_recv().is_err(),
                "no approval request may be raised for rejected targets"
            );
        }
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[[Old]]\n",
            "no file may change for rejected targets"
        );
    }

    #[test]
    fn rename_wikilink_discovers_targets_case_insensitively() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let path = storage.data_dir.join("Links.md");
        fs::write(&path, "see [[old]] here\n").unwrap();
        let handle = WikiLinkIndexHandle::default();
        handle.replace(crate::wiki_link_index::WikiLinkIndex::build(&storage));
        let tool = RenameWikilink::new(directory.path(), handle, bypass_gate()).unwrap();
        let result: Value = serde_json::from_str(
            &test_runtime()
                .block_on(tool.execute(&json!({"from": "Old", "to": "New"})))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["documents"], 1);
        assert_eq!(result["mentions"], 1);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "see [[New]] here\n",
            "the index keys on the written case, so discovery must match case-insensitively"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rename_wikilink_rejects_symlinks_and_managed_directory_escape() {
        use std::os::unix::fs::symlink;

        // A note file replaced by a symlink to an outside file after the index
        // was built is skipped: the rename must never write through it.
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let note = storage.data_dir.join("Note.md");
        fs::write(&note, "[[Old]]\n").unwrap();
        let handle = WikiLinkIndexHandle::default();
        handle.replace(crate::wiki_link_index::WikiLinkIndex::build(&storage));
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().join("Note.md");
        fs::write(&outside, "[[Old]] outside\n").unwrap();
        fs::remove_file(&note).unwrap();
        symlink(&outside, &note).unwrap();
        let tool = RenameWikilink::new(directory.path(), handle.clone(), bypass_gate()).unwrap();
        let error = test_runtime()
            .block_on(tool.execute(&json!({"from": "Old", "to": "New"})))
            .unwrap_err()
            .to_string();
        assert!(error.contains("was not found"), "unexpected error: {error}");
        assert_eq!(
            fs::read_to_string(&outside).unwrap(),
            "[[Old]] outside\n",
            "the outside file must not be rewritten through the symlink"
        );

        // A managed directory swapped for a symlink to an outside directory
        // must not redirect the rename: containment validation rejects the
        // escape before any diff or write.
        let second = tempfile::tempdir().unwrap();
        let storage = Storage::new(second.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(storage.data_dir.join("Real.md"), "[[Old]]\n").unwrap();
        let handle = WikiLinkIndexHandle::default();
        handle.replace(crate::wiki_link_index::WikiLinkIndex::build(&storage));
        let decoy_dir = tempfile::tempdir().unwrap();
        fs::write(decoy_dir.path().join("Real.md"), "[[Old]] decoy\n").unwrap();
        let original = second.path().join("data-original");
        fs::rename(&storage.data_dir, &original).unwrap();
        symlink(decoy_dir.path(), &storage.data_dir).unwrap();
        let tool = RenameWikilink::new(second.path(), handle, bypass_gate()).unwrap();
        let error = test_runtime()
            .block_on(tool.execute(&json!({"from": "Old", "to": "New"})))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("escapes the Nole root"),
            "unexpected error: {error}"
        );
        assert_eq!(
            fs::read_to_string(decoy_dir.path().join("Real.md")).unwrap(),
            "[[Old]] decoy\n",
            "the decoy must not be rewritten through the symlinked directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rename_wikilink_write_failure_leaves_all_originals_untouched() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        // archives/ sorts before daily/, so the writable file is staged first
        // and the read-only one fails second: exactly the partial-update case
        // the staged commit must prevent.
        let archive_note = storage.archives_dir.join("Archive.md");
        fs::write(&archive_note, "[[Old]]\n").unwrap();
        let daily_note = storage.daily_dir.join("2026-07-28.md");
        fs::write(&daily_note, "[[Old]]\n").unwrap();
        let handle = WikiLinkIndexHandle::default();
        handle.replace(crate::wiki_link_index::WikiLinkIndex::build(&storage));
        // Lock the daily directory so staging the second replacement fails
        // after the first file's replacement has already been staged.
        let locked = fs::metadata(&storage.daily_dir).unwrap().permissions();
        fs::set_permissions(&storage.daily_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let tool = RenameWikilink::new(directory.path(), handle, bypass_gate()).unwrap();
        let result = test_runtime().block_on(tool.execute(&json!({"from": "Old", "to": "New"})));
        fs::set_permissions(&storage.daily_dir, locked).unwrap();
        assert!(result.is_err(), "staging into a read-only directory must fail");
        assert_eq!(
            fs::read_to_string(&archive_note).unwrap(),
            "[[Old]]\n",
            "no partial update: the writable file must stay untouched when a sibling fails"
        );
        assert_eq!(fs::read_to_string(&daily_note).unwrap(), "[[Old]]\n");
        for directory in [&storage.archives_dir, &storage.daily_dir] {
            let residue: Vec<_> = fs::read_dir(directory)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".nole-rename-"))
                .collect();
            assert!(residue.is_empty(), "staged temp residue: {residue:?}");
        }
    }

    #[test]
    fn registered_tools_enforce_their_json_schema_before_execution() {
        let (events, mut receiver) = event_channel();
        let tool = Notify { events };
        let schema = tool.input_schema();
        let registered = RegisteredTool::new(tool, &schema);
        let runtime = test_runtime();

        let error = runtime
            .block_on(registered.execute(&json!({"message": 42})))
            .unwrap_err();
        assert!(error.to_string().contains("invalid input for tool notify"));
        assert!(receiver.try_recv().is_err());

        let error = runtime
            .block_on(registered.execute(&json!({"message": "ok", "extra": true})))
            .unwrap_err();
        assert!(error.to_string().contains("invalid input for tool notify"));
        assert!(receiver.try_recv().is_err());

        assert_eq!(
            runtime
                .block_on(registered.execute(&json!({"message": "valid"})))
                .unwrap()
                .text,
            "notification shown"
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(AgentEvent::Notification(message)) if message == "valid"
        ));
    }

    #[test]
    fn tool_error_messages_preserve_the_complete_cause_chain() {
        let error = anyhow::anyhow!("connection refused").context("fetching page");
        assert_eq!(
            tool_error_message(&error),
            "fetching page: connection refused"
        );
    }

    #[test]
    fn approval_gate_enforces_approve_auto_yolo_path_matrix() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root_dir.path()).unwrap();
        let internal = root.join("data/note.md");
        let outside_dir = tempfile::tempdir().unwrap();
        let external = fs::canonicalize(outside_dir.path()).unwrap().join("external.md");
        let request = || ApprovalRequest {
            title: "Test change".to_string(),
            message: "Change path?".to_string(),
            kind: ApprovalKind::Confirm,
        };

        let (events, mut receiver) = event_channel();
        let (_decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let auto_internal = gate(PermissionMode::Auto, &root, events, decision_receiver);
        test_runtime()
            .block_on(auto_internal.request_for_paths(request(), &[&internal]))
            .unwrap();
        assert!(receiver.try_recv().is_err());

        let (events, mut receiver) = event_channel();
        let (decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let auto_external = gate(PermissionMode::Auto, &root, events, decision_receiver);
        decisions.send(ApprovalDecision::Approve).unwrap();
        test_runtime()
            .block_on(auto_external.request_for_paths(request(), &[&internal, &external]))
            .unwrap();
        assert!(matches!(receiver.try_recv(), Ok(AgentEvent::Approval(_))));

        let (events, mut receiver) = event_channel();
        let (decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let approve_internal = gate(PermissionMode::Approve, &root, events, decision_receiver);
        decisions.send(ApprovalDecision::Approve).unwrap();
        test_runtime()
            .block_on(approve_internal.request_for_paths(request(), &[&internal]))
            .unwrap();
        assert!(matches!(receiver.try_recv(), Ok(AgentEvent::Approval(_))));

        let (events, mut receiver) = event_channel();
        let (_decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let yolo_external = gate(PermissionMode::Yolo, &root, events, decision_receiver);
        test_runtime()
            .block_on(yolo_external.request_for_paths(request(), &[&external]))
            .unwrap();
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn host_actions_ask_in_approve_and_auto_but_not_yolo() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root_dir.path()).unwrap();
        let request = || ApprovalRequest {
            title: "Run shell command".to_string(),
            message: String::new(),
            kind: ApprovalKind::Command(CommandApproval {
                purpose: "Check formatting".to_string(),
                label: "Cmd".to_string(),
                code: "markdownlint data/note.md".to_string(),
            }),
        };

        for mode in [PermissionMode::Approve, PermissionMode::Auto] {
            let (events, mut receiver) = event_channel();
            let (decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
            decisions.send(ApprovalDecision::Approve).unwrap();
            test_runtime()
                .block_on(gate(mode, &root, events, decision_receiver).request_host_action(request()))
                .unwrap();
            assert!(matches!(receiver.try_recv(), Ok(AgentEvent::Approval(_))));
        }

        let (events, mut receiver) = event_channel();
        let (_decisions, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        test_runtime()
            .block_on(
                gate(PermissionMode::Yolo, &root, events, decision_receiver)
                    .request_host_action(request()),
            )
            .unwrap();
        assert!(receiver.try_recv().is_err());
    }
