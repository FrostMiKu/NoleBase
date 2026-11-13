use super::*;

impl Agent {
    #[cfg(test)]
    pub fn from_config(
        config_path: &Path,
        nole_root: &Path,
        runtime: AgentRuntime,
    ) -> Result<Self> {
        let source = AgentSource::load(config_path, nole_root)?;
        let client = build_http_client()?;
        Self::from_source(
            source,
            nole_root,
            runtime,
            client,
            Arc::new(SnapshotStore::default()),
            Arc::new(RegisterBank::default()),
            AttachmentUsageHandle::default(),
        )
    }

    pub(super) fn from_source(
        source: AgentSource,
        nole_root: &Path,
        runtime: AgentRuntime,
        client: Client,
        reads: Arc<SnapshotStore>,
        registers: Arc<RegisterBank>,
        attachment_usage: AttachmentUsageHandle,
    ) -> Result<Self> {
        let AgentRuntime {
            events,
            decisions,
            user_responses,
            private_terminal_input,
            input_buffer,
            permission_mode,
            cancelled,
            terminal,
            jobs: jobs_handle,
            workspace_index,
            wiki_links,
            session_directory,
            collaborators,
        } = runtime;
        let interactions = UserInteractionWaits::default();
        let AgentSource {
            config,
            agents_instructions,
            memory,
            skill_catalog,
        } = source;
        let SkillCatalog { skills, warnings } = skill_catalog;
        let tavily_api_key = config.tavily_api_key.trim().to_string();
        let has_web_search = !tavily_api_key.is_empty();
        let system = system_prompt_sections(nole_root, &agents_instructions, &skills, &memory);
        let provider: Arc<dyn Provider> = match config.api_format {
            ApiFormat::Messages => Arc::new(MessagesProvider::new(
                config.api_key.clone(),
                config.base_url.clone(),
            )?),
            ApiFormat::Completions => Arc::new(CompletionsProvider::new(
                config.api_key.clone(),
                config.base_url.clone(),
            )?),
        };
        let concurrency = ToolConcurrencyLimits::from_config(&config);
        let agent_client = client.clone();
        let attachments = AttachmentStore::new(nole_root.join(ATTACHMENTS_DIR));
        let todos = TodoHandle::default();
        let session_directory =
            session_directory.unwrap_or_else(|| nole_root.join("agents/scratch"));
        let results = SessionResultStore::in_directory(&session_directory)?;
        let tool_result_token_cap = tool_result_token_cap_for(&config);
        let mut agent = Self {
            config,
            provider,
            tools: HashMap::new(),
            definitions: Vec::new(),
            system,
            events: events.clone(),
            input_buffer,
            cancelled: cancelled.clone(),
            jobs: jobs_handle
                .clone()
                .with_result_store(results.clone(), tool_result_token_cap),
            concurrency,
            attachments,
            client: agent_client,
            todos: todos.clone(),
            results,
            interactions: interactions.clone(),
        };
        let gate = ApprovalGate::new(
            permission_mode,
            canonical_root(nole_root)?,
            cancelled.clone(),
            events,
            decisions,
            interactions.clone(),
        );
        for warning in warnings {
            let _ = agent.events.send(AgentEvent::Notification(format!(
                "Skill warning: {warning}"
            )));
        }
        agent.register(Calculate);
        agent.register(Todo::new(todos));
        agent.register(Wait::new(cancelled.clone()));
        agent.register(Shell::new(
            nole_root,
            gate.clone(),
            cancelled.clone(),
            agent.jobs.clone(),
            agent.input_buffer.clone(),
            false,
            agent.config.auto_background_threshold_seconds,
        ));
        agent.jobs.set_max_running(agent.config.max_background_jobs);
        agent.register(Jobs::new(agent.jobs.clone()));
        agent.register(JobWait::new(
            agent.jobs.clone(),
            cancelled.clone(),
            agent.input_buffer.clone(),
        ));
        agent.register(TerminalOpen::new(
            nole_root,
            gate.clone(),
            terminal.clone(),
            cancelled.clone(),
        ));
        agent.register(TerminalInput::new(
            gate.clone(),
            terminal.clone(),
            cancelled.clone(),
        ));
        agent.register(TerminalRead::new(
            terminal.clone(),
            cancelled.clone(),
            agent.jobs.clone(),
        ));
        agent.register(AskPrivate {
            events: agent.events.clone(),
            responses: private_terminal_input,
            terminal: terminal.clone(),
            cancelled: cancelled.clone(),
            interactions: interactions.clone(),
        });
        agent.register(TerminalClose::new(terminal));
        let mut read = Read::new(nole_root, reads.clone(), client.clone())?
            .with_session_directory(&session_directory);
        read.register(SkillParser::new(&skills));
        agent.register(read);
        agent.register(Http::new(nole_root, client.clone(), agent.jobs.clone())?);
        agent.register(Notes::new(nole_root)?);
        agent.register(Grep::new(nole_root)?);
        agent.register(SearchNotes::new(nole_root)?);
        agent.register(Tags::new(workspace_index.clone()));
        agent.register(SearchTag::new(nole_root, workspace_index.clone())?);
        agent.register(RenameTag::new(
            nole_root,
            workspace_index.clone(),
            gate.clone(),
        )?);
        agent.register(Wikilink::new(nole_root, wiki_links.clone())?);
        agent.register(Backlinks::new(nole_root, wiki_links.clone())?);
        agent.register(RenameWikilink::new(
            nole_root,
            wiki_links.clone(),
            gate.clone(),
        )?);
        agent.register(Write::new(nole_root, gate.clone())?);
        agent.register(Append::new(
            nole_root,
            gate.clone(),
            reads.clone(),
            registers.clone(),
        )?);
        agent.register(Copy::new(nole_root, gate.clone())?);
        agent.register(Export::new(nole_root, gate.clone())?);
        agent.register(Mkdir::new(nole_root, gate.clone())?);
        agent.register(Rmdir::new(nole_root, gate.clone())?);
        let file_events = agent.events.clone();
        agent.register(Move::new(nole_root, file_events.clone(), gate.clone())?);
        agent.register(MoveMany::new(nole_root, file_events.clone(), gate.clone())?);
        agent.register(Rename::new(nole_root, file_events, gate.clone())?);
        agent.register(Delete::new(nole_root, gate.clone())?);
        agent.register(Edit::new(nole_root, gate.clone(), reads, registers)?);
        agent.register(Daily::new(nole_root)?);
        agent.register(Open::new(nole_root, agent.events.clone())?);
        agent.register(Notify {
            events: agent.events.clone(),
        });
        agent.register(Ask {
            events: agent.events.clone(),
            responses: user_responses,
            cancelled,
            interactions,
        });
        if has_web_search {
            agent.register(SearchWeb {
                client: client.clone(),
                api_key: tavily_api_key.clone(),
            });
        }
        let subagent_runtime = subagent::SubagentRuntime::new(
            &agent.config,
            agent.provider.clone(),
            agent.system.clone(),
            agent.events.clone(),
            agent.cancelled.clone(),
            agent.concurrency.clone(),
        );
        let collaborators = collaborators
            .unwrap_or_else(|| collaboration::CollaboratorsHandle::new(&session_directory));
        collaborators.load()?;
        agent.register(Collaborate::new(
            SubagentTools {
                root: nole_root.into(),
                session_directory: session_directory.clone(),
                workspace_index,
                wiki_links,
                client,
                tavily_api_key,
                skills,
            },
            subagent_runtime,
            collaborators,
            agent.jobs.clone(),
            agent.config.max_concurrent_subagents,
        ));
        agent.register(ImportAttachment::new(nole_root)?);
        agent.register(ListAttachments::new(nole_root)?);
        agent.register(AttachmentInfo::new(nole_root)?);
        agent.register(CheckoutAttachment::new(nole_root)?);
        agent.register(UpdateAttachment::new(nole_root, gate.clone())?);
        agent.register(DeleteAttachment::new(
            nole_root,
            gate.clone(),
            attachment_usage,
        )?);
        if let Some(definition) = agent.definitions.last_mut() {
            definition.cache = true;
        }
        Ok(agent)
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
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
}
