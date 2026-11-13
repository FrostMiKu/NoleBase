use super::*;

impl Agent {
    pub(super) async fn execute_tool_batch(
        &self,
        tool_uses: &[ToolCall],
        tool_input_errors: &HashMap<String, String>,
    ) -> Result<ToolBatchExecution> {
        let mut outputs = Vec::with_capacity(tool_uses.len());
        let mut buffered = Vec::new();
        let mut index = 0usize;
        while index < tool_uses.len() {
            let pending = self.take_buffered_prompts()?;
            if !pending.is_empty() {
                buffered.extend(pending);
                outputs.extend(tool_uses[index..].iter().enumerate().map(|(offset, call)| {
                    // The call never executes, so no ToolStarted/ToolFinished
                    // pair will arrive from the executor: retire its streamed
                    // preparation row here or it spins forever.
                    let _ = self.events.send(AgentEvent::ToolFinished {
                        id: call.id.clone(),
                        message: tool_abandoned_activity(&tool_call_value(call), "Deferred"),
                        preview: None,
                    });
                    (
                        index + offset,
                        ToolCallOutput::text(deferred_tool_result(call)),
                    )
                }));
                break;
            }

            let policy = self.tool_execution_policy(&tool_uses[index]);
            let end = if policy.is_concurrent() {
                tool_uses[index..]
                    .iter()
                    .position(|call| !self.tool_execution_policy(call).is_concurrent())
                    .map_or(tool_uses.len(), |offset| index + offset)
            } else {
                index + 1
            };

            let wave = &tool_uses[index..end];
            let mut executions = stream::iter(wave.iter().enumerate())
                .map(|(offset, call)| async move {
                    let execution = self
                        .execute_scheduled_tool_call(call, tool_input_errors.get(&call.id))
                        .await;
                    (index + offset, execution)
                })
                .buffer_unordered(wave.len().max(1))
                .collect::<Vec<_>>()
                .await;
            executions.sort_by_key(|(offset, _)| *offset);
            let denied = executions
                .iter()
                .any(|(_, execution)| matches!(execution, ToolCallExecution::Denied(_)));
            for (global_offset, execution) in executions {
                let output = match execution {
                    ToolCallExecution::Completed(output) | ToolCallExecution::Denied(output) => {
                        output
                    }
                };
                outputs.push((global_offset, output));
            }
            if denied {
                outputs.extend(tool_uses[end..].iter().enumerate().map(|(offset, call)| {
                    // Skipped without execution: retire the preparation row
                    // the same way the deferral path does.
                    let _ = self.events.send(AgentEvent::ToolFinished {
                        id: call.id.clone(),
                        message: tool_abandoned_activity(&tool_call_value(call), "Skipped"),
                        preview: None,
                    });
                    (
                        end + offset,
                        ToolCallOutput::text(skipped_after_denial_tool_result(call)),
                    )
                }));
                let messages = self
                    .finalize_tool_messages(tool_uses, outputs, buffered)
                    .await?;
                return Ok(ToolBatchExecution::Denied(messages));
            }
            index = end;
        }
        buffered.extend(self.take_buffered_prompts()?);
        let followup_delivered = !buffered.is_empty();
        let messages = self
            .finalize_tool_messages(tool_uses, outputs, buffered)
            .await?;
        Ok(ToolBatchExecution::Completed {
            messages,
            followup_delivered,
        })
    }

    /// Assemble a tool batch's conversation messages: every text `tool_result`
    /// in original offset order, followed by a single trailing user message
    /// carrying all image parts (each prefixed with its tool label) and any
    /// buffered user input parsed after the images.
    pub(super) async fn finalize_tool_messages(
        &self,
        tool_uses: &[ToolCall],
        mut outputs: Vec<(usize, ToolCallOutput)>,
        buffered: Vec<String>,
    ) -> Result<Vec<Message>> {
        outputs.sort_by_key(|(offset, _)| *offset);
        let mut messages = Vec::with_capacity(outputs.len() + 1);
        // Pageable bodies were already materialized before activity reporting,
        // so this layer only assembles the compact rendered results.
        for (_, output) in &outputs {
            let result = output.result.clone();
            messages.push(Message::tool(result));
        }
        let mut trailing = Vec::new();
        for (offset, output) in &outputs {
            let name = tool_uses
                .get(*offset)
                .map(|call| call.name.as_str())
                .unwrap_or("");
            for block in &output.images {
                trailing.push(MessagePart::Text {
                    text: format!("Image returned by tool {name}: {}", block.label),
                });
                trailing.push(MessagePart::Image(block.clone()));
            }
        }
        let mut buffered_parts = Vec::new();
        let mut has_buffered_image = false;
        for raw in buffered {
            let dated = prompt_with_datetime(&raw, Local::now());
            let parsed =
                parse_user_message(dated, &self.attachments, self.config.supports_images).await?;
            has_buffered_image |= parsed
                .parts
                .iter()
                .any(|part| matches!(part, MessagePart::Image(_)));
            buffered_parts.extend(parsed.parts);
        }
        if !buffered_parts.is_empty() {
            if trailing.is_empty() && !has_buffered_image {
                let text = buffered_parts
                    .iter()
                    .filter_map(|part| match part {
                        MessagePart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                messages.push(Message::user(format!(
                    "Additional user input received while you were working:\n\n{text}"
                )));
            } else {
                if !trailing.is_empty() {
                    trailing.push(MessagePart::Text {
                        text: "\n".to_string(),
                    });
                }
                trailing.push(MessagePart::Text {
                    text: "Additional user input received while you were working:".to_string(),
                });
                trailing.extend(buffered_parts);
            }
        }
        if !trailing.is_empty() {
            messages.push(Message::user_parts(trailing));
        }
        Ok(messages)
    }

    pub(super) fn tool_execution_policy(&self, call: &ToolCall) -> ToolExecutionPolicy {
        self.tools
            .get(&call.name)
            .map_or(ToolExecutionPolicy::Exclusive, |tool| {
                tool.execution_policy()
            })
    }

    pub(super) async fn execute_scheduled_tool_call(
        &self,
        call: &ToolCall,
        input_error: Option<&String>,
    ) -> ToolCallExecution {
        let permit = self
            .concurrency
            .acquire(self.tool_execution_policy(call))
            .await;
        let _ = self.events.send(AgentEvent::ToolStarted {
            id: call.id.clone(),
            message: tool_start_activity(&tool_call_value(call)),
        });
        let execution = match input_error {
            Some(error) => {
                ToolCallExecution::Completed(ToolCallOutput::text(failed_tool_result(call, error)))
            }
            None => {
                if self.config.auto_background
                    && self.config.auto_background_threshold_seconds > 0
                    && call.name != "job_wait"
                    && call.name != "jobs"
                    && self
                        .tools
                        .get(&call.name)
                        .is_some_and(|tool| tool.allows_auto_background())
                {
                    self.execute_tool_call_with_auto_background(call, permit)
                        .await
                } else {
                    let _permit = permit;
                    self.execute_tool_call_with_wait_progress(call).await
                }
            }
        };
        let output = match &execution {
            ToolCallExecution::Completed(output) | ToolCallExecution::Denied(output) => output,
        };
        let result = &output.result;
        let error = result.is_error.then_some(result.content.as_str());
        // Ask's answer already lands in the activity text, so it is not
        // duplicated as a preview line.
        let preview = (!result.is_error && call.name != "ask")
            .then(|| tool_result_preview(&result.content))
            .flatten();
        let _ = self.events.send(AgentEvent::ToolFinished {
            id: call.id.clone(),
            message: tool_finish_activity(&tool_call_value(call), error),
            preview,
        });
        execution
    }

    pub(super) async fn execute_tool_call_with_auto_background(
        &self,
        call: &ToolCall,
        permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> ToolCallExecution {
        let Some(tool) = self.tools.get(&call.name).cloned() else {
            return self.execute_tool_call(call).await;
        };
        let input = call.input.clone();
        let mut interaction_changes = self.interactions.subscribe();
        let mut task = tokio::spawn(async move {
            let _permit = permit;
            tool.execute(&input).await
        });
        let threshold = Duration::from_secs(self.config.auto_background_threshold_seconds);
        loop {
            tokio::select! {
                joined = &mut task => return self.tool_output_execution(call, joined),
                changed = interaction_changes.changed() => {
                    // A newly opened or resolved user-interaction dialog starts
                    // a fresh execution window. Time spent awaiting a person
                    // must never count toward automatic backgrounding.
                    if changed.is_err() {
                        return self.tool_output_execution(call, task.await);
                    }
                },
                _ = tokio::time::sleep(threshold) => {
                    // A permission prompt may originate inside any mutating tool,
                    // so this is a runtime-wide state rather than a tool-name
                    // allowlist. Wait for the answer, then restart the full
                    // backgrounding window for the approved execution.
                    if self.interactions.is_active() {
                        tokio::select! {
                            joined = &mut task => return self.tool_output_execution(call, joined),
                            changed = interaction_changes.changed() => {
                                if changed.is_err() {
                                    return self.tool_output_execution(call, task.await);
                                }
                            }
                        }
                        continue;
                    }
                    let started = match self.jobs.start_background(JobKind::Tool, &call.name) {
                        Ok(started) => started,
                        Err(error) => {
                            let _ = error;
                            return self.tool_output_execution(call, task.await);
                        }
                    };
                    let jobs = self.jobs.clone();
                    let job_id = started.id.clone();
                    let job_cancel = Arc::clone(&started.cancel);
                    tokio::spawn(async move {
                        let mut task = task;
                        tokio::select! {
                            joined = &mut task => {
                                let outcome = match joined {
                                    Ok(Ok(output)) => Ok(output),
                                    Ok(Err(error)) => Err(tool_error_message(&error)),
                                    Err(error) => Err(format!("tool task failed: {error}")),
                                };
                                jobs.settle(&job_id, outcome);
                            }
                            _ = async {
                                while !job_cancel.load(Ordering::Relaxed) {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                }
                            } => task.abort()
                        }
                    });
                    let content = format!(
                        "Background job: {}\nThe tool is still running. Its result will be delivered automatically.",
                        started.id
                    );
                    return ToolCallExecution::Completed(ToolCallOutput::text(ToolResult {
                        tool_use_id: call.id.clone(),
                        content,
                        is_error: false,
                    }));
                }
            }
        }
    }

    pub(super) fn tool_output_execution(
        &self,
        call: &ToolCall,
        joined: Result<Result<ToolOutput>, tokio::task::JoinError>,
    ) -> ToolCallExecution {
        match joined {
            Ok(Ok(mut output)) => {
                if !output.images.is_empty() && !self.config.supports_images {
                    return ToolCallExecution::Completed(ToolCallOutput::text(ToolResult {
                        tool_use_id: call.id.clone(),
                        content: DISABLED_IMAGE_ERROR.to_string(),
                        is_error: true,
                    }));
                }
                let images = std::mem::take(&mut output.images);
                match self.materialize_tool_output(output) {
                    Ok(content) => ToolCallExecution::Completed(ToolCallOutput::with_images(
                        ToolResult {
                            tool_use_id: call.id.clone(),
                            content,
                            is_error: false,
                        },
                        images,
                    )),
                    Err(error) => ToolCallExecution::Completed(ToolCallOutput::text(ToolResult {
                        tool_use_id: call.id.clone(),
                        content: tool_error_message(&error),
                        is_error: true,
                    })),
                }
            }
            Ok(Err(error)) => {
                let denied = error.downcast_ref::<ApprovalDenied>().is_some();
                let output = ToolCallOutput::text(ToolResult {
                    tool_use_id: call.id.clone(),
                    content: tool_error_message(&error),
                    is_error: true,
                });
                if denied {
                    ToolCallExecution::Denied(output)
                } else {
                    ToolCallExecution::Completed(output)
                }
            }
            Err(error) => ToolCallExecution::Completed(ToolCallOutput::text(ToolResult {
                tool_use_id: call.id.clone(),
                content: format!("tool task failed: {error}"),
                is_error: true,
            })),
        }
    }

    pub(super) async fn execute_tool_call_with_wait_progress(
        &self,
        call: &ToolCall,
    ) -> ToolCallExecution {
        // `Wait` has no output to stream while it runs, so the activity row
        // instead shows a live per-second countdown via AgentEvent::ToolUpdated.
        if call.name != "wait" {
            return self.execute_tool_call(call).await;
        }
        let Some(seconds) = call.input.get("seconds").and_then(Value::as_u64) else {
            return self.execute_tool_call(call).await;
        };
        if seconds == 0 {
            return self.execute_tool_call(call).await;
        }
        let id = call.id.clone();
        let events = self.events.clone();
        let ticking = tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let remaining = seconds.saturating_sub(started.elapsed().as_secs());
                if remaining == 0 {
                    break;
                }
                let _ = events.send(AgentEvent::ToolUpdated {
                    id: id.clone(),
                    message: tool_wait_progress_activity(remaining),
                });
            }
        });
        let execution = self.execute_tool_call(call).await;
        ticking.abort();
        execution
    }

    pub(super) async fn execute_tool_call(&self, call: &ToolCall) -> ToolCallExecution {
        let id = call.id.as_str();
        let name = call.name.as_str();
        let input = &call.input;
        if let Err(error) = self.ensure_active() {
            return ToolCallExecution::Completed(ToolCallOutput::text(ToolResult {
                tool_use_id: id.to_string(),
                content: error.to_string(),
                is_error: true,
            }));
        }
        let result = self.tools.get(name).cloned().context("unknown tool");
        let result = match result {
            Ok(tool) => tool.execute(input).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(mut output) => {
                if !output.images.is_empty() && !self.config.supports_images {
                    ToolCallExecution::Completed(ToolCallOutput::text(ToolResult {
                        tool_use_id: id.to_string(),
                        content: DISABLED_IMAGE_ERROR.to_string(),
                        is_error: true,
                    }))
                } else {
                    let images = std::mem::take(&mut output.images);
                    match self.materialize_tool_output(output) {
                        Ok(content) => ToolCallExecution::Completed(ToolCallOutput::with_images(
                            ToolResult {
                                tool_use_id: id.to_string(),
                                content,
                                is_error: false,
                            },
                            images,
                        )),
                        Err(error) => {
                            ToolCallExecution::Completed(ToolCallOutput::text(ToolResult {
                                tool_use_id: id.to_string(),
                                content: tool_error_message(&error),
                                is_error: true,
                            }))
                        }
                    }
                }
            }
            Err(error) => {
                let denied = error.downcast_ref::<ApprovalDenied>().is_some();
                let result = ToolCallOutput::text(ToolResult {
                    tool_use_id: id.to_string(),
                    content: tool_error_message(&error),
                    is_error: true,
                });
                if denied {
                    ToolCallExecution::Denied(result)
                } else {
                    ToolCallExecution::Completed(result)
                }
            }
        }
    }

    pub(super) fn materialize_tool_output(&self, output: ToolOutput) -> Result<String> {
        externalize_tool_output_with(&self.results, self.tool_result_token_cap(), output)
            .map(|output| render_tool_output(&output))
    }
}
