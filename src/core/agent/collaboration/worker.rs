//! Each complete reply is durably recorded and delivered before another turn starts.
use super::*;
use crate::core::agent::ToolOutput;
use std::time::Duration;

pub(super) fn spawn(
    agent: Arc<Collaborator>,
    runner: Arc<Agent>,
    jobs: AgentJobsHandle,
    job_id: String,
) -> Result<()> {
    let task_agent = agent.clone();
    let task_jobs = jobs.clone();
    let failed_job = job_id.clone();
    let spawned = std::thread::Builder::new()
        .name("collaborator".into())
        .spawn(move || {
            let mut current_job = Some(job_id);
            let outcome = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| {
                    runtime.block_on(run(&task_agent, &runner, &task_jobs, &mut current_job))
                });
            if let Err(error) = outcome {
                let mut note = task_agent.note.lock().unwrap();
                note.status = if task_agent.cancelled.load(Ordering::Relaxed)
                    || error
                        .downcast_ref::<super::super::ApprovalDenied>()
                        .is_some()
                {
                    CollaboratorStatus::Stopped
                } else {
                    CollaboratorStatus::Failed
                };
                note.error = Some(format!("{error:#}"));
                let _ = task_agent.save(&note);
                if let Some(job) = current_job {
                    task_jobs.settle(&job, Err(format!("{error:#}")));
                }
            }
        });
    if let Err(error) = spawned {
        let mut note = agent.note.lock().unwrap();
        note.status = CollaboratorStatus::Failed;
        note.error = Some(error.to_string());
        let _ = agent.save(&note);
        jobs.settle(&failed_job, Err(error.to_string()));
        return Err(error.into());
    }
    Ok(())
}

async fn run(
    agent: &Collaborator,
    runner: &Agent,
    jobs: &AgentJobsHandle,
    current_job: &mut Option<String>,
) -> Result<()> {
    let mut conversation = crate::core::agent_session::AgentConversation {
        messages: agent.snapshot().messages,
        ..Default::default()
    };
    loop {
        agent.checkpoint(&mut conversation.messages)?;
        let result = tokio::select! {
            biased;
            result = runner.run_conversation(&mut conversation, Some(agent)) => result,
            _ = cancelled(&agent.cancelled) => Err(anyhow::anyhow!("agent task cancelled")),
        };
        let mut note = agent.note.lock().unwrap();
        note.messages = conversation.messages.clone();
        anyhow::ensure!(!*agent.closed.lock().unwrap(), "Session has ended");
        let text = match result? {
            super::super::AgentRunCompletion::Finished(text) => text,
            super::super::AgentRunCompletion::Stopped(_) => {
                return Err(super::super::ApprovalDenied.into())
            }
        };
        let report = CollaboratorReport {
            sequence: note.reports.len() as u64 + 1,
            generation: note.generation,
            through_message: note.consumed_messages,
            text,
        };
        note.reports.push(report.clone());
        agent.save(&note)?;
        // The completed reply becomes visible to the parent while later steering
        // messages remain queued. Its job is settled exactly once.
        if let Some(job) = current_job.take() {
            jobs.settle(&job, Ok(ToolOutput::text(report.text)));
        }
        anyhow::ensure!(
            !agent.cancelled.load(Ordering::Relaxed),
            "agent task cancelled"
        );
        if note.inbox.is_empty() {
            note.status = CollaboratorStatus::Idle;
            agent.save(&note)?;
            return Ok(());
        }
        *current_job = Some(
            jobs.start_background(
                JobKind::Collaborator,
                &format!("Agent {} · report {}", note.name, note.reports.len() + 1),
            )?
            .id,
        );
    }
}

async fn cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
