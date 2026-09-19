use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::agent::AgentEvent;
use crate::core::agent_service::AgentService;
use crate::core::provider::Message;

/// One step of the agent-maintained task plan for the current conversation.
/// Unrelated to the note vault's todo tasks: this list tracks the agent's own
/// multi-step work and renders in the Agent sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentConversation {
    pub(crate) messages: Vec<Message>,
    /// Input tokens (cache-inclusive) the provider billed for the most recent
    /// request on this conversation. Runtime observation for compaction, not
    /// conversation content: never persisted, starts at zero after a restore,
    /// and the local estimate covers the gap until the next request reports a
    /// real figure.
    #[serde(skip)]
    pub(crate) last_request_input_tokens: u64,
    /// The agent's current task plan, mirrored from the shared todo handle
    /// after every tool batch so checkpoints and restarts keep it.
    #[serde(default)]
    pub(crate) todos: Vec<TodoItem>,
}

impl AgentConversation {
    pub fn clear(&mut self) -> bool {
        let had_history = !self.messages.is_empty();
        self.messages.clear();
        self.todos.clear();
        had_history
    }

    #[cfg(test)]
    pub(crate) fn seeded_for_test() -> Self {
        Self {
            messages: vec![Message::user("previous prompt")],
            last_request_input_tokens: 0,
            todos: Vec::new(),
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
pub struct AgentSessionScope {
    messages: Vec<Message>,
    #[serde(default)]
    todos: Vec<TodoItem>,
    panel: Vec<AgentPanelEntry>,
    usage: TokenUsage,
    timed_output_tokens: u64,
    response_duration: Duration,
}

impl AgentSessionScope {
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
            todos: conversation.todos.clone(),
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
                last_request_input_tokens: 0,
                todos: self.todos,
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

/// Outcome of a session start request.
pub enum AgentSessionStart {
    /// The run was accepted; the receiver carries the run's own event
    /// stream for clients that drain events themselves (the TUI loop).
    Started(tokio::sync::broadcast::Receiver<AgentEvent>),
    /// A run is already active on this session.
    AlreadyRunning,
}

/// One agent conversation session: owns the caller-side conversation, the
/// active run handle, and the idle background-job delivery both
/// presentations previously reimplemented on their own.
///
/// A supervisor thread keeps the conversation in sync from
/// `ConversationUpdated` events, marks the session idle on
/// `Finished`/`Stopped`, and — when a background job settles while no run
/// is active — starts a continuation run with the queued results, the
/// same policy the TUI event loop used to encode. Active runs drain
/// deliveries at their own round boundaries and need no help here.
pub struct AgentSession {
    service: Arc<AgentService>,
    conversation: Mutex<AgentConversation>,
    run: Mutex<Option<CancellationToken>>,
    /// Whether the supervisor thread starts continuation runs on its own
    /// (web/headless clients). The TUI keeps delivery triggering inside its
    /// event loop for buffered-input priority and calls the capability
    /// directly instead.
    auto_deliver_jobs: bool,
}

/// A wake-up run carrying settled background-job results: the framed prompt
/// echo for presentation layers, and the run's own event stream for clients
/// that drain events directly (the TUI).
pub struct JobDeliveryRun {
    /// Framed prompt for the model (what the wake-up run receives).
    pub prompt: String,
    /// Human-readable summary for the presentation layer's transcript.
    pub echo: String,
    pub events: tokio::sync::broadcast::Receiver<AgentEvent>,
}

impl AgentSession {
    /// Build a session over the shared agent service. Pass a restored
    /// conversation to continue one. With `auto_deliver_jobs` the session's
    /// supervisor starts continuation runs for settled background jobs on
    /// its own; otherwise the presentation layer triggers delivery via
    /// [`AgentSession::deliver_jobs_if_idle`] at its own point in the event
    /// loop.
    pub fn new(
        service: Arc<AgentService>,
        conversation: AgentConversation,
        auto_deliver_jobs: bool,
    ) -> Arc<Self> {
        let session = Arc::new(Self {
            service,
            conversation: Mutex::new(conversation),
            run: Mutex::new(None),
            auto_deliver_jobs,
        });
        session.spawn_supervisor();
        session
    }

    fn spawn_supervisor(self: &Arc<Self>) {
        let supervisor = Arc::downgrade(self);
        let mut events = self.service.subscribe();
        std::thread::Builder::new()
            .name("agent-session".to_string())
            .spawn(move || loop {
                let event = match events.blocking_recv() {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                };
                let Some(supervisor) = supervisor.upgrade() else {
                    break;
                };
                match event {
                    AgentEvent::ConversationUpdated(conversation) => {
                        if let Ok(mut current) = supervisor.conversation.lock() {
                            *current = conversation;
                        }
                    }
                    // A finished or stopped run frees the session; a job
                    // that settled during the final round would otherwise
                    // strand its result until an unrelated future run.
                    AgentEvent::Finished(_) | AgentEvent::Stopped(_) => {
                        if let Ok(mut run) = supervisor.run.lock() {
                            *run = None;
                        }
                        supervisor.auto_deliver();
                    }
                    AgentEvent::JobSettled { .. } => {
                        supervisor.auto_deliver();
                    }
                    AgentEvent::JobDelivered(_) => {}
                    _ => {}
                }
            })
            .expect("spawning the agent session supervisor");
    }

    pub fn service(&self) -> &AgentService {
        &self.service
    }

    pub fn is_running(&self) -> bool {
        self.run.lock().is_ok_and(|run| run.is_some())
    }

    /// Run `f` with a borrow of the current conversation.
    pub fn with_conversation<R>(&self, f: impl FnOnce(&AgentConversation) -> R) -> R {
        let guard = self
            .conversation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&guard)
    }

    /// Snapshot of the current conversation (for persistence).
    pub fn conversation(&self) -> AgentConversation {
        self.conversation
            .lock()
            .map(|conversation| conversation.clone())
            .unwrap_or_default()
    }

    /// Replace the conversation (new chat, session restore).
    pub fn replace_conversation(&self, conversation: AgentConversation) {
        if let Ok(mut current) = self.conversation.lock() {
            *current = conversation;
        }
    }

    /// Clear the conversation history. Returns whether it had any.
    pub fn clear_conversation(&self) -> bool {
        match self.conversation.lock() {
            Ok(mut conversation) => conversation.clear(),
            Err(_) => false,
        }
    }

    /// Start a run on this session: guards against concurrent runs, stores
    /// the run's cancellation token, and hands the run's event receiver to
    /// the caller. The supervisor keeps the conversation current.
    pub fn start(&self, prompt: String) -> anyhow::Result<AgentSessionStart> {
        let mut run = self
            .run
            .lock()
            .map_err(|_| anyhow::anyhow!("agent session lock poisoned"))?;
        if run.is_some() {
            return Ok(AgentSessionStart::AlreadyRunning);
        }
        let conversation = self
            .conversation
            .lock()
            .map_err(|_| anyhow::anyhow!("agent session lock poisoned"))?
            .clone();
        let observable = self.service.run(prompt, conversation)?;
        *run = Some(observable.cancel.clone());
        Ok(AgentSessionStart::Started(observable.events))
    }

    /// Request cancellation of the active run. The session returns to idle
    /// when the worker confirms via `Stopped`/`Finished`.
    pub fn cancel(&self) {
        self.service.cancel();
        if let Ok(run) = self.run.lock() {
            if let Some(cancel) = run.as_ref() {
                cancel.cancel();
            }
        }
    }

    /// While idle, take settled background-job deliveries from the core
    /// and start a continuation run with them. Active runs drain
    /// deliveries at their own round boundaries, so this is a no-op unless
    /// the session is idle and results are queued. Returns the delivery
    /// run's prompt echo and event stream when a run started.
    /// Supervisor-side automatic delivery; a no-op unless the session was
    /// built with `auto_deliver_jobs`.
    fn auto_deliver(&self) {
        if !self.auto_deliver_jobs {
            return;
        }
        if let Err(error) = self.deliver_jobs_if_idle() {
            eprintln!("[nole] agent session job delivery error: {error:#}");
        }
    }

    pub fn deliver_jobs_if_idle(&self) -> anyhow::Result<Option<JobDeliveryRun>> {
        if self.is_running() {
            return Ok(None);
        }
        let deliveries = self.service.jobs().take_deliveries();
        if deliveries.is_empty() {
            return Ok(None);
        }
        let prompt = crate::core::agent::format_job_deliveries(&deliveries);
        let echo = match (deliveries.first(), deliveries.len()) {
            (Some(first), 1) if first.status == crate::core::agent::JobStatus::Failed => {
                format!("Job failed: {}", first.label)
            }
            (Some(first), _) => format!("Job completed: {}", first.label),
            (None, _) => return Ok(None),
        };
        match self.start(prompt.clone())? {
            AgentSessionStart::Started(events) => Ok(Some(JobDeliveryRun {
                prompt,
                echo,
                events,
            })),
            AgentSessionStart::AlreadyRunning => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent::JobKind;
    use crate::core::provider::MessagePart;
    use crate::core::storage::Storage;
    use crate::core::workspace::Workspace;
    use std::time::Instant;

    fn test_session(auto_deliver: bool) -> (tempfile::TempDir, Arc<AgentSession>) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        storage.ensure_files().unwrap();
        let workspace = Arc::new(Workspace::open(storage).unwrap());
        let service = Arc::new(AgentService::new(&workspace));
        let session = AgentSession::new(service, AgentConversation::default(), auto_deliver);
        (directory, session)
    }

    fn wait_idle(session: &AgentSession) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.is_running() {
            assert!(Instant::now() < deadline, "delivery run must settle");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_finished(
        events: &mut tokio::sync::broadcast::Receiver<AgentEvent>,
    ) -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "run must finish");
            match events.try_recv() {
                Ok(AgentEvent::Finished(result)) => {
                    return match result {
                        Ok(_) => Ok(String::new()),
                        Err(error) => Err(error),
                    };
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    panic!("agent event stream closed");
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            }
        }
    }

    #[test]
    fn idle_delivery_without_queued_deliveries_is_a_noop() {
        let (_directory, session) = test_session(false);
        assert!(session.deliver_jobs_if_idle().unwrap().is_none());
        assert!(!session.is_running());
    }

    #[test]
    fn queued_deliveries_start_a_wake_up_run() {
        let (_directory, session) = test_session(false);
        session
            .service()
            .jobs()
            .enqueue_delivery_for_test("index build", false);
        let delivery = session
            .deliver_jobs_if_idle()
            .unwrap()
            .expect("queued deliveries must start a wake-up run");
        assert_eq!(delivery.echo, "Job completed: index build");
        // The temporary workspace has no provider configuration, so the
        // wake-up run settles with an error quickly; its event stream must
        // still carry the run's Finished marker.
        let mut events = delivery.events;
        let finished = wait_for_finished(&mut events);
        assert!(finished.is_err());
        wait_idle(&session);
        // The delivery was consumed: a second call is a no-op.
        assert!(session.deliver_jobs_if_idle().unwrap().is_none());
    }

    #[test]
    fn settled_jobs_are_delivered_automatically_when_enabled() {
        let (_directory, session) = test_session(true);
        let started = session
            .service()
            .jobs()
            .start_background(JobKind::Shell, "sleep 1 && echo done")
            .unwrap();
        // Settling broadcasts JobSettled on the shared stream; the
        // supervisor reacts by starting the wake-up run even though no
        // client asked for it.
        session.service().jobs().settle(
            &started.id,
            Ok(crate::core::agent::ToolOutput::text("done".to_string())),
        );
        // The supervisor consumes the queued delivery by starting the
        // wake-up run; observe that run's Finished marker on the shared
        // stream (a provider-less run fails fast, so is_running windows
        // are too short to poll).
        let mut events = session.service().subscribe();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "supervisor must wake the agent");
            match events.try_recv() {
                Ok(AgentEvent::Finished(result)) => {
                    // The test never calls start: only the supervisor's
                    // wake-up run could have produced this.
                    assert!(result.is_err());
                    break;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    panic!("agent event stream closed");
                }
            }
        }
        wait_idle(&session);
    }

    #[test]
    fn settled_jobs_do_not_auto_start_runs_when_disabled() {
        let (_directory, session) = test_session(false);
        let started = session
            .service()
            .jobs()
            .start_background(JobKind::Shell, "sleep 1")
            .unwrap();
        session.service().jobs().settle(
            &started.id,
            Ok(crate::core::agent::ToolOutput::text("done".to_string())),
        );
        // The TUI mode triggers delivery from its own event loop; the
        // supervisor must leave the session idle here.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!session.is_running());
        assert!(!session.service().jobs().take_deliveries().is_empty());
    }

    #[test]
    fn persisted_session_drops_queued_prompts_and_transient_activity() {
        let conversation = AgentConversation::seeded_for_test();
        let session = AgentSessionScope::from_parts(
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
        let session = AgentSessionScope::from_parts(
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
                crate::core::provider::ImageBlock {
                    source: crate::core::provider::ImageSource::LocalFile {
                        path: std::path::PathBuf::from("/tmp/scan.png"),
                    },
                    label: "scan".to_string(),
                    media_type: crate::core::provider::ImageMediaType::Png,
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
            crate::core::provider::ImageMediaType::Png
        );
        assert_eq!(
            restored_block.source,
            crate::core::provider::ImageSource::LocalFile {
                path: std::path::PathBuf::from("/tmp/scan.png")
            }
        );
        assert!(restored_block.bytes.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restored_session_reresolves_image_sources() {
        use std::io::{BufRead as _, Cursor, Write as _};
        use std::net::TcpListener;

        use crate::core::attachment::AttachmentStore;
        use crate::core::provider::{ImageBlock, ImageMediaType, ImageSource};
        use crate::core::storage::ATTACHMENTS_DIR;

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
            crate::core::agent::images::parse_user_message(format!("![scan]({uri})"), &store, true)
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
        crate::core::agent::images::prepare_provider_messages(
            &mut restored.messages,
            &store,
            &client,
        )
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
