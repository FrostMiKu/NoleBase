    use super::*;
    use super::test_support::*;

    #[test]
    fn agent_config_defaults_to_twenty_five_rounds_and_validates_overrides() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ai.toml");
        fs::write(&path, TEST_MESSAGES_CONFIG).unwrap();
        let config = AgentConfig::load(&path).unwrap();
        assert_eq!(config.max_rounds, 25);
        assert_eq!(config.max_tokens, 8192);
        assert_eq!(config.context_window_tokens, 200_000);

        fs::write(&path, format!("{TEST_MESSAGES_CONFIG}max_rounds = 40\n")).unwrap();
        assert_eq!(AgentConfig::load(&path).unwrap().max_rounds, 40);

        fs::write(&path, format!("{TEST_MESSAGES_CONFIG}max_rounds = 0\n")).unwrap();
        assert!(AgentConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("max_rounds must be greater than zero"));

        fs::write(
            &path,
            format!("{TEST_MESSAGES_CONFIG}max_tokens = 4096\ncontext_window_tokens = 4096\n"),
        )
        .unwrap();
        assert!(AgentConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("context_window_tokens must be greater than max_tokens"));

        fs::write(
            &path,
            "api_format = 'completions'\napi_key = ''\nmodel = 'local-model'\nbase_url = 'http://127.0.0.1:11434'\n",
        )
        .unwrap();
        assert_eq!(
            AgentConfig::load(&path).unwrap().api_format,
            ApiFormat::Completions
        );

        fs::write(
            &path,
            "api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'https://api.anthropic.com/v1'\n",
        )
        .unwrap();
        assert!(AgentConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("base_url must not include /v1"));
    }
    #[test]
    fn context_compaction_boundaries_keep_tool_protocol_pairs_together() {
        let messages = vec![
            Message::user("old request"),
            Message::assistant(vec![MessagePart::ToolUse(ToolCall {
                id: "tool-1".to_string(),
                name: "read_file".to_string(),
                input: json!({}),
            })]),
            Message::tool(ToolResult {
                tool_use_id: "tool-1".to_string(),
                content: "result".to_string(),
                is_error: false,
            }),
            Message::assistant(vec![MessagePart::Text {
                text: "old answer".to_string(),
            }]),
            Message::user("latest request"),
        ];

        assert!(!is_safe_compaction_boundary(&messages, 1));
        assert!(!is_safe_compaction_boundary(&messages, 2));
        assert!(!is_safe_compaction_boundary(&messages, 3));
        assert!(is_safe_compaction_boundary(&messages, 4));
        let cut = context_compaction_cut(&messages, CONTEXT_ESTIMATE_OVERHEAD + 100).unwrap();
        assert_eq!(cut, 4);
        assert_eq!(messages.last().unwrap().text(), "latest request");
    }

    #[test]
    fn agent_conversation_is_clearable() {
        let mut conversation = AgentConversation::default();
        conversation.messages.push(Message::user("first turn"));
        conversation
            .messages
            .push(Message::assistant(vec![MessagePart::Text {
                text: "reply".to_string(),
            }]));
        assert!(!conversation.messages.is_empty());

        assert!(conversation.clear());
        assert!(conversation.messages.is_empty());
        assert!(!conversation.clear());
    }

    #[test]
    fn token_usage_accumulates_cached_and_uncached_input() {
        let mut total = TokenUsage {
            input_tokens: 400,
            output_tokens: 80,
            cache_creation_input_tokens: 1_000,
            cache_read_input_tokens: 2_000,
        };
        total.add(TokenUsage {
            input_tokens: 100,
            output_tokens: 20,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 500,
        });

        assert_eq!(total.total_input(), 4_000);
        assert_eq!(total.output_tokens, 100);
        assert!(!total.is_empty());
    }

    #[test]
    fn file_tools_stay_inside_root() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        let create = CreateFile::new(directory.path()).unwrap();
        create
            .execute(&json!({"path": "data/test.md", "content": "hello"}))
            .unwrap();
        let read = ReadFile::new(directory.path(), Arc::new(ReadTracker::default())).unwrap();
        let result: Value =
            serde_json::from_str(&read.execute(&json!({"path": "data/test.md"})).unwrap()).unwrap();
        assert_eq!(result["path"], "data/test.md");
        assert_eq!(result["lines"][0]["line"], 0);
        assert_eq!(result["lines"][0]["text"], "hello");
        assert_eq!(result["total_lines"], 1);
        assert_eq!(result["has_more"], false);
        let outside_directory = tempfile::tempdir().unwrap();
        let outside = outside_directory.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        let result: Value =
            serde_json::from_str(&read.execute(&json!({"path": outside})).unwrap()).unwrap();
        assert!(result["path"]
            .as_str()
            .is_some_and(|path| Path::new(path).is_absolute()));
        assert_eq!(result["lines"][0]["line"], 0);
        assert_eq!(result["lines"][0]["text"], "outside");
    }

    #[test]
    fn paginated_file_reads_require_only_the_ranges_touched_by_edit() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        let content = (0..450)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        fs::write(directory.path().join("data/large.md"), &content).unwrap();
        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let edit = EditFile::new(directory.path(), bypass_gate(), reads).unwrap();

        let first: Value = serde_json::from_str(
            &read
                .execute(&json!({"path": "data/large.md", "offset": 0, "limit": 200}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first["returned_lines"], 200);
        assert_eq!(first["total_lines"], 450);
        assert_eq!(first["has_more"], true);
        assert!(edit
            .execute(&json!({
                "path": "data/large.md",
                "edits": [{"operation": "insert", "line": 450, "lines": ["done"]}]
            }))
            .returns_err());

        read.execute(&json!({"path": "data/large.md", "offset": 400, "limit": 50}))
            .unwrap();
        edit.execute(&json!({
            "path": "data/large.md",
            "edits": [{"operation": "insert", "line": 450, "lines": ["done"]}]
        }))
        .unwrap();
    }

    #[test]
    fn file_change_events_invalidate_only_affected_read_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        let alpha = directory.path().join("data/alpha.md");
        let beta = directory.path().join("data/beta.md");
        fs::write(&alpha, "alpha\n").unwrap();
        fs::write(&beta, "beta\n").unwrap();
        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        read.execute(&json!({"path": "data/alpha.md"})).unwrap();
        read.execute(&json!({"path": "data/beta.md"})).unwrap();
        let alpha = fs::canonicalize(alpha).unwrap();
        let beta = fs::canonicalize(beta).unwrap();

        reads.invalidate(&alpha).unwrap();
        assert!(reads.file_state(&alpha).unwrap().is_none());
        assert!(reads.file_state(&beta).unwrap().is_some());

        reads.invalidate(&directory.path().join("data")).unwrap();
        assert!(reads.file_state(&beta).unwrap().is_none());
    }

    #[test]
    fn content_and_filename_searches_are_structured_and_paginated() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        storage.append_to_today("Rust in Daily").unwrap();
        fs::write(
            directory.path().join("data/RustProject.md"),
            "# Project\nRust in a note\n",
        )
        .unwrap();
        fs::write(directory.path().join("data/Research.md"), "unrelated\n").unwrap();
        fs::write(
            directory.path().join("archives/RustHistory.md"),
            "Rust in an archived note\n",
        )
        .unwrap();

        let content = SearchContent::new(directory.path()).unwrap();
        let result: Value = serde_json::from_str(
            &content
                .execute(&json!({"query": "rust", "limit": 1}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["returned"], 1);
        assert_eq!(result["total_matches"], 3);
        assert_eq!(result["has_more"], true);
        assert!(result["matches"][0]["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("daily/") && path.ends_with(".md")));
        assert_eq!(result["matches"][0]["line"], 0);

        let remaining: Value = serde_json::from_str(
            &content
                .execute(&json!({"query": "rust", "offset": 1, "limit": 10}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(remaining["matches"][0]["path"], "data/RustProject.md");
        assert_eq!(remaining["matches"][1]["path"], "archives/RustHistory.md");

        let files = SearchFiles::new(directory.path()).unwrap();
        let result: Value = serde_json::from_str(
            &files
                .execute(&json!({"query": "rsprj", "offset": 0, "limit": 10}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total_matches"], 1);
        assert_eq!(result["matches"][0]["path"], "data/RustProject.md");
        let archived: Value = serde_json::from_str(
            &files
                .execute(&json!({"query": "history", "limit": 10}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(archived["total_matches"], 1);
        assert_eq!(archived["matches"][0]["path"], "archives/RustHistory.md");
    }

    #[test]
    fn private_config_is_not_available_but_daily_files_are_readable() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::write(directory.path().join("config/ai.toml"), "api_key='secret'").unwrap();
        fs::write(directory.path().join("daily/2026-07-27.md"), "daily entry").unwrap();
        let read = ReadFile::new(directory.path(), Arc::new(ReadTracker::default())).unwrap();
        assert!(read
            .execute(&json!({"path": "config/ai.toml"}))
            .returns_err());
        let daily: Value = serde_json::from_str(
            &read
                .execute(&json!({"path": "daily/2026-07-27.md"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(daily["lines"][0]["line"], 0);
        assert_eq!(daily["lines"][0]["text"], "daily entry");
    }

    #[test]
    fn config_is_read_only_to_agent_tools_while_root_files_are_updatable() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::create_dir(directory.path().join("themes")).unwrap();
        fs::write(directory.path().join("config/AGENTS.md"), "user rules\n").unwrap();
        fs::write(
            directory.path().join("themes/custom.toml"),
            "[ui]\naction = \"#94e2d5\"\n",
        )
        .unwrap();
        fs::write(directory.path().join("MEMORY.md"), "old memory\n").unwrap();

        let reads = Arc::new(ReadTracker::default());
        let read = ReadFile::new(directory.path(), reads.clone()).unwrap();
        let edit = EditFile::new(directory.path(), bypass_gate(), reads).unwrap();
        let create = CreateFile::new(directory.path()).unwrap();

        read.execute(&json!({"path": "config/AGENTS.md"})).unwrap();
        assert!(edit
            .execute(&json!({
                "path": "config/AGENTS.md",
                "edits": [{"operation": "replace", "start_line": 0, "end_line": 0, "lines": ["changed"]}]
            }))
            .returns_err());
        assert!(create
            .execute(&json!({"path": "config/new.md", "content": "forbidden"}))
            .returns_err());
        assert!(DeleteFile::new(directory.path(), bypass_gate())
            .unwrap()
            .execute(&json!({"path": "config/AGENTS.md"}))
            .returns_err());
        assert!(directory.path().join("config/AGENTS.md").exists());
        assert!(
            ensure_not_special(directory.path(), &directory.path().join("config/AGENTS.md"))
                .is_err()
        );
        read.execute(&json!({"path": "themes/custom.toml"}))
            .unwrap();
        edit.execute(&json!({
            "path": "themes/custom.toml",
                "edits": [{"operation": "replace", "start_line": 1, "end_line": 1, "lines": ["action = \"#010203\""]}]
        }))
        .unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("themes/custom.toml")).unwrap(),
            "[ui]\naction = \"#010203\"\n"
        );

        read.execute(&json!({"path": "MEMORY.md"})).unwrap();
        edit.execute(&json!({
            "path": "MEMORY.md",
                "edits": [{"operation": "replace", "start_line": 0, "end_line": 0, "lines": ["new memory"]}]
        }))
        .unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("MEMORY.md")).unwrap(),
            "new memory\n"
        );
    }

    #[test]
    fn directory_listing_supports_depth_metadata_sorting_and_pagination() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::create_dir_all(storage.data_dir.join("Nested")).unwrap();
        fs::write(storage.data_dir.join("Alpha.md"), "one\ntwo\n").unwrap();
        fs::write(storage.data_dir.join("Nested/Beta.txt"), "three\n").unwrap();
        let list = ListDirectory::new(directory.path()).unwrap();

        let direct: Value = serde_json::from_str(
            &list
                .execute(&json!({"path": "data", "depth": 1, "sort_by": "name"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(direct["total"], 2);
        assert_eq!(direct["entries"][0]["path"], "data/Alpha.md");
        assert_eq!(direct["entries"][0]["type"], "file");
        assert_eq!(direct["entries"][0]["depth"], 1);
        assert_eq!(direct["entries"][0]["extension"], "md");
        assert_eq!(direct["entries"][0]["line_count"], 2);
        assert_eq!(direct["entries"][0]["size"], 8);
        assert!(direct["entries"][0].get("created_at").is_some());
        assert!(direct["entries"][0]["modified_at"].is_string());
        assert_eq!(direct["entries"][1]["type"], "directory");
        assert!(direct["entries"][1]["line_count"].is_null());

        let nested: Value = serde_json::from_str(
            &list
                .execute(&json!({
                    "path": "data", "depth": 2, "sort_by": "depth",
                    "order": "desc", "offset": 0, "limit": 1
                }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(nested["total"], 3);
        assert_eq!(nested["returned"], 1);
        assert_eq!(nested["has_more"], true);
        assert_eq!(nested["scan_truncated"], false);
        assert_eq!(nested["entries"][0]["path"], "data/Nested/Beta.txt");
        assert_eq!(nested["entries"][0]["depth"], 2);

        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("External.md"), "outside\n").unwrap();
        let external: Value = serde_json::from_str(
            &list
                .execute(&json!({"path": outside.path(), "depth": 1}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(external["total"], 1);
        assert!(external["entries"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("External.md"));
    }

    #[test]
    fn note_listing_includes_metadata_and_supports_sorting_and_pagination() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(directory.path().join("data/Alpha.md"), "one\ntwo\nthree\n").unwrap();
        fs::write(directory.path().join("data/Beta.mb"), "one\n").unwrap();
        fs::write(directory.path().join("data/ignored.txt"), "ignored").unwrap();
        let list = ListNotes::new(directory.path()).unwrap();

        let result: Value = serde_json::from_str(
            &list
                .execute(&json!({
                    "sort_by": "line_count", "order": "desc", "offset": 0, "limit": 1
                }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["returned"], 1);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["entries"][0]["name"], "Alpha.md");
        assert_eq!(result["entries"][0]["line_count"], 3);
        assert!(result["entries"][0].get("created_at").is_some());
        assert!(result["entries"][0]["modified_at"].is_string());
        assert_eq!(result["entries"][0]["size"], 14);

        let by_name: Value = serde_json::from_str(
            &list
                .execute(&json!({"sort_by": "name", "order": "asc"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(by_name["entries"][0]["name"], "Alpha.md");
        assert_eq!(by_name["entries"][1]["name"], "Beta.mb");
    }

    #[test]
    fn copy_and_move_accept_external_sources_but_only_new_internal_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let copy_source = outside.path().join("copy.txt");
        let move_source = outside.path().join("move.txt");
        fs::write(&copy_source, "copy me").unwrap();
        fs::write(&move_source, "move me").unwrap();
        let canonical_move_source = fs::canonicalize(&move_source).unwrap();

        let copy = CopyFile::new(directory.path()).unwrap();
        copy.execute(&json!({
            "source": copy_source,
            "destination": "data/copied.md"
        }))
        .unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("data/copied.md")).unwrap(),
            "copy me"
        );
        assert!(copy_source.exists());
        assert!(copy
            .execute(&json!({
                "source": copy_source,
                "destination": "data/copied.md"
            }))
            .returns_err());
        assert!(copy
            .execute(&json!({
                "source": copy_source,
                "destination": "../escaped.md"
            }))
            .returns_err());

        let (event_sender, mut event_receiver) = event_channel();
        let move_file = MoveFile::new(directory.path(), event_sender).unwrap();
        move_file
            .execute(&json!({
                "source": move_source,
                "destination": "data/moved.md"
            }))
            .unwrap();
        assert!(!move_source.exists());
        assert_eq!(
            fs::read_to_string(directory.path().join("data/moved.md")).unwrap(),
            "move me"
        );
        let AgentEvent::FileMoved { from, to } = event_receiver.blocking_recv().unwrap() else {
            panic!("expected file-moved event");
        };
        assert_eq!(from, canonical_move_source);
        assert_eq!(to, PathBuf::from("data/moved.md"));
    }

    #[test]
    fn batch_move_and_rename_are_explicit_non_overwriting_tools() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let destination = directory.path().join("data/collected");
        fs::create_dir(&destination).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alpha = outside.path().join("alpha.md");
        let beta = outside.path().join("beta.md");
        fs::write(&alpha, "alpha").unwrap();
        fs::write(&beta, "beta").unwrap();

        let (event_sender, mut event_receiver) = event_channel();
        let mover = MoveFiles::new(directory.path(), event_sender.clone()).unwrap();
        let result = mover
            .execute(&json!({
                "sources": [alpha, beta],
                "destination_directory": "data/collected"
            }))
            .unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["count"], 2);
        assert!(!alpha.exists());
        assert!(!beta.exists());
        assert_eq!(
            fs::read_to_string(destination.join("alpha.md")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs::read_to_string(destination.join("beta.md")).unwrap(),
            "beta"
        );
        let moved_events = [
            event_receiver.blocking_recv().unwrap(),
            event_receiver.blocking_recv().unwrap(),
        ];
        assert!(moved_events
            .iter()
            .all(|event| matches!(event, AgentEvent::FileMoved { .. })));

        let rename = RenameFile::new(directory.path(), event_sender).unwrap();
        rename
            .execute(&json!({
                "path": "data/collected/alpha.md",
                "new_name": "renamed.md"
            }))
            .unwrap();
        assert!(!destination.join("alpha.md").exists());
        assert_eq!(
            fs::read_to_string(destination.join("renamed.md")).unwrap(),
            "alpha"
        );
        let AgentEvent::FileMoved { from, to } = event_receiver.blocking_recv().unwrap() else {
            panic!("expected file-moved event");
        };
        assert_eq!(from, PathBuf::from("data/collected/alpha.md"));
        assert_eq!(to, PathBuf::from("data/collected/renamed.md"));
        assert!(rename
            .execute(&json!({
                "path": "data/collected/renamed.md",
                "new_name": "beta.md"
            }))
            .returns_err());
    }

    #[test]
    fn delete_file_is_internal_and_waits_for_approval() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let target = directory.path().join("data/delete.md");
        fs::write(&target, "remove me\n").unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let (event_sender, mut event_receiver) = event_channel();
        let (decision_sender, decision_receiver) = tokio::sync::mpsc::unbounded_channel();
        let gate = ApprovalGate {
            bypass: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            events: event_sender,
            decisions: Arc::new(tokio::sync::Mutex::new(decision_receiver)),
        };
        let delete = DeleteFile::new(directory.path(), gate).unwrap();
        assert!(delete
            .execute(&json!({"path": outside.path()}))
            .returns_err());
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(delete.execute(&json!({"path": "data/delete.md"})))
        });
        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert_eq!(request.title, "Delete data/delete.md");
        assert!(request.diff.contains("-remove me"));
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        worker.join().unwrap().unwrap();
        assert!(!target.exists());
        assert!(outside.path().exists());
    }

    #[test]
    fn user_prompt_includes_current_local_datetime() {
        let now = Local::now();
        let prompt = prompt_with_datetime("Summarize this", now);
        assert!(prompt.starts_with("Current local date and time: "));
        assert!(prompt.contains(&now.to_rfc3339()));
        assert!(prompt.ends_with("\n\nSummarize this"));
    }

    #[test]
    fn tool_activity_uses_readable_names_and_identifying_arguments() {
        let fetch = json!({
            "name": "web_fetch",
            "input": {"url": "https://docs.example.com:8443/guide/page?q=private"}
        });
        assert_eq!(
            tool_start_activity(&fetch),
            "Fetching Web...\nhttps://docs.example.com:8443"
        );
        assert_eq!(
            tool_finish_activity(&fetch, None),
            "Completed Web Fetch.\nhttps://docs.example.com:8443"
        );

        let search = json!({
            "name": "web_search",
            "input": {"query": "Rust terminal user interface"}
        });
        assert_eq!(
            tool_start_activity(&search),
            "Searching Web...\nRust terminal user interface"
        );

        let read = json!({
            "name": "read_file",
            "input": {"path": "data/project notes.md", "offset": 100}
        });
        assert_eq!(
            tool_start_activity(&read),
            "Calling Read File...\ndata/project notes.md"
        );
        assert_eq!(
            tool_finish_activity(&read, None),
            "Completed Read File.\ndata/project notes.md"
        );
        assert_eq!(tool_display_name("add_daily_entry"), "Add Daily Entry");

        let explore = json!({
            "name": "explore",
            "input": {"task": "Find every note about context compaction"}
        });
        assert_eq!(
            tool_start_activity(&explore),
            "Calling Explore...\nFind every note about context compaction"
        );

        assert_eq!(
            tool_finish_activity(&read, Some("file not found")),
            "Failed Read File: file not found\ndata/project notes.md"
        );
    }

    #[test]
    fn tool_batch_emits_one_timeline_activity_per_call() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(&storage.ai_config_path, TEST_MESSAGES_CONFIG).unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
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
        let calls = [
            ToolCall {
                id: "notify-1".to_string(),
                name: "notify".to_string(),
                input: json!({"message": "First"}),
            },
            ToolCall {
                id: "notify-2".to_string(),
                name: "notify".to_string(),
                input: json!({"message": "Second"}),
            },
        ];

        let results =
            completed_tool_results(agent.execute_tool_batch(&calls, &HashMap::new()).unwrap());
        assert_eq!(results.len(), 2);
        let activities = drain_events(&mut event_receiver)
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::ToolStarted(text) => Some((true, text)),
                AgentEvent::ToolFinished(text) => Some((false, text)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            activities,
            [
                (true, "Calling Notify...\nFirst".to_string()),
                (false, "Completed Notify.\nFirst".to_string()),
                (true, "Calling Notify...\nSecond".to_string()),
                (false, "Completed Notify.\nSecond".to_string()),
            ]
        );
    }

    #[test]
    fn messages_api_streams_text_deltas_and_reconstructs_the_final_response() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
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
            let request = serde_json::from_slice::<Value>(&request).unwrap();
            let body = concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello \"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n"
            );
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            reader.get_mut().flush().unwrap();
            request
        });

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            &storage.ai_config_path,
            format!("api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\n"),
        )
        .unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
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
            run_agent(&agent, "Greet me", &mut conversation).unwrap(),
            "Hello world"
        );
        let request = server.join().unwrap();
        assert_eq!(request["stream"], true);
        let events = drain_events(&mut event_receiver);
        let first_text = events
            .iter()
            .position(
                |event| matches!(event, AgentEvent::AssistantDelta(delta) if delta == "Hello "),
            )
            .unwrap();
        assert!(events[..first_text]
            .iter()
            .any(|event| matches!(event, AgentEvent::Usage(usage) if usage.input_tokens == 7)));
        assert!(events[..first_text]
            .iter()
            .any(|event| matches!(event, AgentEvent::ContextWindow { tokens: 7, capacity: 200_000 })));
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::AssistantDelta(delta) if delta == "world")));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::AssistantMessageFinished {
                final_output: true,
                ..
            }
        )));
        let mut usage = TokenUsage::default();
        for event in &events {
            if let AgentEvent::Usage(delta) = event {
                usage.add(*delta);
            }
        }
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 2);
    }

    #[test]
    fn malformed_streamed_tool_input_is_returned_to_the_model_as_a_tool_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();

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
            let events = [
                json!({
                    "type": "message_start",
                    "message": {"usage": {"input_tokens": 7, "output_tokens": 0}}
                }),
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": "malformed-notify",
                        "name": "notify",
                        "input": {}
                    }
                }),
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": "{\"message\":\"hello\",}"
                    }
                }),
                json!({"type": "content_block_stop", "index": 0}),
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "tool_use"},
                    "usage": {"output_tokens": 3}
                }),
                json!({"type": "message_stop"}),
            ];
            let body = events
                .iter()
                .map(|event| format!("data: {event}\n\n"))
                .collect::<String>();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            reader.get_mut().flush().unwrap();

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
            let response = json!({
                "content": [{"type": "text", "text": "Recovered after invalid tool input"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 12, "output_tokens": 5}
            });
            let body = serde_json::to_vec(&response).unwrap();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            reader.get_mut().write_all(&body).unwrap();
            reader.get_mut().flush().unwrap();
            requests
        });

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            &storage.ai_config_path,
            format!("api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\n"),
        )
        .unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
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

        let output = run_agent(&agent, "Start", &mut AgentConversation::default()).unwrap();
        assert_eq!(output, "Recovered after invalid tool input");
        let requests = server.join().unwrap();
        let messages = requests[1]["messages"].as_array().unwrap();
        assert_eq!(messages[1]["content"][0]["input"], json!({}));
        let result = &messages[2]["content"][0];
        assert_eq!(result["tool_use_id"], "malformed-notify");
        assert_eq!(result["is_error"], true);
        assert!(result["content"]
            .as_str()
            .unwrap()
            .contains("streamed JSON was invalid: invalid JSON"));
        assert!(drain_events(&mut event_receiver)
            .into_iter()
            .any(|event| matches!(
                event,
                AgentEvent::ToolFinished(message) if message.contains("invalid JSON")
            )));
    }

    #[test]
    fn messages_api_retries_transient_status_without_double_counting_usage() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (status, body) in [
                (
                    "503 Service Unavailable",
                    json!({"error": {"message": "busy"}}),
                ),
                (
                    "200 OK",
                    json!({
                        "content": [{"type": "text", "text": "Recovered"}],
                        "stop_reason": "end_turn",
                        "usage": {
                            "input_tokens": 11,
                            "output_tokens": 3,
                            "cache_read_input_tokens": 7
                        }
                    }),
                ),
            ] {
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
                let body = serde_json::to_vec(&body).unwrap();
                write!(
                    reader.get_mut(),
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
            format!("api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\n"),
        )
        .unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
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
            run_agent(&agent, "Recover", &mut AgentConversation::default()).unwrap(),
            "Recovered"
        );
        server.join().unwrap();
        let events = drain_events(&mut event_receiver);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::Retry))
                .count(),
            1
        );
        let usages = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Usage(usage) => Some(*usage),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].input_tokens, 11);
        assert_eq!(usages[0].output_tokens, 3);
        assert_eq!(usages[0].cache_read_input_tokens, 7);
    }

    #[test]
    fn interrupted_stream_reports_confirmed_usage_without_success_timing() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
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
            let body = concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":13,\"cache_read_input_tokens\":9}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n"
            );
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len() + 50,
                body
            )
            .unwrap();
            reader.get_mut().flush().unwrap();
        });

        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(
            &storage.ai_config_path,
            format!("api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\n"),
        )
        .unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
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

        let error = run_agent(&agent, "Start", &mut AgentConversation::default()).unwrap_err();
        assert!(error.to_string().contains("reading Messages event stream"));
        server.join().unwrap();
        let events = drain_events(&mut event_receiver);
        assert!(events.iter().any(
            |event| matches!(event, AgentEvent::Usage(usage) if usage.input_tokens == 13 && usage.cache_read_input_tokens == 9)
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::ResponseTiming { .. })));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::Retry)));
    }

    #[test]
    fn denied_tool_approval_stops_the_run_and_skips_remaining_tools() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let response = json!({
            "content": [
                {
                    "type": "tool_use",
                    "id": "delete-first",
                    "name": "delete_file",
                    "input": {"path": "data/first.md"}
                },
                {
                    "type": "tool_use",
                    "id": "delete-second",
                    "name": "delete_file",
                    "input": {"path": "data/second.md"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 4}
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
        fs::write(storage.data_dir.join("first.md"), "first").unwrap();
        fs::write(storage.data_dir.join("second.md"), "second").unwrap();
        fs::write(
            &storage.ai_config_path,
            format!("api_format = 'messages'\napi_key = 'test'\nmodel = 'test-model'\nbase_url = 'http://{address}'\n"),
        )
        .unwrap();
        let (approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        approval_sender.send(ApprovalDecision::Deny).unwrap();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
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

        let completion = test_runtime()
            .block_on(agent.run("Delete both", &mut conversation))
            .unwrap();

        assert!(matches!(
            completion,
            AgentRunCompletion::Stopped(AgentStopReason::ToolApprovalDenied)
        ));
        server.join().unwrap();
        assert!(storage.data_dir.join("first.md").exists());
        assert!(storage.data_dir.join("second.md").exists());
        assert_eq!(conversation.messages.len(), 4);
        let results = conversation.messages[2..]
            .iter()
            .map(|message| match &message.parts[0] {
                MessagePart::ToolResult(result) => result,
                _ => panic!("expected tool result"),
            })
            .collect::<Vec<_>>();
        assert!(results[0].content.contains("denied by user"));
        assert!(results[1].content.contains("not executed"));
        let activities = drain_events(&mut event_receiver)
            .into_iter()
            .filter(|event| matches!(event, AgentEvent::ToolStarted(_)))
            .count();
        assert_eq!(activities, 1);
    }
