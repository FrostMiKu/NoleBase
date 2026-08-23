//! Background job registry: long-running tool work (shell commands, downloads,
//! terminal watches) that outlives the tool call and delivers its result back
//! into the conversation as a framed user message when it settles.
//!
//! Delivery model: a settled, unsuppressed job enqueues a [`JobDelivery`]. The
//! App drains deliveries when the Agent is idle and starts a new run with the
//! formatted frame; while a run is active the frame is pushed into the steering
//! input buffer so the existing round-boundary injection machinery delivers it.
//! A job whose delivery is suppressed (foreground race acknowledgment, or an
//! in-flight `job_wait`) is never enqueued; [`AgentJobsHandle::resume`] lifts
//! the suppression and re-enqueues a result that settled while suppressed.

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde_json::json;

use super::types::{AgentEvent, AgentEventSender};

/// Result text delivered inline with a completion frame.
pub(crate) const INLINE_RESULT_LIMIT: usize = 16 * 1024;
/// How long settled job rows stay listed after settlement.
const SETTLED_RETENTION: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobKind {
    Shell,
    Download,
    TerminalWatch,
}

impl JobKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Download => "download",
            Self::TerminalWatch => "terminal-watch",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn is_settled(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// One listed background job row, for the `jobs` tool and the sidebar.
#[derive(Clone, Debug)]
pub(crate) struct JobRow {
    pub(crate) id: String,
    pub(crate) kind: JobKind,
    pub(crate) label: String,
    pub(crate) status: JobStatus,
    pub(crate) elapsed: Duration,
}

/// A settled job result waiting to be framed into the conversation.
#[derive(Clone, Debug)]
pub(crate) struct JobDelivery {
    pub(crate) id: String,
    pub(crate) kind: JobKind,
    pub(crate) label: String,
    pub(crate) status: JobStatus,
    pub(crate) duration: Duration,
    /// Inline result text (already truncated to [`INLINE_RESULT_LIMIT`] with a
    /// spill pointer when the full output was larger).
    pub(crate) result: String,
}

struct JobEntry {
    id: String,
    kind: JobKind,
    label: String,
    status: JobStatus,
    started: Instant,
    settled: Option<Instant>,
    /// Rendered result text; present once settled.
    result: Option<String>,
    /// Path holding the full output when it exceeded the inline limit.
    spill: Option<PathBuf>,
    /// Foreground waiter for the auto-background race. The waiter holds the
    /// receiving half; settling sends once and drops the sender.
    waiter: Option<tokio::sync::oneshot::Sender<Result<String, String>>>,
    /// Delivery suppressed: the foreground race acknowledged this job, or a
    /// `job_wait` is watching it.
    suppressed: bool,
    /// True once the delivery has been handed to the conversation. A settled
    /// job with `false` that is later resumed re-enqueues.
    delivered: bool,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct JobState {
    next_id: u64,
    entries: Vec<JobEntry>,
    deliveries: VecDeque<JobDelivery>,
    /// Bumped on every state mutation so the UI can cheaply poll for changes.
    revision: u64,
    seen_revision: u64,
    workspace: Option<PathBuf>,
    max_running: usize,
}

impl JobState {
    fn running_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.status == JobStatus::Running)
            .count()
    }

    fn evict_expired(&mut self) {
        let now = Instant::now();
        let before = self.entries.len();
        self.entries.retain(|entry| {
            let keep = entry.status == JobStatus::Running
                || entry
                    .settled
                    .is_none_or(|settled| now.duration_since(settled) < SETTLED_RETENTION);
            if !keep {
                if let Some(spill) = &entry.spill {
                    let _ = fs::remove_file(spill);
                }
            }
            keep
        });
        if self.entries.len() != before {
            self.revision += 1;
        }
    }
}

/// Shared handle to the background job registry, cloned into `AgentRuntime`,
/// tools, and the App UI. All methods are thread-safe; settlement may come from
/// any job thread.
#[derive(Clone)]
pub(crate) struct AgentJobsHandle {
    inner: Arc<Mutex<JobState>>,
    events: AgentEventSender,
}

/// A freshly started background job: its id, cancellation flag, and the
/// completion receiver consumed by the foreground auto-background race.
pub(crate) struct StartedJob {
    pub(crate) id: String,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) completion: tokio::sync::oneshot::Receiver<Result<String, String>>,
}

impl AgentJobsHandle {
    pub(crate) fn new(events: AgentEventSender) -> Self {
        Self {
            inner: Arc::new(Mutex::new(JobState {
                max_running: super::config::DEFAULT_MAX_BACKGROUND_JOBS,
                ..JobState::default()
            })),
            events,
        }
    }

    pub(crate) fn with_workspace(self, workspace: PathBuf) -> Self {
        if let Ok(mut state) = self.inner.lock() {
            state.workspace = Some(workspace);
        }
        self
    }

    /// Update the running-job capacity from the active Agent config.
    pub(crate) fn set_max_running(&self, max_running: usize) {
        if let Ok(mut state) = self.inner.lock() {
            state.max_running = max_running.max(1);
        }
    }

    pub(crate) fn at_capacity(&self) -> bool {
        self.inner
            .lock()
            .map(|state| state.running_count() >= state.max_running)
            .unwrap_or(true)
    }

    /// Start a job. `suppress` acknowledges delivery up front for the
    /// foreground race; a background-only start passes `false`.
    fn start(&self, kind: JobKind, label: &str, suppress: bool) -> Result<StartedJob> {
        let (sender, completion) = tokio::sync::oneshot::channel();
        let mut state = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("job state poisoned"))?;
        state.evict_expired();
        if state.running_count() >= state.max_running {
            bail!(
                "background job limit reached ({}); wait for running jobs to finish or cancel one",
                state.max_running
            );
        }
        state.next_id = state.next_id.saturating_add(1);
        let id = format!("job-{}", state.next_id);
        state.entries.push(JobEntry {
            id: id.clone(),
            kind,
            label: label.to_string(),
            status: JobStatus::Running,
            started: Instant::now(),
            settled: None,
            result: None,
            spill: None,
            waiter: Some(sender),
            suppressed: suppress,
            delivered: false,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        state.revision += 1;
        let cancel = state
            .entries
            .last()
            .expect("entry was just pushed")
            .cancel
            .clone();
        drop(state);
        let _ = self.events.send(AgentEvent::JobStarted {
            id: id.clone(),
            label: label.to_string(),
        });
        Ok(StartedJob {
            id,
            cancel,
            completion,
        })
    }

    /// Start a job that runs purely in the background; its result is delivered
    /// as a completion frame when it settles.
    pub(crate) fn start_background(&self, kind: JobKind, label: &str) -> Result<StartedJob> {
        self.start(kind, label, false)
    }

    /// Start a job for the foreground auto-background race. Delivery is
    /// suppressed while the tool foreground-waits; if the wait converts to a
    /// background job, call [`AgentJobsHandle::resume`].
    pub(crate) fn start_raced(&self, kind: JobKind, label: &str) -> Result<StartedJob> {
        self.start(kind, label, true)
    }

    /// Settle a job from its body thread: record the outcome, notify the
    /// foreground waiter if one is still listening, and enqueue the delivery
    /// frame unless suppressed.
    pub(crate) fn settle(&self, id: &str, outcome: Result<String, String>) {
        let delivery = {
            let Ok(mut state) = self.inner.lock() else {
                return;
            };
            let Some(index) = state.entries.iter().position(|entry| entry.id == id) else {
                return;
            };
            if state.entries[index].status.is_settled() {
                return;
            }
            let status = match &outcome {
                Ok(_) => JobStatus::Done,
                Err(_) => JobStatus::Failed,
            };
            let duration = state.entries[index].started.elapsed();
            let text = match outcome {
                Ok(text) => text,
                Err(error) => error,
            };
            let (inline, spill) = truncate_result(&mut state, id, &text);
            let rendered = render_result(&inline, spill.as_deref());
            let entry = &mut state.entries[index];
            entry.status = status;
            entry.settled = Some(Instant::now());
            entry.result = Some(inline.clone());
            entry.spill = spill;
            if let Some(waiter) = entry.waiter.take() {
                let forwarded = match status {
                    JobStatus::Done => Ok(rendered),
                    _ => Err(rendered),
                };
                let _ = waiter.send(forwarded);
            }
            state.revision += 1;
            let should_deliver =
                !state.entries[index].suppressed && !state.entries[index].delivered;
            if should_deliver {
                state.entries[index].delivered = true;
            }
            if should_deliver {
                let entry = &state.entries[index];
                Some(JobDelivery {
                    id: entry.id.clone(),
                    kind: entry.kind,
                    label: entry.label.clone(),
                    status,
                    duration,
                    result: inline,
                })
            } else {
                None
            }
        };
        if let Some(delivery) = delivery {
            self.enqueue(delivery);
        }
        // A settlement without a delivery was suppressed: the foreground
        // race or an active `job_wait` already owns the outcome, so it must
        // stay silent instead of emitting JobSettled.
    }

    /// Lift a suppression. A job that settled while suppressed has its
    /// delivery re-enqueued exactly once.
    pub(crate) fn resume(&self, id: &str) {
        let reenqueue = {
            let Ok(mut state) = self.inner.lock() else {
                return;
            };
            let Some(index) = state.entries.iter().position(|entry| entry.id == id) else {
                return;
            };
            if !state.entries[index].suppressed {
                return;
            }
            state.entries[index].suppressed = false;
            state.revision += 1;
            let reenqueue =
                state.entries[index].status.is_settled() && !state.entries[index].delivered;
            if reenqueue {
                state.entries[index].delivered = true;
            }
            if reenqueue {
                let entry = &state.entries[index];
                Some(JobDelivery {
                    id: entry.id.clone(),
                    kind: entry.kind,
                    label: entry.label.clone(),
                    status: entry.status,
                    duration: entry
                        .settled
                        .map(|settled| settled.duration_since(entry.started))
                        .unwrap_or_default(),
                    result: entry.result.clone().unwrap_or_default(),
                })
            } else {
                None
            }
        };
        if let Some(delivery) = reenqueue {
            self.enqueue(delivery);
        }
    }

    /// Suppress a job's delivery for `job_wait`. Returns the current row when
    /// the id is known.
    pub(crate) fn suppress(&self, id: &str) -> Option<JobRow> {
        let mut state = self.inner.lock().ok()?;
        state.evict_expired();
        let index = state.entries.iter().position(|entry| entry.id == id)?;
        state.entries[index].suppressed = true;
        state.revision += 1;
        Some(row_of(&state.entries[index]))
    }

    /// Permanently suppress a job's delivery: the foreground race consumed the
    /// completion inline, so no frame must ever be delivered.
    pub(crate) fn acknowledge(&self, id: &str) {
        if let Ok(mut state) = self.inner.lock() {
            if let Some(index) = state.entries.iter().position(|entry| entry.id == id) {
                state.entries[index].suppressed = true;
                state.entries[index].delivered = true;
                state.revision += 1;
            }
        }
    }

    /// Take the rendered result of a settled job (used by `job_wait`).
    pub(crate) fn take_result(&self, id: &str) -> Option<String> {
        let mut state = self.inner.lock().ok()?;
        let index = state.entries.iter().position(|entry| entry.id == id)?;
        state.entries[index].delivered = true;
        state.entries[index]
            .result
            .clone()
            .map(|truncated| render_result(&truncated, state.entries[index].spill.as_deref()))
    }

    /// Cancel one running job; its body observes the flag and settles as
    /// cancelled (no delivery frame).
    pub(crate) fn cancel(&self, id: &str) -> bool {
        let cancelled = {
            let Ok(mut state) = self.inner.lock() else {
                return false;
            };
            let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == id) else {
                return false;
            };
            if entry.status != JobStatus::Running {
                return false;
            }
            entry.status = JobStatus::Cancelled;
            entry.settled = Some(Instant::now());
            entry.cancel.store(true, Ordering::Relaxed);
            state.revision += 1;
            true
        };
        cancelled
    }

    /// Snapshot the listed rows, newest first.
    pub(crate) fn rows(&self) -> Vec<JobRow> {
        let Ok(mut state) = self.inner.lock() else {
            return Vec::new();
        };
        state.evict_expired();
        state.entries.iter().map(row_of).rev().collect::<Vec<_>>()
    }

    pub(crate) fn has_running(&self) -> bool {
        self.inner
            .lock()
            .map(|state| state.running_count() > 0)
            .unwrap_or(false)
    }

    /// Take every queued delivery, marking the jobs delivered.
    pub(crate) fn take_deliveries(&self) -> Vec<JobDelivery> {
        let mut deliveries = Vec::new();
        if let Ok(mut state) = self.inner.lock() {
            state.evict_expired();
            while let Some(delivery) = state.deliveries.pop_front() {
                deliveries.push(delivery);
            }
            state.revision += 1;
        }
        deliveries
    }

    pub(crate) fn has_pending_deliveries(&self) -> bool {
        self.inner
            .lock()
            .map(|state| !state.deliveries.is_empty())
            .unwrap_or(false)
    }

    /// UI change poll: true once per state mutation.
    pub(crate) fn poll_change(&self) -> bool {
        self.inner
            .lock()
            .map(|mut state| {
                if state.revision != state.seen_revision {
                    state.seen_revision = state.revision;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false)
    }

    /// Cancel every running job and drop all queued deliveries and settled
    /// rows. Used by session clear and application shutdown.
    pub(crate) fn terminate(&self) {
        if let Ok(mut state) = self.inner.lock() {
            for entry in &state.entries {
                if entry.status == JobStatus::Running {
                    entry.cancel.store(true, Ordering::Relaxed);
                }
                if let Some(spill) = &entry.spill {
                    let _ = fs::remove_file(spill);
                }
            }
            state.entries.clear();
            state.deliveries.clear();
            state.revision += 1;
        }
    }

    fn enqueue(&self, delivery: JobDelivery) {
        let id = delivery.id.clone();
        let failed = delivery.status == JobStatus::Failed;
        if let Ok(mut state) = self.inner.lock() {
            state.deliveries.push_back(delivery);
            state.revision += 1;
        }
        self.notify_settled(&id, failed);
    }

    fn notify_settled(&self, id: &str, failed: bool) {
        let _ = self.events.send(AgentEvent::JobSettled {
            id: id.to_string(),
            failed,
        });
    }
}

fn row_of(entry: &JobEntry) -> JobRow {
    JobRow {
        id: entry.id.clone(),
        kind: entry.kind,
        label: entry.label.clone(),
        status: entry.status,
        elapsed: match entry.settled {
            Some(settled) => settled.duration_since(entry.started),
            None => entry.started.elapsed(),
        },
    }
}

/// Truncate `text` to the inline limit, spilling the full text to
/// `workspace/jobs/<id>.out` when it exceeds it.
fn truncate_result(state: &mut JobState, id: &str, text: &str) -> (String, Option<PathBuf>) {
    if text.len() <= INLINE_RESULT_LIMIT {
        return (text.to_string(), None);
    }
    let spill = state
        .workspace
        .as_ref()
        .and_then(|workspace| write_spill(workspace, id, text));
    let inline = format!(
        "{}\n\n[Output truncated; showing the first {} characters.{}]",
        &text[..INLINE_RESULT_LIMIT],
        INLINE_RESULT_LIMIT,
        spill
            .as_ref()
            .map(|path| format!(" Full output: {}", path.display()))
            .unwrap_or_default()
    );
    (inline, spill)
}

fn write_spill(workspace: &std::path::Path, id: &str, text: &str) -> Option<PathBuf> {
    let dir = workspace.join("jobs");
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{id}.out"));
    fs::write(&path, text).ok()?;
    Some(path)
}

/// Render a stored (possibly truncated) result for a foreground waiter or a
/// `job_wait` consumer.
fn render_result(truncated: &str, spill: Option<&std::path::Path>) -> String {
    match spill {
        Some(path) => format!("{truncated}\nFull output: {}", path.display()),
        None => truncated.to_string(),
    }
}

/// Format settled-job deliveries as one framed block of user text. The frame
/// tells the model these are tool-delivered results, not human input.
pub(crate) fn format_job_deliveries(deliveries: &[JobDelivery]) -> String {
    deliveries
        .iter()
        .map(|delivery| {
            let heading = match delivery.status {
                JobStatus::Failed => format!(
                    "[background job failed] {} · {} · {}",
                    delivery.id,
                    delivery.label,
                    delivery.kind.as_str()
                ),
                _ => format!(
                    "[background job completed] {} · {} · {} · {}",
                    delivery.id,
                    delivery.label,
                    delivery.kind.as_str(),
                    format_duration(delivery.duration)
                ),
            };
            format!("{heading}\n{}", delivery.result)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// Serialize job rows for the `jobs` tool.
pub(crate) fn rows_value(rows: &[JobRow]) -> serde_json::Value {
    json!(rows
        .iter()
        .map(|row| json!({
            "id": row.id,
            "kind": row.kind.as_str(),
            "label": row.label,
            "status": row.status.as_str(),
            "elapsed_seconds": row.elapsed.as_secs(),
        }))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_support::event_channel;

    fn handle() -> AgentJobsHandle {
        let (events, _receiver) = event_channel();
        AgentJobsHandle::new(events)
    }

    #[test]
    fn background_settle_enqueues_a_delivery() {
        let jobs = handle();
        let started = jobs.start_background(JobKind::Shell, "echo hi").unwrap();
        assert!(jobs.has_running());
        assert!(!jobs.has_pending_deliveries());
        jobs.settle(&started.id, Ok("done".to_string()));
        assert!(!jobs.has_running());
        let deliveries = jobs.take_deliveries();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].id, started.id);
        assert_eq!(deliveries[0].result, "done");
        assert_eq!(deliveries[0].status, JobStatus::Done);
        assert!(jobs.take_deliveries().is_empty());
    }

    #[test]
    fn acknowledged_settlement_never_delivers() {
        let jobs = handle();
        let started = jobs.start_raced(JobKind::Shell, "build").unwrap();
        jobs.acknowledge(&started.id);
        jobs.settle(&started.id, Ok("inline".to_string()));
        assert!(!jobs.has_pending_deliveries());
        jobs.resume(&started.id);
        assert!(!jobs.has_pending_deliveries());
    }

    #[test]
    fn suppressed_settlement_stays_silent() {
        let (events, mut receiver) = event_channel();
        let jobs = AgentJobsHandle::new(events);
        let started = jobs.start_raced(JobKind::Shell, "build").unwrap();
        // The foreground race consumed the outcome inline and never resumes
        // the job, so settling while suppressed must neither deliver a frame
        // nor emit JobSettled.
        jobs.settle(&started.id, Ok("inline".to_string()));
        assert!(!jobs.has_pending_deliveries());
        while let Ok(event) = receiver.try_recv() {
            assert!(
                !matches!(event, AgentEvent::JobSettled { .. }),
                "suppressed settlement emitted JobSettled"
            );
        }
    }

    #[test]
    fn resume_after_settle_delivers_once() {
        let jobs = handle();
        let started = jobs.start_raced(JobKind::Shell, "build").unwrap();
        jobs.settle(&started.id, Ok("result".to_string()));
        // Settled while suppressed: no delivery yet.
        assert!(!jobs.has_pending_deliveries());
        jobs.resume(&started.id);
        let deliveries = jobs.take_deliveries();
        assert_eq!(deliveries.len(), 1);
        jobs.resume(&started.id);
        assert!(!jobs.has_pending_deliveries());
    }

    #[test]
    fn capacity_rejects_new_jobs() {
        let jobs = handle();
        jobs.set_max_running(1);
        let first = jobs.start_background(JobKind::Shell, "one").unwrap();
        assert!(jobs.start_background(JobKind::Shell, "two").is_err());
        assert!(jobs.at_capacity());
        jobs.cancel(&first.id);
        jobs.settle(&first.id, Err("cancelled".to_string()));
        assert!(!jobs.at_capacity());
    }

    #[test]
    fn terminate_cancels_and_clears() {
        let jobs = handle();
        let started = jobs.start_background(JobKind::Shell, "long").unwrap();
        jobs.terminate();
        assert!(!jobs.has_running());
        assert!(jobs.rows().is_empty());
        assert!(started.cancel.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn large_results_spill_and_inline_truncate() {
        let directory = tempfile::tempdir().unwrap();
        let (events, _receiver) = event_channel();
        let jobs = AgentJobsHandle::new(events).with_workspace(directory.path().to_path_buf());
        let started = jobs.start_background(JobKind::Shell, "big").unwrap();
        let big = "x".repeat(INLINE_RESULT_LIMIT + 100);
        jobs.settle(&started.id, Ok(big));
        let deliveries = jobs.take_deliveries();
        assert!(deliveries[0].result.contains("Output truncated"));
        assert!(deliveries[0].result.contains("jobs"));
    }

    #[test]
    fn take_result_returns_settled_text() {
        let jobs = handle();
        let started = jobs.start_background(JobKind::Shell, "job").unwrap();
        jobs.suppress(&started.id);
        jobs.settle(&started.id, Ok("payload".to_string()));
        let result = jobs.take_result(&started.id).unwrap();
        assert_eq!(result, "payload");
        assert!(!jobs.has_pending_deliveries());
    }

    #[test]
    fn delivery_frame_formatting_is_framed() {
        let jobs = handle();
        let started = jobs
            .start_background(JobKind::Shell, "cargo build")
            .unwrap();
        jobs.settle(&started.id, Ok("ok".to_string()));
        let deliveries = jobs.take_deliveries();
        let frame = format_job_deliveries(&deliveries);
        assert!(frame.contains("[background job completed] job-1 · cargo build · shell"));
        assert!(frame.contains("ok"));
    }
}
