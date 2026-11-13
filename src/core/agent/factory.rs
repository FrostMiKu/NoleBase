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
            mutation_lock,
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
        let environment = environment::AgentEnvironment {
            config,
            provider,
            system,
            events: events.clone(),
            concurrency,
            attachments,
            client: agent_client,
            results,
        };
        let mut agent = environment.create(cancelled.clone(), environment::RunPolicy::Session);
        agent.input_buffer = input_buffer;
        agent.jobs = jobs_handle.with_result_store(agent.results.clone(), tool_result_token_cap);
        agent.todos = todos.clone();
        agent.interactions = interactions.clone();
        let gate = ApprovalGate::new(
            permission_mode,
            canonical_root(nole_root)?,
            cancelled.clone(),
            events,
            decisions,
            interactions.clone(),
        );
        let mutations = tools::mutations::MutationTools {
            root: nole_root.into(),
            workspace_index: workspace_index.clone(),
            wiki_links: wiki_links.clone(),
            attachment_usage,
            gate: gate.clone(),
            lock: mutation_lock,
            cancelled: cancelled.clone(),
        };
        let agent_tools = AgentTools {
            root: nole_root.into(),
            session_directory: session_directory.clone(),
            workspace_index,
            wiki_links,
            client: client.clone(),
            tavily_api_key: tavily_api_key.clone(),
            skills,
            mutations: mutations.clone(),
            jobs: agent.jobs.clone(),
        };
        for warning in warnings {
            let _ = agent.events.send(AgentEvent::Notification(format!(
                "Skill warning: {warning}"
            )));
        }
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
        agent_tools.register_inspection(&mut agent, reads.clone())?;
        mutations.register(&mut agent, reads, registers)?;
        agent.register(Http::new(nole_root, client.clone(), agent.jobs.clone())?);
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
        let collaborators = collaborators
            .unwrap_or_else(|| collaboration::CollaboratorsHandle::new(&session_directory));
        collaborators.load()?;
        agent.register(Collaborate::new(
            agent_tools,
            environment,
            collaborators,
            agent.jobs.clone(),
            agent.config.max_concurrent_subagents,
        ));
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
        let last = self.definitions.len().saturating_sub(1);
        for (index, definition) in self.definitions.iter_mut().enumerate() {
            definition.cache = index == last;
        }
        self.tools
            .insert(name, Arc::new(RegisteredTool::new(tool, &schema)));
    }
}
