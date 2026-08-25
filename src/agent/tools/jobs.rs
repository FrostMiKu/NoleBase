//! Background-job inspection and blocking wait.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::agent::{AgentJobsHandle, JobStatus, Tool};

/// Poll cadence for `job_wait`.
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct Jobs {
    jobs: AgentJobsHandle,
}

impl Jobs {
    pub fn new(jobs: AgentJobsHandle) -> Self {
        Self { jobs }
    }
}

#[async_trait::async_trait]
impl Tool for Jobs {
    fn name(&self) -> &'static str {
        "jobs"
    }

    fn description(&self) -> &'static str {
        "Inspect and control background jobs (backgrounded shell commands, downloads, terminal watches). `list` returns every job with its id, label, status, and elapsed time; settled jobs stay listed for a while then expire. `cancel` stops one running job by id. Background jobs keep running when the Agent is interrupted; only `cancel` or clearing the Agent session stops them."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "cancel"],
                    "description": "`list` every job or `cancel` one running job."
                },
                "job": {
                    "type": "string",
                    "description": "Job id for `cancel`, e.g. `job-3`."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn execution_policy(&self) -> crate::agent::ToolExecutionPolicy {
        crate::agent::ToolExecutionPolicy::LocalRead
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let action = input
            .get("action")
            .and_then(Value::as_str)
            .context("field action must be \"list\" or \"cancel\"")?;
        match action {
            "list" => {
                let rows = self.jobs.rows();
                Ok(serde_json::to_string_pretty(&crate::agent::rows_value(
                    &rows,
                ))?)
            }
            "cancel" => {
                let job = input
                    .get("job")
                    .and_then(Value::as_str)
                    .context("field job is required for cancel")?;
                if self.jobs.cancel(job) {
                    Ok(format!("cancel requested for {job}"))
                } else {
                    bail!("no running job named {job}")
                }
            }
            other => bail!("unknown jobs action: {other}"),
        }
    }
}

pub struct JobWait {
    jobs: AgentJobsHandle,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    input_buffer: Arc<std::sync::Mutex<Vec<String>>>,
}

impl JobWait {
    pub fn new(
        jobs: AgentJobsHandle,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
        input_buffer: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> Self {
        Self {
            jobs,
            cancelled,
            input_buffer,
        }
    }

    fn has_buffered_prompts(&self) -> bool {
        self.input_buffer
            .lock()
            .map(|buffer| !buffer.is_empty())
            .unwrap_or(false)
    }
}

#[async_trait::async_trait]
impl Tool for JobWait {
    fn name(&self) -> &'static str {
        "job_wait"
    }

    fn description(&self) -> &'static str {
        "Block until one background job settles, or until the timeout. Only use it when a later step in this turn needs the job's result before any other action—never just to await completion. Ending the turn is the normal way to wait: the result arrives automatically as a [background job] frame that wakes the Agent. The wait suppresses that job's automatic delivery and returns its result directly. If a new user message arrives mid-wait the wait returns immediately with the job still running so the message can be handled."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "job": {
                    "type": "string",
                    "description": "Job id to wait for, e.g. `job-3`."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600,
                    "description": "Maximum wait; defaults to 60."
                }
            },
            "required": ["job"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let job = input
            .get("job")
            .and_then(Value::as_str)
            .context("field job must be a job id")?
            .to_string();
        let timeout_seconds = input
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(60);
        let Some(row) = self.jobs.suppress(&job) else {
            bail!("no job named {job}");
        };
        if row.status.is_settled() {
            // Settled while unsuppressed before this call: the result was
            // already delivered. Report the row state.
            return Ok(serde_json::to_string_pretty(&json!({
                "job": row.id,
                "status": row.status.as_str(),
                "note": "already settled and delivered"
            }))?);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_seconds);
        loop {
            if self.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                self.jobs.resume(&job);
                bail!("agent task cancelled");
            }
            let rows = self.jobs.rows();
            if let Some(row) = rows.iter().find(|row| row.id == job) {
                if row.status.is_settled() {
                    let result = self.jobs.take_result(&job).unwrap_or_default();
                    return Ok(serde_json::to_string_pretty(&json!({
                        "job": row.id,
                        "status": row.status.as_str(),
                        "result": result,
                    }))?);
                }
            } else {
                // Evicted mid-wait (session cleared).
                bail!("job {job} is no longer listed");
            }
            if std::time::Instant::now() >= deadline {
                self.jobs.resume(&job);
                return Ok(serde_json::to_string_pretty(&json!({
                    "job": job,
                    "status": JobStatus::Running.as_str(),
                    "elapsed_seconds": rows
                        .iter()
                        .find(|row| row.id == job)
                        .map(|row| row.elapsed.as_secs())
                        .unwrap_or(0),
                    "note": "timed out waiting; the job keeps running"
                }))?);
            }
            if self.has_buffered_prompts() {
                self.jobs.resume(&job);
                return Ok(serde_json::to_string_pretty(&json!({
                    "job": job,
                    "status": JobStatus::Running.as_str(),
                    "note": "returned early for an incoming user message; the job keeps running"
                }))?);
            }
            tokio::time::sleep(WAIT_POLL_INTERVAL).await;
        }
    }
}
