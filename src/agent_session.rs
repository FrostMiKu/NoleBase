use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::provider::Message;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentConversation {
    pub(crate) messages: Vec<Message>,
}

impl AgentConversation {
    pub fn clear(&mut self) -> bool {
        let had_history = !self.messages.is_empty();
        self.messages.clear();
        had_history
    }

    #[cfg(test)]
    pub(crate) fn seeded_for_test() -> Self {
        Self {
            messages: vec![Message::user("previous prompt")],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl TokenUsage {
    pub fn total_input(self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.cache_read_input_tokens)
    }

    pub fn cache_read_percent(self) -> Option<f64> {
        let total = self.total_input();
        (total > 0).then(|| self.cache_read_input_tokens as f64 * 100.0 / total as f64)
    }

    pub fn add(&mut self, usage: Self) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
    }

    pub fn saturating_sub(self, previous: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(previous.input_tokens),
            output_tokens: self.output_tokens.saturating_sub(previous.output_tokens),
            cache_creation_input_tokens: self
                .cache_creation_input_tokens
                .saturating_sub(previous.cache_creation_input_tokens),
            cache_read_input_tokens: self
                .cache_read_input_tokens
                .saturating_sub(previous.cache_read_input_tokens),
        }
    }

    pub fn is_empty(self) -> bool {
        self.total_input() == 0 && self.output_tokens == 0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "entry", rename_all = "snake_case")]
pub enum AgentPanelEntry {
    Prompt {
        text: String,
        muted: bool,
    },
    Assistant {
        text: String,
        streaming: bool,
        final_output: bool,
    },
    /// Interim reasoning text (thinking blocks, or pre-tool plain text) rendered
    /// as an activity tree, distinct from the final reply card.
    Thinking {
        text: String,
        streaming: bool,
    },
    Tool {
        text: String,
        active: bool,
        /// Single-line human-readable preview of a successful tool result.
        /// Shown only in the wide Agent Chat view; withheld for structured
        /// (JSON) and failed results. See `agent::activity::tool_result_preview`.
        #[serde(default)]
        preview: Option<String>,
    },
    Error(String),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentSession {
    messages: Vec<Message>,
    panel: Vec<AgentPanelEntry>,
    usage: TokenUsage,
    timed_output_tokens: u64,
    response_duration: Duration,
}

impl AgentSession {
    pub fn from_parts(
        conversation: &AgentConversation,
        panel: &[AgentPanelEntry],
        usage: TokenUsage,
        timed_output_tokens: u64,
        response_duration: Duration,
    ) -> Self {
        let panel = panel
            .iter()
            .filter_map(|entry| match entry {
                AgentPanelEntry::Prompt { muted: true, .. } => None,
                AgentPanelEntry::Prompt { text, .. } => Some(AgentPanelEntry::Prompt {
                    text: text.clone(),
                    muted: false,
                }),
                AgentPanelEntry::Assistant {
                    text, final_output, ..
                } if text.trim().is_empty() => None,
                AgentPanelEntry::Assistant {
                    text, final_output, ..
                } => Some(AgentPanelEntry::Assistant {
                    text: text.clone(),
                    streaming: false,
                    final_output: *final_output,
                }),
                AgentPanelEntry::Thinking { text, .. } => Some(AgentPanelEntry::Thinking {
                    text: text.clone(),
                    streaming: false,
                }),
                AgentPanelEntry::Tool { text, preview, .. } => Some(AgentPanelEntry::Tool {
                    text: text.clone(),
                    active: false,
                    preview: preview.clone(),
                }),
                AgentPanelEntry::Error(text) => Some(AgentPanelEntry::Error(text.clone())),
            })
            .collect();
        Self {
            messages: conversation.messages.clone(),
            panel,
            usage,
            timed_output_tokens,
            response_duration,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        AgentConversation,
        Vec<AgentPanelEntry>,
        TokenUsage,
        u64,
        Duration,
    ) {
        let panel = self
            .panel
            .into_iter()
            .filter(|entry| {
                !matches!(
                    entry,
                    AgentPanelEntry::Assistant { text, .. } if text.trim().is_empty()
                )
            })
            .collect();
        (
            AgentConversation {
                messages: self.messages,
            },
            panel,
            self.usage,
            self.timed_output_tokens,
            self.response_duration,
        )
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.panel.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_session_drops_queued_prompts_and_transient_activity() {
        let conversation = AgentConversation::seeded_for_test();
        let session = AgentSession::from_parts(
            &conversation,
            &[
                AgentPanelEntry::Prompt {
                    text: "completed".to_string(),
                    muted: false,
                },
                AgentPanelEntry::Prompt {
                    text: "queued".to_string(),
                    muted: true,
                },
                AgentPanelEntry::Assistant {
                    text: "reply".to_string(),
                    streaming: true,
                    final_output: true,
                },
                AgentPanelEntry::Thinking {
                    text: "I will read first".to_string(),
                    streaming: true,
                },
                AgentPanelEntry::Tool {
                    text: "Reading".to_string(),
                    active: true,
                    preview: None,
                },
                AgentPanelEntry::Assistant {
                    text: String::new(),
                    streaming: false,
                    final_output: false,
                },
            ],
            TokenUsage::default(),
            0,
            Duration::ZERO,
        );
        let (_, panel, _, _, _) = session.into_parts();

        assert_eq!(panel.len(), 4);
        assert!(matches!(
            &panel[1],
            AgentPanelEntry::Assistant {
                streaming: false,
                ..
            }
        ));
        assert!(matches!(
            &panel[2],
            AgentPanelEntry::Thinking {
                streaming: false,
                ..
            }
        ));
        assert!(panel.iter().all(|entry| {
            !matches!(
                entry,
                AgentPanelEntry::Assistant { text, .. } if text.trim().is_empty()
            )
        }));
        assert!(matches!(
            &panel[3],
            AgentPanelEntry::Tool { active: false, .. }
        ));
    }

    #[test]
    fn retry_count_is_not_part_of_the_persisted_session_schema() {
        let session = AgentSession::from_parts(
            &AgentConversation::default(),
            &[],
            TokenUsage::default(),
            0,
            Duration::ZERO,
        );

        let json = serde_json::to_value(session).unwrap();
        assert!(json.get("retry_count").is_none());
    }
}
