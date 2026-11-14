//! Persistent collaborators with independent histories and round-boundary mailboxes.
mod model;
#[cfg(test)]
mod tests;
mod worker;
use super::{Agent, AgentJobsHandle, JobKind};
use crate::core::provider::Message;
use anyhow::{Context, Result};
pub(crate) use model::*;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub(crate) struct Collaborator {
    note: Mutex<CollaboratorNote>,
    cancelled: Arc<AtomicBool>,
    path: PathBuf,
    closed: Arc<Mutex<bool>>,
}

impl Collaborator {
    pub fn is_running(&self) -> bool {
        self.note.lock().unwrap().status.is_active()
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
        while let Some(message) = note.inbox.pop_front() {
            note.consumed_messages = message.sequence;
            messages.push(Message::user(message.text));
        }
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
        let mut restored = HashMap::<Uuid, Arc<Collaborator>>::new();
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
                if note.status.is_active() {
                    note.status = CollaboratorStatus::Interrupted;
                }
                validate_name(&note.name)?;
                anyhow::ensure!(
                    !restored
                        .values()
                        .any(|agent| agent.snapshot().name == note.name),
                    "Duplicate collaborator name: {}",
                    note.name
                );
                restored.insert(
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
        *agents = restored;
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    pub fn rows(&self) -> Vec<CollaboratorSummary> {
        let mut rows = self
            .agents
            .lock()
            .unwrap()
            .values()
            .map(|agent| agent.note.lock().unwrap().summary())
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

    pub fn resolve(&self, identifier: &str) -> Result<Uuid> {
        let agents = self.agents.lock().unwrap();
        if let Ok(id) = Uuid::parse_str(identifier) {
            if agents.contains_key(&id) {
                return Ok(id);
            }
        }
        if let Some((id, _)) = agents
            .iter()
            .find(|(_, agent)| agent.note.lock().unwrap().name == identifier)
        {
            return Ok(*id);
        }
        anyhow::bail!("invalid input for tool agents (id): choose a full UUID or exact collaborator name from agents list; received {identifier:?}")
    }

    pub fn create(
        &self,
        name: String,
        access: CollaboratorAccess,
        instructions: String,
    ) -> Result<Arc<Collaborator>> {
        validate_name(&name)?;
        self.load()?;
        let mut agents = self.agents.lock().unwrap();
        anyhow::ensure!(!*self.closed.lock().unwrap(), "Session has ended");
        anyhow::ensure!(!agents.values().any(|agent| agent.note.lock().unwrap().name == name), "invalid input for tool agents (name): collaborator {name:?} already exists in this Session; use send to continue it");
        let id = Uuid::new_v4();
        let agent = Arc::new(Collaborator {
            path: self.directory.join(format!("{id}.json")),
            closed: self.closed.clone(),
            cancelled: Arc::default(),
            note: Mutex::new(CollaboratorNote {
                id,
                name,
                access,
                instructions,
                status: CollaboratorStatus::Idle,
                messages: Vec::new(),
                inbox: VecDeque::new(),
                received_messages: 0,
                consumed_messages: 0,
                reports: Vec::new(),
                error: None,
                generation: 0,
            }),
        });
        agent.save(&agent.snapshot())?;
        agents.insert(id, agent.clone());
        Ok(agent)
    }

    pub fn discard_unstarted(&self, id: Uuid) -> Result<()> {
        let mut agents = self.agents.lock().unwrap();
        if let Some(agent) = agents.get(&id) {
            anyhow::ensure!(!agent.is_running(), "Collaborator is active");
            std::fs::remove_file(&agent.path)?;
        }
        agents.remove(&id);
        Ok(())
    }

    pub fn send(
        &self,
        agent: Arc<Collaborator>,
        text: String,
        runner: Arc<Agent>,
        jobs: AgentJobsHandle,
        maximum: usize,
    ) -> Result<MessageReceipt> {
        anyhow::ensure!(
            !text.trim().is_empty(),
            "invalid input for tool agents (message): provide a non-empty message"
        );
        let agents = self.agents.lock().unwrap();
        anyhow::ensure!(!*self.closed.lock().unwrap(), "Session has ended");
        let running = agents.values().filter(|agent| agent.is_running()).count();
        let mut note = agent.note.lock().unwrap();
        anyhow::ensure!(
            note.status != CollaboratorStatus::Stopping,
            "Collaborator is stopping; wait for stopped before sending another message"
        );
        let active = note.status == CollaboratorStatus::Running;
        anyhow::ensure!(
            active || running < maximum,
            "All collaborator slots are busy; wait for a collaborator to finish"
        );
        let job = if active {
            None
        } else {
            Some(jobs.start_background(
                JobKind::Collaborator,
                &format!("Agent {} · report {}", note.name, note.reports.len() + 1),
            )?)
        };
        let previous = note.clone();
        note.received_messages += 1;
        let sequence = note.received_messages;
        note.inbox.push_back(CollaboratorMessage { sequence, text });
        if !active {
            note.status = CollaboratorStatus::Running;
            note.generation += 1;
            note.error = None;
            agent.cancelled.store(false, Ordering::Relaxed);
        }
        if let Err(error) = agent.save(&note) {
            *note = previous;
            if let Some(job) = job {
                jobs.cancel(&job.id);
            }
            return Err(error);
        }
        let receipt = MessageReceipt {
            id: note.id,
            sequence,
            generation: note.generation,
            pending_messages_at_acceptance: note.inbox.len(),
        };
        drop(note);
        drop(agents);
        if let Some(job) = job {
            worker::spawn(agent, runner, jobs, job.id)?;
        }
        Ok(receipt)
    }

    pub fn cancellation(&self, id: Uuid) -> Result<Arc<AtomicBool>> {
        Ok(self.get(id)?.cancelled.clone())
    }
    pub fn stop(&self, id: Uuid) -> Result<StopOutcome> {
        let agent = self.get(id)?;
        let mut note = agent.note.lock().unwrap();
        let outcome = match note.status {
            CollaboratorStatus::Running => {
                note.status = CollaboratorStatus::Stopping;
                if let Err(error) = agent.save(&note) {
                    note.status = CollaboratorStatus::Running;
                    return Err(error);
                }
                agent.cancelled.store(true, Ordering::Relaxed);
                StopOutcome::Stopping
            }
            CollaboratorStatus::Stopping => StopOutcome::AlreadyStopping,
            CollaboratorStatus::Stopped => StopOutcome::AlreadyStopped,
            _ => StopOutcome::AlreadyIdle,
        };
        Ok(outcome)
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
