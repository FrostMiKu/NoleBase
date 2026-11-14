//! Desktop adapter for the shared, persisted Agent session runtime.
use crate::core::{
    agent::{
        AgentEvent, ApprovalDecision, ApprovalRequest, AskUserRequest, AskUserResponse,
        PrivateTerminalInputDecision, PrivateTerminalInputRequest,
    },
    agent_panel::AgentPanel,
    agent_session::{AgentPanelEntry, AgentSessionStart},
    agent_sessions::{AgentSessions, SessionNoteRuntime},
};
use anyhow::{bail, Result};
use std::sync::Arc;
use tokio::sync::broadcast::{self, error::TryRecvError};

pub struct AgentClient {
    pub sessions: Arc<AgentSessions>,
    pub current: Arc<SessionNoteRuntime>,
    pub panel: AgentPanel,
    pub approval: Option<ApprovalRequest>,
    pub question: Option<AskUserRequest>,
    pub private_input: Option<PrivateTerminalInputRequest>,
    pub revision: u64,
    pub rows: Vec<AgentRow>,
    row_cache: Vec<(Arc<AgentPanelEntry>, usize)>,
    pub changed_from: usize,
    events: broadcast::Receiver<AgentEvent>,
}

impl AgentClient {
    pub fn open(sessions: Arc<AgentSessions>) -> Result<Self> {
        let summaries = sessions.list();
        let current = match summaries.first() {
            Some(summary) => sessions.get(summary.id)?,
            None => sessions.draft("新会话")?,
        };
        let (panel, _, events) = current.attach();
        let (approval, question, private_input) = current.pending_interactions();
        let mut client = Self {
            sessions,
            current,
            panel,
            approval,
            question,
            private_input,
            revision: 0,
            rows: Vec::new(),
            row_cache: Vec::new(),
            changed_from: 0,
            events,
        };
        client.rebuild_rows();
        Ok(client)
    }

    pub fn select(&mut self, id: uuid::Uuid) -> Result<()> {
        let current = self.sessions.get(id)?;
        let (panel, _, events) = current.attach();
        let (approval, question, private_input) = current.pending_interactions();
        current.mark_read()?;
        self.current = current;
        self.panel = panel;
        self.events = events;
        self.approval = approval;
        self.question = question;
        self.private_input = private_input;
        self.rebuild_rows();
        self.revision += 1;
        Ok(())
    }

    pub fn new_session(&mut self) -> Result<()> {
        let current = self.sessions.draft("新会话")?;
        let (panel, _, events) = current.attach();
        self.current = current;
        self.panel = panel;
        self.events = events;
        self.approval = None;
        self.question = None;
        self.private_input = None;
        self.rebuild_rows();
        self.revision += 1;
        Ok(())
    }

    pub fn send(&mut self, prompt: String) -> Result<()> {
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() {
            return Ok(());
        }
        if self.current.session.is_running() {
            bail!("Agent 正在处理当前请求。")
        }
        if self.panel.is_empty() {
            self.current.rename(
                &prompt
                    .lines()
                    .next()
                    .unwrap_or(&prompt)
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(32)
                    .collect::<String>(),
            )?;
        }
        self.sessions.retain(self.current.clone())?;
        match self.current.start(prompt.clone())? {
            AgentSessionStart::Started(_) => self.panel.push_prompt(prompt, false),
            AgentSessionStart::AlreadyRunning => bail!("Agent 正在处理当前请求。"),
        }
        self.rebuild_rows();
        self.revision += 1;
        Ok(())
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    self.panel.observe(&event);
                    changed = true;
                }
                Err(TryRecvError::Lagged(_)) => {
                    let (panel, _, events) = self.current.attach();
                    self.panel = panel;
                    self.events = events;
                    changed = true;
                }
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
        if changed {
            (self.approval, self.question, self.private_input) =
                self.current.pending_interactions();
            self.rebuild_rows();
            self.revision += 1;
        }
        changed
    }

    pub(super) fn rebuild_rows(&mut self) {
        let unchanged = self
            .panel
            .entries()
            .iter()
            .zip(&self.row_cache)
            .take_while(|(entry, (cached, _))| Arc::ptr_eq(entry, cached))
            .count();
        self.changed_from = self.row_cache[..unchanged]
            .iter()
            .map(|(_, count)| count)
            .sum();
        self.row_cache.truncate(unchanged);
        self.rows.truncate(self.changed_from);
        for (index, entry) in self.panel.entries().iter().enumerate().skip(unchanged) {
            let (kind, text) = match entry.as_ref() {
                AgentPanelEntry::Prompt { text, .. } => (AgentRowKind::Prompt, text),
                AgentPanelEntry::Assistant { text, .. } => (AgentRowKind::Assistant, text),
                AgentPanelEntry::Thinking { text, streaming } => (
                    AgentRowKind::Thinking {
                        streaming: *streaming,
                    },
                    text,
                ),
                AgentPanelEntry::Tool {
                    text,
                    active,
                    preview,
                } => {
                    let (status, details) = crate::core::agent::activity_parts(text);
                    self.rows.push(AgentRow {
                        id: format!("agent-{index}"),
                        text: status.into(),
                        kind: AgentRowKind::Tool {
                            active: *active,
                            details: details.join("\n").into(),
                            preview: preview.clone().map(Into::into),
                        },
                    });
                    self.row_cache.push((entry.clone(), 1));
                    continue;
                }
                AgentPanelEntry::Error(text) => (AgentRowKind::Error, text),
            };
            let blocks = if matches!(kind, AgentRowKind::Assistant) {
                super::markdown::blocks(text)
            } else {
                vec![text.clone().into()]
            };
            let count = blocks.len();
            self.rows
                .extend(blocks.into_iter().enumerate().map(|(part, text)| AgentRow {
                    id: format!("agent-{index}-{part}"),
                    text,
                    kind: kind.clone(),
                }));
            self.row_cache.push((entry.clone(), count));
        }
    }

    pub fn approve(&mut self, decision: ApprovalDecision) -> Result<()> {
        self.current.service.respond_approval(decision)?;
        self.current.clear_approval();
        self.approval = None;
        Ok(())
    }

    pub fn answer(&mut self, answer: AskUserResponse) -> Result<()> {
        self.current.service.respond_user(answer)?;
        self.current.clear_question();
        self.question = None;
        Ok(())
    }

    pub fn private_answer(&mut self, answer: PrivateTerminalInputDecision) -> Result<()> {
        self.current.service.submit_private_terminal_input(answer)?;
        self.current.clear_private_input();
        self.private_input = None;
        Ok(())
    }
}

#[derive(Clone)]
pub struct AgentRow {
    pub id: String,
    pub text: gpui_kit::SharedString,
    pub kind: AgentRowKind,
}

#[derive(Clone)]
pub enum AgentRowKind {
    Prompt,
    Assistant,
    Thinking {
        streaming: bool,
    },
    Tool {
        active: bool,
        details: gpui_kit::SharedString,
        preview: Option<gpui_kit::SharedString>,
    },
    Error,
}

#[cfg(test)]
mod tests;
