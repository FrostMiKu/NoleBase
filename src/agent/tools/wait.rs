//! Cancellation-aware asynchronous waiting for the current Agent task.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::agent::{wait_while_active, Tool};

const MAX_WAIT_SECONDS: u64 = 24 * 60 * 60;

pub struct Wait {
    cancelled: Arc<AtomicBool>,
}

impl Wait {
    pub fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self { cancelled }
    }
}

#[async_trait::async_trait]
impl Tool for Wait {
    fn name(&self) -> &'static str {
        "wait"
    }

    fn description(&self) -> &'static str {
        "Pause the current Agent task for a fixed duration before continuing. Use this when an external process, terminal command, download, build, or other work needs time to progress before checking it again. The wait is asynchronous and leaves the application and PTY responsive. Cancelling the Agent interrupts only the wait; it does not stop external work or release a PTY."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_WAIT_SECONDS,
                    "description": "Number of seconds to wait, from 1 second through 24 hours."
                }
            },
            "required": ["seconds"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, input: &Value) -> Result<String> {
        let seconds = input
            .get("seconds")
            .and_then(Value::as_u64)
            .context("field seconds must be a positive integer")?;
        if !(1..=MAX_WAIT_SECONDS).contains(&seconds) {
            bail!("field seconds must be between 1 and {MAX_WAIT_SECONDS}");
        }
        wait_while_active(&self.cancelled, Duration::from_secs(seconds)).await?;
        Ok(format!("waited {seconds} seconds"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::agent::RegisteredTool;

    #[tokio::test]
    async fn an_active_wait_stops_promptly_after_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = cancelled.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancellation.store(true, Ordering::Relaxed);
        });

        let started = std::time::Instant::now();
        let error = Wait::new(cancelled).execute(&json!({"seconds": 60})).await;

        assert_eq!(error.unwrap_err().to_string(), "agent task cancelled");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn registered_wait_enforces_its_duration_contract() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let wait = Wait::new(cancelled.clone());
        let schema = wait.input_schema();
        let registered = RegisteredTool::new(wait, &schema);

        for input in [json!({"seconds": 0}), json!({"seconds": 86_401})] {
            assert!(registered.execute(&input).await.is_err());
        }
        assert!(!cancelled.load(Ordering::Relaxed));
    }
}
