//! Persistent collaborators with independent histories and round-boundary mailboxes.
#[cfg(test)]
mod tests;
use super::{AgentJobsHandle, JobKind, SubagentRunner, ToolOutput};
use crate::core::provider::Message;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CollaboratorStatus {
    Running,
    Idle,
    Failed,
    Stopped,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CollaboratorNote {
    pub id: Uuid,
    pub name: String,
    pub profile: String,
    pub instructions: String,
    pub status: CollaboratorStatus,
    pub messages: Vec<Message>,
    pub inbox: VecDeque<String>,
    pub result: Option<String>,
    pub generation: u64,
}

pub(crate) struct Collaborator {
    note: Mutex<CollaboratorNote>,
    cancelled: Arc<AtomicBool>,
    path: PathBuf,
    closed: Arc<Mutex<bool>>,
}

impl Collaborator {
    pub fn has_pending(&self) -> bool {
        !self.note.lock().unwrap().inbox.is_empty()
    }
    pub fn is_running(&self) -> bool {
        matches!(
            self.note.lock().unwrap().status,
            CollaboratorStatus::Running
        )
    }
    pub fn snapshot(&self) -> CollaboratorNote {
        self.note.lock().unwrap().clone()
    }
    fn save(&self, note: &CollaboratorNote) -> Result<()> {
        let closed = self.closed.lock().unwrap();
        anyhow::ensure!(!*closed, "Session has ended");
        let parent = self.path.parent().context("collaborator directory")?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            serde_json::to_writer(&mut file, note)?;
            file.flush()?;
            file.sync_all()?;
            super::super::storage::replace_file_atomically(&temporary, &self.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }

    /// Called only between complete provider/tool exchanges.
    pub fn checkpoint(&self, messages: &mut Vec<Message>) -> Result<bool> {
        let mut note = self.note.lock().unwrap();
        let received = !note.inbox.is_empty();
        messages.extend(note.inbox.drain(..).map(Message::user));
        note.messages = messages.clone();
        self.save(&note)?;
        Ok(received)
    }
}

#[derive(Clone)]
pub(crate) struct CollaboratorsHandle {
    directory: PathBuf,
    agents: Arc<Mutex<HashMap<Uuid, Arc<Collaborator>>>>,
    loaded: Arc<AtomicBool>,
    closed: Arc<Mutex<bool>>,
}

impl CollaboratorsHandle {
    pub fn new(directory: &Path) -> Self {
        Self {
            directory: directory.join("collaborators"),
            agents: Arc::default(),
            loaded: Arc::default(),
            closed: Arc::default(),
        }
    }

    pub fn load(&self) -> Result<()> {
        let mut agents = self.agents.lock().unwrap();
        if self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        if self.directory.exists() {
            for entry in std::fs::read_dir(&self.directory)? {
                let entry = entry?;
                if entry.path().extension().is_none_or(|ext| ext != "json") {
                    continue;
                }
                anyhow::ensure!(
                    entry.file_type()?.is_file(),
                    "Collaborator history must be a regular file"
                );
                let mut note: CollaboratorNote =
                    serde_json::from_reader(std::fs::File::open(entry.path())?)?;
                anyhow::ensure!(
                    entry.path().file_stem().and_then(|name| name.to_str())
                        == Some(&note.id.to_string()),
                    "Collaborator identity differs from its file"
                );
                if matches!(note.status, CollaboratorStatus::Running) {
                    note.status = CollaboratorStatus::Interrupted;
                }
                agents.insert(
                    note.id,
                    Arc::new(Collaborator {
                        note: Mutex::new(note),
                        path: entry.path(),
                        cancelled: Arc::default(),
                        closed: self.closed.clone(),
                    }),
                );
            }
        }
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    pub fn rows(&self) -> Vec<CollaboratorNote> {
        let mut rows = self
            .agents
            .lock()
            .unwrap()
            .values()
            .map(|agent| agent.snapshot())
            .collect::<Vec<_>>();
        rows.sort_by_key(|agent| agent.id);
        rows
    }
    pub fn is_running(&self) -> bool {
        self.any_running(&[])
    }

    pub fn any_running(&self, ids: &[Uuid]) -> bool {
        self.agents
            .lock()
            .unwrap()
            .iter()
            .any(|(id, agent)| (ids.is_empty() || ids.contains(id)) && agent.is_running())
    }
    pub fn get(&self, id: Uuid) -> Result<Arc<Collaborator>> {
        self.agents
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .context("Collaborator not found in this Session")
    }

    pub fn create(
        &self,
        name: String,
        profile: String,
        instructions: String,
    ) -> Result<Arc<Collaborator>> {
        anyhow::ensure!(!*self.closed.lock().unwrap(), "Session has ended");
        let id = Uuid::new_v4();
        let agent = Arc::new(Collaborator {
            path: self.directory.join(format!("{id}.json")),
            closed: self.closed.clone(),
            cancelled: Arc::default(),
            note: Mutex::new(CollaboratorNote {
                id,
                name,
                profile,
                instructions,
                status: CollaboratorStatus::Idle,
                messages: Vec::new(),
                inbox: VecDeque::new(),
                result: None,
                generation: 0,
            }),
        });
        agent.save(&agent.snapshot())?;
        self.agents.lock().unwrap().insert(id, agent.clone());
        Ok(agent)
    }

    pub fn send(
        &self,
        agent: Arc<Collaborator>,
        text: String,
        runner: Arc<SubagentRunner>,
        jobs: AgentJobsHandle,
        maximum: usize,
    ) -> Result<()> {
        let agents = self.agents.lock().unwrap();
        anyhow::ensure!(!*self.closed.lock().unwrap(), "Session has ended");
        let running = agents.values().filter(|agent| agent.is_running()).count();
        let mut note = agent.note.lock().unwrap();
        if matches!(note.status, CollaboratorStatus::Running) {
            note.inbox.push_back(text);
            return agent.save(&note);
        }
        anyhow::ensure!(
            running < maximum,
            "All collaborator slots are busy; wait for a collaborator to finish"
        );
        let job = jobs.start_background(JobKind::Tool, &format!("Agent {}", note.name))?;
        note.inbox.push_back(text);
        note.status = CollaboratorStatus::Running;
        note.generation += 1;
        note.result = None;
        agent.cancelled.store(false, Ordering::Relaxed);
        if let Err(error) = agent.save(&note) {
            jobs.cancel(&job.id);
            return Err(error);
        }
        let mut messages = note.messages.clone();
        drop(note);
        drop(agents);
        let closed = self.closed.clone();
        let worker_agent = agent.clone();
        let worker_jobs = jobs.clone();
        let job_id = job.id.clone();
        let spawned = std::thread::Builder::new()
            .name("collaborator".into())
            .spawn(move || {
                let agent = worker_agent;
                let result = (|| -> Result<String> {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async {
                        loop {
                            agent.checkpoint(&mut messages)?;
                            let result = runner.run_conversation(&mut messages, Some(&agent)).await;
                            let mut note = agent.note.lock().unwrap();
                            note.messages = messages.clone();
                            if *closed.lock().unwrap() {
                                return Err(anyhow::anyhow!("Session has ended"));
                            }
                            if result.is_ok() && !note.inbox.is_empty() {
                                drop(note);
                                continue;
                            }
                            note.status = if agent.cancelled.load(Ordering::Relaxed) {
                                CollaboratorStatus::Stopped
                            } else if result.is_ok() {
                                CollaboratorStatus::Idle
                            } else {
                                CollaboratorStatus::Failed
                            };
                            note.result = Some(
                                result
                                    .as_ref()
                                    .map(Clone::clone)
                                    .unwrap_or_else(|error| format!("{error:#}")),
                            );
                            agent.save(&note)?;
                            return result;
                        }
                    })
                })();
                if let Err(error) = &result {
                    let mut note = agent.note.lock().unwrap();
                    if matches!(note.status, CollaboratorStatus::Running) {
                        note.status = if agent.cancelled.load(Ordering::Relaxed) {
                            CollaboratorStatus::Stopped
                        } else {
                            CollaboratorStatus::Failed
                        };
                        note.result = Some(format!("{error:#}"));
                        let _ = agent.save(&note);
                    }
                }
                worker_jobs.settle(
                    &job_id,
                    result
                        .map(ToolOutput::text)
                        .map_err(|error| format!("{error:#}")),
                );
            });
        if let Err(error) = spawned {
            let mut note = agent.note.lock().unwrap();
            note.status = CollaboratorStatus::Failed;
            note.result = Some(error.to_string());
            let _ = agent.save(&note);
            jobs.cancel(&job.id);
            return Err(error.into());
        }
        Ok(())
    }

    pub fn cancellation(&self, id: Uuid) -> Result<Arc<AtomicBool>> {
        Ok(self.get(id)?.cancelled.clone())
    }
    pub fn stop(&self, id: Uuid) -> Result<()> {
        self.cancellation(id)?.store(true, Ordering::Relaxed);
        Ok(())
    }
    pub fn cancel_all(&self) {
        for agent in self.agents.lock().unwrap().values() {
            agent.cancelled.store(true, Ordering::Relaxed);
        }
    }
    pub fn terminate(&self) {
        *self.closed.lock().unwrap() = true;
        for agent in self.agents.lock().unwrap().values() {
            agent.cancelled.store(true, Ordering::Relaxed);
        }
    }
}
