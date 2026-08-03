    use super::*;
    use super::test_support::*;

    fn file_snapshot_tag(path: impl AsRef<std::path::Path>) -> String {
        snapshot_tag(&fs::read_to_string(path).unwrap())
    }

    #[test]
    fn agent_config_defaults_to_twenty_five_rounds_and_validates_overrides() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ai.toml");
        fs::write(&path, TEST_MESSAGES_CONFIG).unwrap();
        let config = AgentConfig::load(&path).unwrap();
        assert_eq!(config.max_rounds, 25);
        assert_eq!(config.max_tokens, 8192);
        assert_eq!(config.context_window_tokens, 200_000);
        assert_eq!(config.max_concurrent_local_reads, 8);
        assert_eq!(config.max_concurrent_network_tools, 8);
        assert_eq!(config.max_concurrent_subagents, 4);

        fs::write(
            &path,
            format!(
                "{TEST_MESSAGES_CONFIG}max_rounds = 40\nmax_concurrent_local_reads = 3\nmax_concurrent_network_tools = 5\nmax_concurrent_subagents = 2\n"
            ),
        )
        .unwrap();
        let configured = AgentConfig::load(&path).unwrap();
        assert_eq!(configured.max_rounds, 40);
        let concurrency = ToolConcurrencyLimits::from_config(&configured);
        assert_eq!(concurrency.local_reads.available_permits(), 3);
        assert_eq!(concurrency.network.available_permits(), 5);
        assert_eq!(concurrency.subagents.available_permits(), 2);

        fs::write(&path, format!("{TEST_MESSAGES_CONFIG}max_rounds = 0\n")).unwrap();
        assert!(AgentConfig::load(&path)
            .unwrap_err()
            .to_string()
            .contains("max_rounds must be greater than zero"));

        for field in [
            "max_concurrent_local_reads",
            "max_concurrent_network_tools",
            "max_concurrent_subagents",
        ] {
            fs::write(&path, format!("{TEST_MESSAGES_CONFIG}{field} = 0\n")).unwrap();
            assert!(
                AgentConfig::load(&path)
                    .unwrap_err()
                    .to_string()
                    .contains(&format!("{field} must be greater than zero")),
                "missing validation for {field}"
            );
        }

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
                name: "read".to_string(),
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
        assert!((total.cache_hit_percent().unwrap() - 71.428_571).abs() < 0.000_001);
        assert_eq!(TokenUsage::default().cache_hit_percent(), None);
    }

    #[test]
    fn untimed_provider_output_does_not_inflate_stream_throughput() {
        let (events, mut receiver) = event_channel();
        let mut reported_usage = TokenUsage::default();
        let mut reported_duration = Duration::ZERO;

        report_provider_metrics(
            &events,
            &mut reported_usage,
            &mut reported_duration,
            TokenUsage {
                input_tokens: 10,
                output_tokens: 4,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            Duration::ZERO,
            100,
        );

        let events = drain_events(&mut receiver);
        assert!(events.iter().any(|event| matches!(event, AgentEvent::Usage(_))));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::ResponseTiming { .. })));
    }

    #[test]
    fn file_tools_stay_inside_root() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("data")).unwrap();
        let write = Write::new(directory.path()).unwrap();
        write
            .execute(&json!({"path": "data/test.md", "content": "hello\nworld\n"}))
            .unwrap();
        let read = Read::new(
            directory.path(),
            Arc::new(ReadTracker::default()),
            build_http_client().unwrap(),
        )
        .unwrap();
        let result = read
            .execute(&json!({"path": "data/test.md"}))
            .unwrap();
        assert_eq!(
            result,
            format!(
                "[data/test.md#{}]\n1:hello\n2:world\n\n[Showing lines 1-2 of 2]",
                snapshot_tag("hello\nworld\n")
            )
        );
        let outside_directory = tempfile::tempdir().unwrap();
        let outside = outside_directory.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        let result = read.execute(&json!({"path": outside})).unwrap();
        let header = result.lines().next().unwrap();
        assert!(header.starts_with("[/") && header.contains("outside.txt#") && header.ends_with(']'));
        assert!(result.contains("\n1:outside\n"));
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
        let read = Read::new(directory.path(), reads.clone(), build_http_client().unwrap()).unwrap();
        let edit = Edit::new(directory.path(), bypass_gate(), reads).unwrap();

        let first = read
            .execute(&json!({"path": "data/large.md:1-200"}))
            .unwrap();
        assert!(first.starts_with("[data/large.md#"));
        assert!(first.contains("\n1:line 0\n"));
        assert!(first.ends_with(
            "[Showing lines 1-200 of 450. Continue with data/large.md:201-400]"
        ));
        assert!(edit
            .execute(&json!({
                "path": "data/large.md",
                "tag": snapshot_tag(&content),
                "edits": [{"operation": "insert", "line": 450, "position": "after", "lines": ["done"]}]
            }))
            .returns_err());

        let last = read
            .execute(&json!({"path": "data/large.md:401-450"}))
            .unwrap();
        assert_eq!(first.lines().next(), last.lines().next());
        assert!(last.ends_with("[Showing lines 401-450 of 450]"));
        edit.execute(&json!({
            "path": "data/large.md",
            "tag": snapshot_tag(&content),
            "edits": [{"operation": "insert", "line": 450, "position": "after", "lines": ["done"]}]
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
        let read = Read::new(directory.path(), reads.clone(), build_http_client().unwrap()).unwrap();
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
                .execute(&json!({"query": "rust", "range": "1-1"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["range"], "1-1");
        assert_eq!(result["returned"], 1);
        assert_eq!(result["total"], 3);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["next"], "2-2");
        assert!(result["items"][0]["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("daily/") && path.ends_with(".md")));
        assert_eq!(result["items"][0]["line"], 1);

        let remaining: Value = serde_json::from_str(
            &content
                .execute(&json!({"query": "rust", "range": "2-11"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(remaining["items"][0]["path"], "data/RustProject.md");
        assert_eq!(remaining["items"][1]["path"], "archives/RustHistory.md");

        let files = SearchFiles::new(directory.path()).unwrap();
        let result: Value = serde_json::from_str(
            &files
                .execute(&json!({"query": "rsprj", "range": "1-10"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["items"][0]["path"], "data/RustProject.md");
        let archived: Value = serde_json::from_str(
            &files
                .execute(&json!({"query": "history", "range": "1-10"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(archived["total"], 1);
        assert_eq!(archived["items"][0]["path"], "archives/RustHistory.md");
    }

    #[test]
    fn private_config_is_not_available_but_daily_files_are_readable() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("config")).unwrap();
        fs::create_dir(directory.path().join("daily")).unwrap();
        fs::write(directory.path().join("config/ai.toml"), "api_key='secret'").unwrap();
        fs::write(directory.path().join("daily/2026-07-27.md"), "daily entry").unwrap();
        let read = Read::new(directory.path(), Arc::new(ReadTracker::default()), build_http_client().unwrap()).unwrap();
        assert!(read
            .execute(&json!({"path": "config/ai.toml"}))
            .returns_err());
        let daily = read
            .execute(&json!({"path": "daily/2026-07-27.md"}))
            .unwrap();
        assert!(daily.starts_with("[daily/2026-07-27.md#"));
        assert!(daily.contains("\n1:daily entry\n"));
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
        let read = Read::new(directory.path(), reads.clone(), build_http_client().unwrap()).unwrap();
        let edit = Edit::new(directory.path(), bypass_gate(), reads).unwrap();
        let write = Write::new(directory.path()).unwrap();

        read.execute(&json!({"path": "config/AGENTS.md"})).unwrap();
        assert!(edit
            .execute(&json!({
                "path": "config/AGENTS.md",
                "tag": file_snapshot_tag(directory.path().join("config/AGENTS.md")),
                "edits": [{"operation": "replace", "start_line": 1, "end_line": 1, "lines": ["changed"]}]
            }))
            .returns_err());
        assert!(write
            .execute(&json!({"path": "config/new.md", "content": "forbidden"}))
            .returns_err());
        assert!(Delete::new(directory.path(), bypass_gate())
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
            "tag": file_snapshot_tag(directory.path().join("themes/custom.toml")),
                "edits": [{"operation": "replace", "start_line": 2, "end_line": 2, "lines": ["action = \"#010203\""]}]
        }))
        .unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("themes/custom.toml")).unwrap(),
            "[ui]\naction = \"#010203\"\n"
        );

        read.execute(&json!({"path": "MEMORY.md"})).unwrap();
        edit.execute(&json!({
            "path": "MEMORY.md",
            "tag": file_snapshot_tag(directory.path().join("MEMORY.md")),
                "edits": [{"operation": "replace", "start_line": 1, "end_line": 1, "lines": ["new memory"]}]
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
        let list = Read::new(
            directory.path(),
            Arc::new(ReadTracker::default()),
            build_http_client().unwrap(),
        )
        .unwrap();

        let direct: Value = serde_json::from_str(
            &list
                .execute(&json!({"path": "data", "depth": 1, "sort_by": "name"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(direct["total"], 2);
        assert_eq!(direct["items"][0]["path"], "data/Alpha.md");
        assert_eq!(direct["items"][0]["type"], "file");
        assert_eq!(direct["items"][0]["depth"], 1);
        assert_eq!(direct["items"][0]["extension"], "md");
        assert_eq!(direct["items"][0]["line_count"], 2);
        assert_eq!(direct["items"][0]["size"], 8);
        assert!(direct["items"][0].get("created_at").is_some());
        assert!(direct["items"][0]["modified_at"].is_string());
        assert_eq!(direct["items"][1]["type"], "directory");
        assert!(direct["items"][1]["line_count"].is_null());

        let nested: Value = serde_json::from_str(
            &list
                .execute(&json!({
                    "path": "data", "depth": 2, "sort_by": "depth",
                    "order": "desc", "range": "1-1"
                }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(nested["total"], 3);
        assert_eq!(nested["returned"], 1);
        assert_eq!(nested["has_more"], true);
        assert_eq!(nested["next"], "2-2");
        assert_eq!(nested["items"][0]["path"], "data/Nested/Beta.txt");
        assert_eq!(nested["items"][0]["depth"], 2);

        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("External.md"), "outside\n").unwrap();
        let external: Value = serde_json::from_str(
            &list
                .execute(&json!({"path": outside.path(), "depth": 1}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(external["total"], 1);
        assert!(external["items"][0]["path"]
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
                    "sort_by": "line_count", "order": "desc", "range": "1-1"
                }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["returned"], 1);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["next"], "2-2");
        assert_eq!(result["items"][0]["name"], "Alpha.md");
        assert_eq!(result["items"][0]["line_count"], 3);
        assert!(result["items"][0].get("created_at").is_some());
        assert!(result["items"][0]["modified_at"].is_string());
        assert_eq!(result["items"][0]["size"], 14);

        let by_name: Value = serde_json::from_str(
            &list
                .execute(&json!({"sort_by": "name", "order": "asc"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(by_name["items"][0]["name"], "Alpha.md");
        assert_eq!(by_name["items"][1]["name"], "Beta.mb");
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

        let copy = Copy::new(directory.path()).unwrap();
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
        let move_file = Move::new(directory.path(), event_sender, bypass_gate()).unwrap();
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
        let mover = MoveMany::new(directory.path(), event_sender.clone(), bypass_gate()).unwrap();
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

        let rename = Rename::new(directory.path(), event_sender, bypass_gate()).unwrap();
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
        let delete = Delete::new(directory.path(), gate).unwrap();
        assert!(delete
            .execute(&json!({"path": outside.path()}))
            .returns_err());
        let worker = std::thread::spawn(move || {
            test_runtime().block_on(delete.execute(&json!({"path": "data/delete.md"})))
        });
        let AgentEvent::Approval(request) = event_receiver.blocking_recv().unwrap() else {
            panic!("expected approval request");
        };
        assert_eq!(request.kind, ApprovalKind::Confirm);
        assert_eq!(request.title, "Delete file");
        assert_eq!(request.message, "Delete data/delete.md?");
        decision_sender.send(ApprovalDecision::Approve).unwrap();
        worker.join().unwrap().unwrap();
        assert!(!target.exists());
        assert!(outside.path().exists());
    }

    #[test]
    fn attachment_tools_are_registered_with_canonical_names() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(&storage.ai_config_path, TEST_MESSAGES_CONFIG).unwrap();
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
        let names = agent
            .definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect::<Vec<_>>();
        for name in [
            "import_attachment",
            "list_attachments",
            "attachment_info",
            "checkout_attachment",
            "update_attachment",
            "delete_attachment",
        ] {
            assert!(names.contains(&name), "missing registered tool {name}");
        }
        assert!(!names.contains(&"copy_attachment_to_workspace"));
    }

    #[test]
    fn system_prompt_describes_the_attachment_publish_and_edit_workflow() {
        let prompt = system_prompt(Path::new("/tmp/nole"), false, "", "");
        assert!(prompt.contains("import_attachment"));
        assert!(prompt.contains("checkout_attachment"));
        assert!(prompt.contains("update_attachment"));
        assert!(prompt.contains("expected_content_token"));
        assert!(prompt.contains("workspace/main"));
        assert!(prompt.contains("nole://attachment/<uuid>"));
        assert!(prompt.contains("delete_attachment"));
        assert!(prompt.contains("importing the same content twice yields two distinct attachments"));
    }

    #[test]
    fn system_prompt_assigns_review_scope_to_the_main_agent() {
        let prompt = system_prompt(Path::new("/tmp/nole"), false, "", "");
        let explore_bullet = prompt
            .find("Delegate broad, multi-step exploration")
            .unwrap();
        let review_bullet = prompt
            .find("Delegate independent critical evaluation")
            .unwrap();
        assert!(review_bullet > explore_bullet);
        assert!(prompt[review_bullet..]
            .contains("The main Agent defines the review scope"));
        assert!(prompt[review_bullet..]
            .contains("instead of imposing a preset domain checklist"));
        assert!(prompt[review_bullet..].contains("never mutates anything"));
        assert!(!prompt[explore_bullet..review_bullet].contains("review"));
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
            "name": "read",
            "input": {"path": "https://docs.example.com:8443/guide/page?q=private"}
        });
        assert_eq!(
            tool_start_activity(&fetch),
            "Fetching Web...\nhttps://docs.example.com:8443"
        );
        assert_eq!(
            tool_finish_activity(&fetch, None),
            "Completed Read.\nhttps://docs.example.com:8443"
        );

        let search = json!({
            "name": "search_web",
            "input": {"query": "Rust terminal user interface"}
        });
        assert_eq!(
            tool_start_activity(&search),
            "Searching Web...\nRust terminal user interface"
        );

        let read = json!({
            "name": "read",
            "input": {"path": "data/project notes.md:101-300"}
        });
        assert_eq!(
            tool_start_activity(&read),
            "Calling Read...\ndata/project notes.md:101-300"
        );
        assert_eq!(
            tool_finish_activity(&read, None),
            "Completed Read.\ndata/project notes.md:101-300"
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

        let review = json!({
            "name": "review",
            "input": {"task": "Critique the draft migration plan"}
        });
        assert_eq!(
            tool_start_activity(&review),
            "Calling Review...\nCritique the draft migration plan"
        );

        assert_eq!(
            tool_finish_activity(&read, Some("file not found")),
            "Failed Read: file not found\ndata/project notes.md:101-300"
        );
    }

    #[test]
    fn tool_result_preview_withholds_json_and_compacts_text() {
        assert_eq!(tool_result_preview(""), None);
        assert_eq!(tool_result_preview("   \n  "), None);
        assert_eq!(tool_result_preview("{\"key\": 1}"), None);
        assert_eq!(tool_result_preview("[1, 2, 3]"), None);
        assert_eq!(
            tool_result_preview("  line one\nline two  "),
            Some("line one line two".to_string())
        );
        let long = "word ".repeat(60);
        let preview = tool_result_preview(&long).unwrap();
        assert!(preview.chars().count() <= 120);
        assert!(preview.ends_with('…'));
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
                AgentEvent::ToolStarted { message, .. } => Some((true, message)),
                AgentEvent::ToolFinished { message, .. } => Some((false, message)),
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

    struct ConcurrentProbe {
        barrier: Arc<tokio::sync::Barrier>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        maximum: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tool for ConcurrentProbe {
        fn name(&self) -> &'static str {
            "concurrent_probe"
        }

        fn description(&self) -> &'static str {
            "Test-only concurrent tool"
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn execution_policy(&self) -> ToolExecutionPolicy {
            ToolExecutionPolicy::Subagent
        }

        async fn execute(&self, input: &Value) -> Result<String> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(input["label"].as_str().unwrap().to_string())
        }
    }

    struct ExclusiveProbe {
        concurrent_active: Arc<std::sync::atomic::AtomicUsize>,
        executed: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Tool for ExclusiveProbe {
        fn name(&self) -> &'static str {
            "exclusive_probe"
        }

        fn description(&self) -> &'static str {
            "Test-only exclusive tool"
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }

        async fn execute(&self, _input: &Value) -> Result<String> {
            assert_eq!(self.concurrent_active.load(Ordering::SeqCst), 0);
            self.executed.store(true, Ordering::SeqCst);
            Ok("exclusive".to_string())
        }
    }

    #[test]
    fn tool_batch_runs_safe_calls_concurrently_and_keeps_exclusive_barriers() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        fs::write(&storage.ai_config_path, TEST_MESSAGES_CONFIG).unwrap();
        let (_approval_sender, approval_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (_user_sender, user_receiver) = tokio::sync::mpsc::unbounded_channel();
        let (event_sender, _event_receiver) = event_channel();
        let mut agent = Agent::from_config(
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
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let maximum = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let exclusive_executed = Arc::new(AtomicBool::new(false));
        agent.register(ConcurrentProbe {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            active: active.clone(),
            maximum: maximum.clone(),
        });
        agent.register(ExclusiveProbe {
            concurrent_active: active,
            executed: exclusive_executed.clone(),
        });
        let calls = [
            ToolCall {
                id: "parallel-a".to_string(),
                name: "concurrent_probe".to_string(),
                input: json!({"label": "first"}),
            },
            ToolCall {
                id: "parallel-b".to_string(),
                name: "concurrent_probe".to_string(),
                input: json!({"label": "second"}),
            },
            ToolCall {
                id: "exclusive".to_string(),
                name: "exclusive_probe".to_string(),
                input: json!({}),
            },
        ];

        let execution = test_runtime()
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(1),
                    agent.execute_tool_batch(&calls, &HashMap::new()),
                )
                .await
            })
            .expect("concurrent wave timed out")
            .unwrap();
        let results = completed_tool_results(execution);

        assert_eq!(maximum.load(Ordering::SeqCst), 2);
        assert!(exclusive_executed.load(Ordering::SeqCst));
        let contents = results
            .iter()
            .map(|message| match &message.parts[0] {
                MessagePart::ToolResult(result) => result.content.as_str(),
                _ => panic!("expected tool result"),
            })
            .collect::<Vec<_>>();
        assert_eq!(contents, ["first", "second", "exclusive"]);
    }

    #[test]
    fn tool_concurrency_limits_bound_the_complete_agent_tree() {
        test_runtime().block_on(async {
            for (policy, limit) in [
                (
                    ToolExecutionPolicy::Network,
                    DEFAULT_MAX_CONCURRENT_NETWORK_TOOLS,
                ),
                (
                    ToolExecutionPolicy::Subagent,
                    DEFAULT_MAX_CONCURRENT_SUBAGENTS,
                ),
            ] {
                let concurrency = ToolConcurrencyLimits::new(
                    DEFAULT_MAX_CONCURRENT_LOCAL_READS,
                    DEFAULT_MAX_CONCURRENT_NETWORK_TOOLS,
                    DEFAULT_MAX_CONCURRENT_SUBAGENTS,
                );
                let mut permits = Vec::new();
                for _ in 0..limit {
                    permits.push(concurrency.acquire(policy).await.unwrap());
                }
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(10),
                        concurrency.acquire(policy)
                    )
                    .await
                    .is_err(),
                    "{policy:?} exceeded its configured limit"
                );
                permits.pop();
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(100),
                        concurrency.acquire(policy)
                    )
                    .await
                    .unwrap()
                    .is_some()
                );
            }
        });
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
            .any(|event| matches!(event, AgentEvent::ContextWindow { tokens: 7, capacity: 191_808 })));
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
                AgentEvent::ToolFinished { message, .. } if message.contains("invalid JSON")
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
    fn interrupted_stream_retries_and_recovers() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) =
                        line.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut request = vec![0; content_length];
                reader.read_exact(&mut request).unwrap();
                if attempt == 0 {
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
                } else {
                    let body = serde_json::to_vec(&json!({
                        "content": [{"type": "text", "text": "Recovered"}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 13, "output_tokens": 3}
                    }))
                    .unwrap();
                    write!(
                        reader.get_mut(),
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    reader.get_mut().write_all(&body).unwrap();
                }
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
            run_agent(&agent, "Start", &mut AgentConversation::default()).unwrap(),
            "Recovered"
        );
        server.join().unwrap();
        let events = drain_events(&mut event_receiver);
        assert!(events.iter().any(
            |event| matches!(event, AgentEvent::Usage(usage) if usage.input_tokens == 13 && usage.cache_read_input_tokens == 9)
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::ResponseTiming { .. })));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::Retry))
                .count(),
            1
        );
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
                    "name": "delete",
                    "input": {"path": "data/first.md"}
                },
                {
                    "type": "tool_use",
                    "id": "delete-second",
                    "name": "delete",
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
            .filter(|event| matches!(event, AgentEvent::ToolStarted { .. }))
            .count();
        assert_eq!(activities, 1);
    }
