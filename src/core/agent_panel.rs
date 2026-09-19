//! The agent activity transcript: the ordered [`AgentPanelEntry`] display
//! sequence plus the tool-row index maps that let streaming events address
//! the entry they belong to.
//!
//! Panel semantics are presentation-independent: the TUI renders the entries
//! directly, and the web backend maintains the same panel to persist and
//! serve the transcript. Presentations add their own entries (prompt echoes,
//! cancellation notices) through [`AgentPanel::push_prompt`] and
//! [`AgentPanel::push_error`]; everything event-driven flows through the
//! `apply_*` methods or [`AgentPanel::observe`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::agent::AgentEvent;
use crate::core::agent_session::AgentPanelEntry;

/// The display transcript of one agent session, with the bookkeeping that
/// keeps stream events pointing at the right entry.
#[derive(Debug, Default)]
pub struct AgentPanel {
    entries: Vec<Arc<AgentPanelEntry>>,
    /// `ToolPreparing` stream index → entries index, until the preparation
    /// hands off to a real tool id (or settles without one).
    preparing_tools: HashMap<usize, usize>,
    /// Executing tool id → entries index.
    active_tools: HashMap<String, usize>,
}

impl AgentPanel {
    /// Build a panel over restored entries (a persisted session's panel).
    pub fn from_entries(
        entries: impl IntoIterator<Item = impl Into<Arc<AgentPanelEntry>>>,
    ) -> Self {
        Self {
            entries: entries.into_iter().map(Into::into).collect(),
            preparing_tools: HashMap::new(),
            active_tools: HashMap::new(),
        }
    }

    pub fn entries(&self) -> &[Arc<AgentPanelEntry>] {
        &self.entries
    }

    /// Mutable entries access — in-place entry edits (renderer tests, the
    /// runtime's streaming shortcut).
    pub fn entries_mut(&mut self) -> &mut [Arc<AgentPanelEntry>] {
        &mut self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Owned clone of the entries, for persistence snapshots.
    pub fn snapshot(&self) -> Vec<AgentPanelEntry> {
        self.entries
            .iter()
            .map(|entry| entry.as_ref().clone())
            .collect()
    }

    /// The most recent non-empty assistant text (copy-to-clipboard source).
    pub fn last_assistant_output(&self) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match entry.as_ref() {
                AgentPanelEntry::Assistant { text, .. } if !text.trim().is_empty() => {
                    Some(text.as_str())
                }
                _ => None,
            })
    }

    /// Push the user prompt echo. `muted` marks a prompt queued behind a
    /// running turn; [`AgentPanel::apply_buffered_input_consumed`] unmutes
    /// queued prompts as their run consumes them.
    pub fn push_prompt(&mut self, text: impl Into<String>, muted: bool) {
        self.entries.push(Arc::new(AgentPanelEntry::Prompt {
            text: text.into(),
            muted,
        }));
    }

    /// Remove the trailing prompt echo — the rollback for a run start that
    /// was rejected after the echo was already pushed.
    pub fn pop_prompt(&mut self) -> bool {
        let trailing = matches!(
            self.entries.last().map(|entry| entry.as_ref()),
            Some(AgentPanelEntry::Prompt { .. })
        );
        if trailing {
            self.entries.pop();
        }
        trailing
    }

    /// Push a terminal error row (cancellation, worker failure).
    pub fn push_error(&mut self, text: impl Into<String>) {
        self.entries
            .push(Arc::new(AgentPanelEntry::Error(text.into())));
    }

    /// Append an arbitrary entry (presentation-added rows, test setups).
    pub fn push_entry(&mut self, entry: impl Into<Arc<AgentPanelEntry>>) {
        self.entries.push(entry.into());
    }

    /// Unmute the first `count` queued prompts.
    pub fn apply_buffered_input_consumed(&mut self, count: usize) {
        for entry in self
            .entries
            .iter_mut()
            .filter_map(|entry| match entry.as_ref() {
                AgentPanelEntry::Prompt { muted: true, .. } => Some(entry),
                _ => None,
            })
            .take(count)
        {
            if let AgentPanelEntry::Prompt { muted, .. } = Arc::make_mut(entry) {
                *muted = false;
            }
        }
    }

    pub fn apply_assistant_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        // A buffered prompt echo may sit above the still-streaming block, so
        // stream events must address their entry by its streaming flag, not
        // by the panel tail.
        if let Some(entry) = self.streaming_assistant_index() {
            if let AgentPanelEntry::Assistant { text, .. } = Arc::make_mut(&mut self.entries[entry])
            {
                text.push_str(delta);
            }
        } else {
            self.entries.push(Arc::new(AgentPanelEntry::Assistant {
                text: delta.to_string(),
                streaming: true,
                final_output: false,
            }));
        }
    }

    pub fn apply_assistant_message_finished(&mut self, text: &str, final_output: bool) {
        if text.trim().is_empty() {
            self.entries.retain(|entry| {
                !matches!(
                    entry.as_ref(),
                    AgentPanelEntry::Assistant {
                        streaming: true,
                        ..
                    }
                )
            });
            return;
        }
        if let Some(index) = self.streaming_assistant_index() {
            if let AgentPanelEntry::Assistant {
                text: entry_text,
                streaming,
                final_output: entry_final,
            } = Arc::make_mut(&mut self.entries[index])
            {
                *entry_text = text.to_string();
                *streaming = false;
                *entry_final = final_output;
            }
        } else {
            self.entries.push(Arc::new(AgentPanelEntry::Assistant {
                text: text.to_string(),
                streaming: false,
                final_output,
            }));
        }
    }

    pub fn apply_thinking_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(index) = self.streaming_thinking_index() {
            if let AgentPanelEntry::Thinking { text, .. } = Arc::make_mut(&mut self.entries[index])
            {
                text.push_str(delta);
            }
        } else {
            self.entries.push(Arc::new(AgentPanelEntry::Thinking {
                text: delta.to_string(),
                streaming: true,
            }));
        }
    }

    pub fn apply_thinking_finished(&mut self) {
        if let Some(index) = self.streaming_thinking_index() {
            if let AgentPanelEntry::Thinking { streaming, .. } =
                Arc::make_mut(&mut self.entries[index])
            {
                *streaming = false;
            }
        }
    }

    pub fn apply_tool_preparing(&mut self, index: usize, message: &str) {
        if let Some(&entry_index) = self.preparing_tools.get(&index) {
            if let Some(AgentPanelEntry::Tool { text, active, .. }) =
                self.entries.get_mut(entry_index).map(Arc::make_mut)
            {
                *text = message.to_string();
                *active = true;
            }
            return;
        }
        self.preparing_tools.insert(index, self.entries.len());
        self.entries.push(Arc::new(AgentPanelEntry::Tool {
            text: message.to_string(),
            active: true,
            preview: None,
        }));
    }

    pub fn apply_tool_preparation_finished(&mut self, index: usize, id: Option<&str>) {
        if let Some(entry_index) = self.preparing_tools.remove(&index) {
            if let Some(id) = id {
                self.active_tools.insert(id.to_string(), entry_index);
            } else if let Some(AgentPanelEntry::Tool { active, .. }) =
                self.entries.get_mut(entry_index).map(Arc::make_mut)
            {
                *active = false;
            }
        }
    }

    pub fn apply_tool_started(&mut self, id: &str, message: &str) {
        let index = match self
            .active_tools
            .get(id)
            .copied()
            .or_else(|| self.stranded_preparation_index())
        {
            Some(index) => {
                if let Some(AgentPanelEntry::Tool {
                    text,
                    active,
                    preview,
                }) = self.entries.get_mut(index).map(Arc::make_mut)
                {
                    *text = message.to_string();
                    *active = true;
                    *preview = None;
                }
                index
            }
            None => {
                self.entries.push(Arc::new(AgentPanelEntry::Tool {
                    text: message.to_string(),
                    active: true,
                    preview: None,
                }));
                self.entries.len() - 1
            }
        };
        self.active_tools.insert(id.to_string(), index);
    }

    /// Update a running tool's activity text. Returns whether the tool id
    /// was known — unknown updates are ignored.
    pub fn apply_tool_updated(&mut self, id: &str, message: &str) -> bool {
        match self.active_tools.get(id) {
            Some(&index) => {
                if let Some(AgentPanelEntry::Tool { text, active, .. }) =
                    self.entries.get_mut(index).map(Arc::make_mut)
                {
                    *text = message.to_string();
                    *active = true;
                }
                true
            }
            None => false,
        }
    }

    pub fn apply_tool_finished(&mut self, id: &str, message: &str, preview: Option<&str>) {
        let index = self
            .active_tools
            .remove(id)
            .or_else(|| self.stranded_preparation_index())
            .or_else(|| {
                self.entries.iter().rposition(|entry| {
                    matches!(entry.as_ref(), AgentPanelEntry::Tool { active: true, .. })
                })
            });
        let entry = index.and_then(|index| self.entries.get_mut(index));
        if let Some(entry) = entry {
            if let AgentPanelEntry::Tool {
                text,
                active,
                preview: entry_preview,
            } = Arc::make_mut(entry)
            {
                // An answered Ask row folds the answer into its outcome line.
                let answer = (message.starts_with("Completed Ask.")
                    && text.starts_with("Calling Ask..."))
                .then(|| text.lines().nth(2).map(str::to_string))
                .flatten();
                *text = answer
                    .map(|answer| format!("{message}\n{answer}"))
                    .unwrap_or_else(|| message.to_string());
                *active = false;
                *entry_preview = preview.map(str::to_string);
            }
        } else {
            self.entries.push(Arc::new(AgentPanelEntry::Tool {
                text: message.to_string(),
                active: false,
                preview: preview.map(str::to_string),
            }));
        }
    }

    /// A retried request re-streams its tool calls with fresh ids and reuses
    /// stream indexes, so preparation rows left by the failed attempt can
    /// never be claimed again; drop them along with the abandoned streams.
    /// Clearing the index maps is required: the retain shifts entries indexes,
    /// which would leave both maps pointing at wrong rows.
    pub fn apply_retry(&mut self) {
        self.entries.retain(|entry| {
            !matches!(
                entry.as_ref(),
                AgentPanelEntry::Assistant {
                    streaming: true,
                    ..
                } | AgentPanelEntry::Thinking {
                    streaming: true,
                    ..
                }
            ) && !Self::is_unclaimed_preparation(entry)
        });
        self.preparing_tools.clear();
        self.active_tools.clear();
    }

    /// End every still-streaming block (run failed or was cancelled).
    pub fn settle_streaming(&mut self) {
        for entry in &mut self.entries {
            if let AgentPanelEntry::Assistant { streaming, .. }
            | AgentPanelEntry::Thinking { streaming, .. } = Arc::make_mut(entry)
            {
                *streaming = false;
            }
        }
    }

    /// A finished run executes none of the tool calls it was still streaming,
    /// so unclaimed preparation rows are dropped instead of lingering above
    /// the outcome row.
    pub fn drop_unclaimed_preparations(&mut self) {
        self.entries
            .retain(|entry| !Self::is_unclaimed_preparation(entry));
    }

    /// Close every tool row and forget the index maps (run stopped).
    pub fn deactivate_tools(&mut self) {
        self.preparing_tools.clear();
        self.active_tools.clear();
        for entry in &mut self.entries {
            if let AgentPanelEntry::Tool { active, .. } = Arc::make_mut(entry) {
                *active = false;
            }
        }
    }

    /// Whether any tool row is currently executing.
    pub fn has_active_tools(&self) -> bool {
        !self.active_tools.is_empty()
    }

    /// Fold a user answer into the still-running "Calling Ask..." row.
    pub fn apply_ask_answer(&mut self, answer: &str) {
        let answer = answer.split_whitespace().collect::<Vec<_>>().join(" ");
        if answer.is_empty() {
            return;
        }
        if let Some(entry) = self.active_ask_entry() {
            if let AgentPanelEntry::Tool { text, .. } = Arc::make_mut(entry) {
                if text.lines().count() < 3 {
                    text.push('\n');
                    text.push_str(&answer);
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.preparing_tools.clear();
        self.active_tools.clear();
    }

    /// Apply the panel-relevant subset of an agent event. Headless observers
    /// (the web backend's session recorder) drive persistence from this;
    /// presentations with per-event side effects call the `apply_*` methods
    /// from their own event arms instead.
    pub fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AssistantDelta(delta) => self.apply_assistant_delta(delta),
            AgentEvent::AssistantMessageFinished { text, final_output } => {
                self.apply_assistant_message_finished(text, *final_output);
            }
            AgentEvent::ThinkingDelta(delta) => self.apply_thinking_delta(delta),
            AgentEvent::ThinkingFinished => self.apply_thinking_finished(),
            AgentEvent::BufferedInputConsumed(count) => {
                self.apply_buffered_input_consumed(*count);
            }
            AgentEvent::ToolPreparing { index, message } => {
                self.apply_tool_preparing(*index, message);
            }
            AgentEvent::ToolPreparationFinished { index, id } => {
                self.apply_tool_preparation_finished(*index, id.as_deref());
            }
            AgentEvent::ToolStarted { id, message } => self.apply_tool_started(id, message),
            AgentEvent::ToolUpdated { id, message } => {
                let _ = self.apply_tool_updated(id, message);
            }
            AgentEvent::ToolFinished {
                id,
                message,
                preview,
            } => self.apply_tool_finished(id, message, preview.as_deref()),
            AgentEvent::Retry => self.apply_retry(),
            AgentEvent::Stopped(_) => self.deactivate_tools(),
            AgentEvent::Finished(result) => {
                self.deactivate_tools();
                self.drop_unclaimed_preparations();
                if let Err(error) = result {
                    self.settle_streaming();
                    self.push_error(format!("Agent failed: {error}"));
                }
            }
            _ => {}
        }
    }

    fn streaming_assistant_index(&self) -> Option<usize> {
        self.entries.iter().rposition(|entry| {
            matches!(
                entry.as_ref(),
                AgentPanelEntry::Assistant {
                    streaming: true,
                    ..
                }
            )
        })
    }

    fn streaming_thinking_index(&self) -> Option<usize> {
        self.entries.iter().rposition(|entry| {
            matches!(
                entry.as_ref(),
                AgentPanelEntry::Thinking {
                    streaming: true,
                    ..
                }
            )
        })
    }

    /// Entries index of an unclaimed preparation row: a streamed tool call
    /// whose id was lost between the streaming preview and execution, so no
    /// `ToolStarted`/`ToolFinished` event can address it through the maps.
    fn stranded_preparation_index(&self) -> Option<usize> {
        self.entries.iter().rposition(|entry| {
            matches!(
                entry.as_ref(),
                AgentPanelEntry::Tool {
                    active: false,
                    text,
                    ..
                } if text.starts_with("Preparing")
            )
        })
    }

    /// A preparation row whose request attempt was abandoned: the retry
    /// re-issues tool ids and a finished run executes nothing, so no later
    /// event can ever claim this row.
    fn is_unclaimed_preparation(entry: &AgentPanelEntry) -> bool {
        matches!(
            entry,
            AgentPanelEntry::Tool { text, .. } if text.starts_with("Preparing")
        )
    }

    fn active_ask_entry(&mut self) -> Option<&mut Arc<AgentPanelEntry>> {
        self.entries.iter_mut().rev().find(|entry| {
            matches!(
                entry.as_ref(),
                AgentPanelEntry::Tool {
                    text,
                    active: true,
                    ..
                } if text.starts_with("Calling Ask...")
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(text: &str, streaming: bool) -> AgentPanelEntry {
        AgentPanelEntry::Assistant {
            text: text.to_string(),
            streaming,
            final_output: false,
        }
    }

    #[test]
    fn assistant_streaming_appends_in_place_and_finalizes() {
        let mut panel = AgentPanel::default();
        panel.apply_assistant_delta("Hello");
        panel.apply_assistant_delta(", world");
        assert_eq!(panel.len(), 1);
        panel.apply_assistant_message_finished("Hello, world!", true);

        assert!(matches!(
            panel.entries()[0].as_ref(),
            AgentPanelEntry::Assistant {
                text,
                streaming: false,
                final_output: true,
            } if text == "Hello, world!"
        ));
        assert_eq!(panel.last_assistant_output(), Some("Hello, world!"));
    }

    #[test]
    fn empty_finished_message_drops_streaming_placeholder() {
        let mut panel = AgentPanel::default();
        panel.apply_assistant_delta("");
        panel.apply_assistant_delta("partial");
        panel.apply_assistant_message_finished("", false);
        assert!(panel.is_empty());
    }

    #[test]
    fn prompt_echo_above_streaming_block_does_not_intercept_deltas() {
        let mut panel = AgentPanel::default();
        panel.push_prompt("first", false);
        panel.apply_assistant_delta("stream");
        panel.push_prompt("queued", true);
        panel.apply_assistant_delta(" more");

        assert!(matches!(
            panel.entries()[1].as_ref(),
            AgentPanelEntry::Assistant { text, .. } if text == "stream more"
        ));
        panel.apply_buffered_input_consumed(1);
        assert!(matches!(
            panel.entries()[2].as_ref(),
            AgentPanelEntry::Prompt {
                muted: false,
                text,
            } if text == "queued"
        ));
    }

    #[test]
    fn tool_rows_are_reused_across_the_preparation_handoff() {
        let mut panel = AgentPanel::default();
        panel.apply_tool_preparing(0, "Preparing write...");
        let preparation = Arc::as_ptr(&panel.entries()[0]);

        panel.apply_tool_preparation_finished(0, Some("t1"));
        panel.apply_tool_started("t1", "Writing notes.md");
        assert_eq!(Arc::as_ptr(&panel.entries()[0]), preparation);
        panel.apply_tool_updated("t1", "Writing notes.md (12 lines)");
        panel.apply_tool_finished("t1", "Wrote notes.md", Some("12 lines"));

        assert_eq!(panel.len(), 1);
        assert!(matches!(
            panel.entries()[0].as_ref(),
            AgentPanelEntry::Tool {
                text,
                active: false,
                preview,
            } if text == "Wrote notes.md" && preview.as_deref() == Some("12 lines")
        ));
    }

    #[test]
    fn answered_ask_folds_the_answer_into_the_outcome() {
        let mut panel = AgentPanel::default();
        panel.apply_tool_started("t1", "Calling Ask...\nContinue with the plan?");
        panel.apply_ask_answer("  Option   B  ");
        panel.apply_tool_finished("t1", "Completed Ask.", None);

        assert!(matches!(
            panel.entries()[0].as_ref(),
            AgentPanelEntry::Tool { text, .. } if text == "Completed Ask.\nOption B"
        ));
    }

    #[test]
    fn retry_drops_abandoned_streams_and_preparations() {
        let mut panel = AgentPanel::default();
        panel.apply_assistant_delta("half answer");
        panel.apply_thinking_delta("half thought");
        panel.apply_tool_preparing(3, "Preparing grep...");
        panel.apply_tool_finished("t9", "Done grep", None);

        panel.apply_retry();
        assert_eq!(panel.len(), 1);
        assert!(matches!(
            panel.entries()[0].as_ref(),
            AgentPanelEntry::Tool { text, .. } if text == "Done grep"
        ));
        // The freed stream index starts a fresh row instead of hijacking one.
        panel.apply_tool_preparing(3, "Preparing grep...");
        assert_eq!(panel.len(), 2);
    }

    #[test]
    fn observe_mirrors_the_apply_methods() {
        let mut panel = AgentPanel::default();
        panel.observe(&AgentEvent::AssistantDelta("hi".to_string()));
        panel.observe(&AgentEvent::ToolStarted {
            id: "t1".to_string(),
            message: "Reading x".to_string(),
        });
        panel.observe(&AgentEvent::Finished(Err("boom".to_string())));

        assert!(matches!(
            panel.entries()[0].as_ref(),
            AgentPanelEntry::Assistant {
                streaming: false,
                ..
            }
        ));
        assert!(matches!(
            panel.entries()[1].as_ref(),
            AgentPanelEntry::Tool { active: false, .. }
        ));
        assert!(matches!(
            panel.entries()[2].as_ref(),
            AgentPanelEntry::Error(message) if message == "Agent failed: boom"
        ));
    }

    #[test]
    fn pop_prompt_removes_only_a_trailing_prompt() {
        let mut panel = AgentPanel::from_entries(vec![assistant("answer", false)]);
        assert!(!panel.pop_prompt());
        panel.push_prompt("echo", false);
        assert!(panel.pop_prompt());
        assert_eq!(panel.len(), 1);
    }
}
