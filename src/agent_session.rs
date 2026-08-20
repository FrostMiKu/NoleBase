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
    use crate::provider::MessagePart;

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

    #[test]
    fn agent_session_persists_image_references_without_pixels() {
        let mut conversation = AgentConversation::default();
        conversation
            .messages
            .push(Message::user_parts(vec![MessagePart::Image(
                crate::provider::ImageBlock {
                    source: crate::provider::ImageSource::LocalFile {
                        path: std::path::PathBuf::from("/tmp/scan.png"),
                    },
                    label: "scan".to_string(),
                    media_type: crate::provider::ImageMediaType::Png,
                    width: 8,
                    height: 4,
                    bytes: Some(std::sync::Arc::from(vec![0xABu8; 4096])),
                },
            )]));
        let json = serde_json::to_string(&conversation).unwrap();
        // Pixels and base64 never reach the serialized session.
        assert!(!json.contains("base64"));
        assert!(!json.contains("\"bytes\""));

        let restored: AgentConversation = serde_json::from_str(&json).unwrap();
        let restored_block = match &restored.messages[0].parts[0] {
            MessagePart::Image(block) => block,
            _ => panic!("expected restored image part"),
        };
        assert_eq!(restored_block.label, "scan");
        assert_eq!(restored_block.width, 8);
        assert_eq!(restored_block.height, 4);
        assert_eq!(
            restored_block.media_type,
            crate::provider::ImageMediaType::Png
        );
        assert_eq!(
            restored_block.source,
            crate::provider::ImageSource::LocalFile {
                path: std::path::PathBuf::from("/tmp/scan.png")
            }
        );
        assert!(restored_block.bytes.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restored_session_reresolves_image_sources() {
        use std::io::{BufRead as _, Cursor, Write as _};
        use std::net::TcpListener;

        use crate::attachment::AttachmentStore;
        use crate::provider::{ImageBlock, ImageMediaType, ImageSource};
        use crate::storage::ATTACHMENTS_DIR;

        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(directory.path().join(ATTACHMENTS_DIR));
        store.ensure_layout().unwrap();
        let image = image::DynamicImage::new_rgb8(4, 2);
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let png = encoded.into_inner();
        let uri = store
            .import_bytes(&png, Some("scan.png"))
            .unwrap()
            .uri()
            .to_string();

        let local_path = directory.path().join("local.png");
        std::fs::write(&local_path, &png).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/remote.png");
        let remote_png = png.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                remote_png.len()
            )
            .unwrap();
            stream.write_all(&remote_png).unwrap();
            stream.flush().unwrap();
        });

        let mut conversation = AgentConversation::default();
        conversation.messages.push(
            crate::agent::images::parse_user_message(format!("![scan]({uri})"), &store, true)
                .await
                .unwrap(),
        );
        conversation.messages.push(Message::user_parts(vec![
            MessagePart::Image(ImageBlock {
                source: ImageSource::LocalFile {
                    path: local_path.clone(),
                },
                label: "local label".to_string(),
                media_type: ImageMediaType::Jpeg,
                width: 1,
                height: 1,
                bytes: None,
            }),
            MessagePart::Image(ImageBlock {
                source: ImageSource::Url { url: url.clone() },
                label: "remote label".to_string(),
                media_type: ImageMediaType::Webp,
                width: 1,
                height: 1,
                bytes: None,
            }),
        ]));

        let json = serde_json::to_string(&conversation).unwrap();
        let mut restored: AgentConversation = serde_json::from_str(&json).unwrap();
        assert!(restored
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .filter_map(|part| match part {
                MessagePart::Image(block) => Some(block),
                _ => None,
            })
            .all(|block| block.bytes.is_none()));

        let client = reqwest::Client::new();
        crate::agent::images::prepare_provider_messages(&mut restored.messages, &store, &client)
            .await
            .unwrap();
        server.join().unwrap();

        let blocks = restored
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .filter_map(|part| match part {
                MessagePart::Image(block) => Some(block),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(blocks.len(), 3);
        assert!(blocks.iter().all(|block| {
            block.bytes.is_some()
                && block.width == 4
                && block.height == 2
                && block.media_type == ImageMediaType::Png
        }));
        assert_eq!(blocks[1].label, "local label");
        assert_eq!(blocks[2].label, "remote label");
        assert_eq!(
            blocks[1].source,
            ImageSource::LocalFile {
                path: std::fs::canonicalize(local_path).unwrap()
            }
        );
        assert_eq!(blocks[2].source, ImageSource::Url { url });
    }
}
