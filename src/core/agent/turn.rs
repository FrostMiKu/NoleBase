//! The shared conversation loop for Session and collaborator Agents.
use super::*;

impl Agent {
    pub(super) async fn run(
        &self,
        prompt: &str,
        conversation: &mut AgentConversation,
    ) -> Result<AgentRunCompletion> {
        // The conversation snapshot is the plan's source of truth across
        // restarts and session switches; re-arm the shared handle from it
        // before any tool or compaction can read or write it.
        self.todos.replace(conversation.todos.clone());
        let prompt = prompt_with_datetime(prompt, Local::now());
        let parsed =
            parse_user_message(prompt, &self.attachments, self.config.supports_images).await?;
        conversation.messages.push(parsed);
        self.run_conversation(conversation, None).await
    }

    pub(super) async fn run_conversation(
        &self,
        conversation: &mut AgentConversation,
        mailbox: Option<&collaboration::Collaborator>,
    ) -> Result<AgentRunCompletion> {
        self.checkpoint_conversation(conversation);
        let mut empty_response_retries = 0usize;
        let mut truncation_retries = 0usize;
        let mut round = 0u32;

        loop {
            if let Some(mailbox) = mailbox {
                mailbox.checkpoint(&mut conversation.messages)?;
            }
            let buffered = self.take_buffered_prompts()?;
            let delivered = self.jobs.take_deliveries();
            if !buffered.is_empty() || !delivered.is_empty() {
                for raw_prompt in buffered {
                    let dated = prompt_with_datetime(&raw_prompt, Local::now());
                    let parsed =
                        parse_user_message(dated, &self.attachments, self.config.supports_images)
                            .await?;
                    append_user_parts(&mut conversation.messages, parsed.parts);
                }
                if !delivered.is_empty() {
                    let frame = format_job_deliveries(&delivered);
                    append_user_parts(&mut conversation.messages, Message::user(frame).parts);
                    let image_parts = delivered
                        .iter()
                        .flat_map(|job| job.output.images.iter())
                        .flat_map(|image| {
                            [
                                MessagePart::Text {
                                    text: format!(
                                        "Image returned by background job: {}",
                                        image.label
                                    ),
                                },
                                MessagePart::Image(image.clone()),
                            ]
                        })
                        .collect::<Vec<_>>();
                    if !image_parts.is_empty() {
                        append_user_parts(&mut conversation.messages, image_parts);
                    }
                    let _ = self.emit(AgentEvent::JobDelivered(
                        delivered.iter().map(|job| job.id.clone()).collect(),
                    ));
                }
                self.checkpoint_conversation(conversation);
                round = 0;
            }
            round = round.saturating_add(1);
            self.ensure_active()?;
            let final_round = self.policy.final_round(round)?;
            if final_round {
                conversation.messages.push(Message::user(
                    "Return a complete report of the work and its outcome now.",
                ));
            }
            let definitions = if final_round {
                &[][..]
            } else {
                self.definitions.as_slice()
            };
            let _ = self.emit(AgentEvent::Round { current: round });
            if self
                .compact_context_if_needed(
                    &mut conversation.messages,
                    conversation.last_request_input_tokens,
                    definitions,
                )
                .await?
            {
                self.checkpoint_conversation(conversation);
            }
            let response = self
                .request_message(&mut conversation.messages, definitions)
                .await?;
            let reported_input_tokens = response.token_usage.total_input();
            if reported_input_tokens > 0 {
                conversation.last_request_input_tokens = reported_input_tokens;
            }
            let content = response.message.parts.clone();
            let stop_reason = response.stop_reason.clone();
            let tool_uses = response.message.tool_calls().cloned().collect::<Vec<_>>();
            let response_text = response.text();
            if tool_uses.is_empty() {
                let output = response_text;
                if !output.trim().is_empty() {
                    conversation.messages.push(response.message);
                }
                self.checkpoint_conversation(conversation);
                let buffered = self.take_buffered_prompts()?;
                if !buffered.is_empty() {
                    if !output.trim().is_empty() {
                        let _ = self.emit(AgentEvent::AssistantMessageFinished {
                            text: output.clone(),
                            final_output: false,
                        });
                    }
                    append_user_text(
                        &mut conversation.messages,
                        format_buffered_prompts(buffered),
                    );
                    self.checkpoint_conversation(conversation);
                    round = 0;
                    empty_response_retries = 0;
                    truncation_retries = 0;
                    continue;
                }
                if final_round && (stop_reason == StopReason::Length || output.trim().is_empty()) {
                    bail!("collaborator returned no complete response within its round budget");
                }
                if stop_reason == StopReason::Length {
                    if !output.trim().is_empty() {
                        let _ = self.emit(AgentEvent::AssistantMessageFinished {
                            text: output.clone(),
                            final_output: false,
                        });
                    }
                    if truncation_retries >= MAX_TRUNCATION_RETRIES {
                        bail!("{}", empty_response_diagnostic(&stop_reason, &content));
                    }
                    truncation_retries += 1;
                    append_user_text(
                        &mut conversation.messages,
                        "Continue from the previous response and provide the complete answer. Do not repeat completed work.".to_string(),
                    );
                    self.checkpoint_conversation(conversation);
                    continue;
                }
                if output.trim().is_empty() {
                    if stop_reason == StopReason::Refusal
                        || empty_response_retries >= MAX_EMPTY_RESPONSE_RETRIES
                    {
                        bail!("{}", empty_response_diagnostic(&stop_reason, &content));
                    }
                    empty_response_retries += 1;
                    append_user_text(
                        &mut conversation.messages,
                        "Provide a non-empty final answer to the user's request. If required information is missing, use ask.".to_string(),
                    );
                    self.checkpoint_conversation(conversation);
                    continue;
                }
                let _ = self.emit(AgentEvent::AssistantMessageFinished {
                    text: output.clone(),
                    final_output: true,
                });
                return Ok(AgentRunCompletion::Finished(output));
            }

            empty_response_retries = 0;
            truncation_retries = 0;

            let _ = self.emit(AgentEvent::AssistantMessageFinished {
                text: response_text,
                final_output: false,
            });

            conversation.messages.push(response.message);
            self.ensure_active()?;
            let results = self
                .execute_tool_batch(&tool_uses, &response.tool_input_errors)
                .await?;
            match results {
                ToolBatchExecution::Completed {
                    messages,
                    followup_delivered,
                } => {
                    conversation.messages.extend(messages);
                    // A completed batch is the only place the plan can change
                    // (todo is a tool), so mirror the handle into the
                    // conversation before the checkpoint persists it.
                    conversation.todos = self.todos.snapshot();
                    self.checkpoint_conversation(conversation);
                    if followup_delivered {
                        round = 0;
                    }
                }
                ToolBatchExecution::Denied(results) => {
                    conversation.messages.extend(results);
                    self.checkpoint_conversation(conversation);
                    return Ok(AgentRunCompletion::Stopped(
                        AgentStopReason::ToolApprovalDenied,
                    ));
                }
            }
        }
    }
}
