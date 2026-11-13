use super::*;

impl AgentWorker {
    /// The shared attachment-usage state this worker's `delete_attachment`
    /// tool observes. Test-only: lets a regression test verify the app and
    /// the worker received the same handle.
    #[cfg(test)]
    pub(crate) fn attachment_usage(&self) -> &AttachmentUsageHandle {
        &self.attachment_usage
    }

    /// The shared wiki-link index this worker's wiki-link tools observe.
    /// Test-only: lets a regression test verify the app and the worker
    /// received the same handle.
    #[cfg(test)]
    pub(crate) fn wiki_links(&self) -> &WikiLinkIndexHandle {
        &self.wiki_links
    }
}

pub(super) struct AgentTask {
    prompt: String,
    conversation: AgentConversation,
    output: tokio::sync::oneshot::Sender<Result<AgentRunOutput, String>>,
    cancel: tokio_util::sync::CancellationToken,
}

impl AgentWorker {
    pub fn spawn(
        config_path: PathBuf,
        nole_root: PathBuf,
        runtime: AgentRuntime,
        attachment_usage: AttachmentUsageHandle,
    ) -> Self {
        let (tasks, receiver) = mpsc::channel::<AgentTask>();
        let cancelled = runtime.cancelled.clone();
        let events = runtime.events.clone();
        let reads = Arc::new(SnapshotStore::default());
        let worker_reads = reads.clone();
        let registers = Arc::new(RegisterBank::default());
        let worker_registers = registers.clone();
        let worker_usage = attachment_usage.clone();
        #[cfg(test)]
        let worker_wiki_links = runtime.wiki_links.clone();
        std::thread::spawn(move || {
            let async_runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("building Agent async runtime");
            let mut client: Option<Client> = None;
            let mut loaded_source: Option<AgentSource> = None;
            let mut agent: Option<Agent> = None;

            loop {
                // Sync task channel; while idle, wake at a coarse interval so
                // short `block_on` cycles keep background job futures on the
                // worker runtime advancing (downloads, settle notifications).
                // Job completion itself is broadcast as an AgentEvent the App
                // reacts to by starting a delivery run.
                let task = match receiver.recv_timeout(Duration::from_millis(250)) {
                    Ok(task) => task,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        async_runtime
                            .block_on(async { tokio::time::sleep(Duration::from_millis(1)).await });
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let AgentTask {
                    prompt,
                    conversation,
                    output,
                    cancel,
                } = task;
                let mut conversation = conversation;
                let result = (|| {
                    let source = AgentSource::load(&config_path, &nole_root)?;
                    if loaded_source.as_ref() != Some(&source) {
                        let http = match &client {
                            Some(client) => client.clone(),
                            None => {
                                let created = build_http_client()?;
                                client = Some(created.clone());
                                created
                            }
                        };
                        agent = Some(Agent::from_source(
                            source.clone(),
                            &nole_root,
                            runtime.clone(),
                            http,
                            worker_reads.clone(),
                            worker_registers.clone(),
                            worker_usage.clone(),
                        )?);
                        loaded_source = Some(source);
                    }
                    let agent = agent.as_ref().context("Agent worker was not initialized")?;
                    async_runtime.block_on(async {
                        tokio::select! {
                            result = agent.run(&prompt, &mut conversation) => result,
                            _ = cancel.cancelled() => {
                                runtime.cancelled.store(true, Ordering::Relaxed);
                                bail!("Agent task cancelled")
                            }
                        }
                    })
                })();
                let (result, stop_reason) = match result {
                    Ok(completion) => {
                        let (text, stop_reason) = match completion {
                            AgentRunCompletion::Finished(text) => (text, None),
                            AgentRunCompletion::Stopped(reason) => (String::new(), Some(reason)),
                        };
                        (
                            Ok(AgentRunOutput {
                                text,
                                conversation: conversation.clone(),
                            }),
                            stop_reason,
                        )
                    }
                    Err(error) => (Err(format!("{error:#}")), None),
                };
                if let Ok(run_output) = &result {
                    let _ = runtime.events.send(AgentEvent::ConversationUpdated(
                        run_output.conversation.clone(),
                    ));
                }
                let _ = output.send(result.clone());
                let finished = result
                    .as_ref()
                    .map(|output| output.text.clone())
                    .map_err(Clone::clone);
                let _ = runtime.events.send(if let Some(reason) = stop_reason {
                    AgentEvent::Stopped(reason)
                } else {
                    AgentEvent::Finished(finished)
                });
            }
        });
        Self {
            tasks,
            cancelled,
            reads,
            registers,
            events,
            #[cfg(test)]
            attachment_usage,
            #[cfg(test)]
            wiki_links: worker_wiki_links,
        }
    }

    pub fn run(
        &self,
        prompt: String,
        conversation: AgentConversation,
    ) -> Result<Observable<AgentRunOutput, AgentEvent>> {
        self.cancelled.store(false, Ordering::Relaxed);
        let (output, result) = tokio::sync::oneshot::channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        let events = self.events.subscribe();
        self.tasks
            .send(AgentTask {
                prompt,
                conversation,
                output,
                cancel: cancel.clone(),
            })
            .context("sending task to Agent worker")?;
        Ok(Observable {
            output: Box::pin(async move {
                match result.await.context("waiting for Agent result")? {
                    Ok(output) => Ok(output),
                    Err(error) => Err(anyhow::anyhow!(error)),
                }
            }),
            events,
            cancel,
        })
    }

    pub fn clear_read_state(&self) -> Result<()> {
        self.reads.clear()?;
        self.registers.clear()?;
        Ok(())
    }

    pub fn mark_reads_dirty(&self, paths: &[PathBuf]) -> Result<()> {
        for path in paths {
            self.reads.mark_dirty(path)?;
        }
        Ok(())
    }

    pub fn cancellation_token(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
}
