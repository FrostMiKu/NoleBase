//! The workspace filesystem watcher: observes application-managed content,
//! refreshes the document index, and publishes domain-level workspace
//! events. It never touches terminal events, layout, or UI state.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::core::storage::Storage;
use crate::core::workspace::Workspace;

type WatchEvents = mpsc::Receiver<notify::Result<notify::Event>>;

/// A domain-level change observed in the workspace filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceEvent {
    /// Managed workspace content changed (a note file, the settings file, or
    /// a theme file): consumers should refresh workspace data.
    /// `markdown_paths` are the managed note files to re-index; the watcher
    /// has already fed them to the document index.
    ContentChanged { markdown_paths: Vec<PathBuf> },
    /// Attachment store files changed (content or metadata under
    /// `attachments/`); consumers refresh attachment-derived state.
    AttachmentsChanged { paths: Vec<PathBuf> },
    /// Files changed outside the agent sandbox; consumers forward these
    /// paths so cached agent reads are invalidated.
    AgentReadsDirty { paths: Vec<PathBuf> },
    /// The filesystem watcher reported an error.
    WatcherFailed(String),
}

/// The paths the classification rules need, captured from storage so raw
/// events can be classified without holding a storage reference.
#[derive(Clone, Debug)]
pub(crate) struct WatchContext {
    workspace_dir: PathBuf,
    attachments_dir: PathBuf,
    settings_path: PathBuf,
    themes_dir: PathBuf,
}

impl WatchContext {
    pub(crate) fn from_storage(storage: &Storage) -> Self {
        Self {
            workspace_dir: storage.workspace_dir.clone(),
            attachments_dir: storage.attachments_dir.clone(),
            settings_path: storage.settings_path.clone(),
            themes_dir: storage.themes_dir.clone(),
        }
    }
}

/// Watches application-managed content only.
///
/// The watcher covers only locations whose changes require application
/// refresh. The agent workspace (`workspace/`) churns constantly while a
/// task runs, so its events stay outside the notify queue. Registered
/// locations are:
///
/// - `daily/`, `data/`, `archives/` — managed notes (workspace reload and
///   the note/attachment reference indexes);
/// - `themes/` — theme TOMLs (theme reload);
/// - `config/` — the specific config files that refresh reactively
///   (settings.toml for the theme selection, and AGENTS.md for agent read
///   caches). The directory is registered rather than the settings file
///   alone so atomic-rename saves by external editors are still observed;
/// - `attachments/` — per-attachment directories (`<uuid>/content.<ext>` and
///   `<uuid>/metadata.json`); events refresh mutable externally edited
///   attachment metadata in the browser.
///
/// Optional directories contribute watches when present; an absent directory
/// contributes no watch, matching the previous root-recursive behavior for
/// directories that had not appeared yet.
pub struct WorkspaceWatcher {
    /// Held for its drop guard (unregistering the filesystem watch);
    /// never read directly.
    #[allow(dead_code)]
    watcher: Option<RecommendedWatcher>,
    receiver: WatchEvents,
    context: WatchContext,
}

impl WorkspaceWatcher {
    pub fn new(storage: &Storage) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .context("creating Nole directory watcher")?;
        for directory in [
            &storage.daily_dir,
            &storage.data_dir,
            &storage.archives_dir,
            &storage.themes_dir,
            &storage.attachments_dir,
        ] {
            if fs::metadata(directory).is_ok_and(|metadata| metadata.is_dir()) {
                watcher
                    .watch(directory, RecursiveMode::Recursive)
                    .with_context(|| format!("watching {}", directory.display()))?;
            }
        }
        if fs::metadata(&storage.config_dir).is_ok_and(|metadata| metadata.is_dir()) {
            watcher
                .watch(&storage.config_dir, RecursiveMode::NonRecursive)
                .with_context(|| format!("watching {}", storage.config_dir.display()))?;
        }
        Ok(Self {
            watcher: Some(watcher),
            receiver,
            context: WatchContext::from_storage(storage),
        })
    }

    /// A watcher over an injected event stream, without a real filesystem
    /// watcher behind it. Test-only.
    #[cfg(test)]
    pub(crate) fn from_parts(context: WatchContext, receiver: WatchEvents) -> Self {
        Self {
            watcher: None,
            receiver,
            context,
        }
    }

    /// Drain pending filesystem events, feed the changed managed-note paths
    /// into the workspace's document index, and return the resulting domain
    /// events for consumers to respond to.
    pub fn poll(&mut self, workspace: &Workspace) -> Vec<WorkspaceEvent> {
        let raw: Vec<_> = self.receiver.try_iter().collect();
        let (events, markdown_paths) = classify_events(&raw, &self.context);
        if !markdown_paths.is_empty() {
            workspace.refresh_documents(markdown_paths);
        }
        events
    }
}

/// Classify drained notify events into domain events plus the managed
/// markdown paths that need re-indexing.
///
/// Ordering note: the returned events list agent-read invalidation first,
/// then attachment changes, then content changes — the order consumers
/// applied when this logic lived in the terminal runtime.
pub(crate) fn classify_events(
    raw: &[notify::Result<notify::Event>],
    context: &WatchContext,
) -> (Vec<WorkspaceEvent>, Vec<PathBuf>) {
    // The agent workspace sandbox (workspace/) churns constantly while an
    // Agent task runs. It stays outside the watcher, and the check below
    // adds defense in depth. Workspace events leave workspace UI and
    // note/attachment indexes unchanged; session file handling is the
    // workspace policy's job.
    let mut agent_read_paths = Vec::new();
    let mut attachment_paths = Vec::new();
    let mut markdown_paths = Vec::new();
    let mut content_changed = false;
    let mut watcher_error = None;
    for event in raw {
        if let Ok(event) = &event {
            if !event.paths.is_empty()
                && event
                    .paths
                    .iter()
                    .all(|path| path.starts_with(&context.workspace_dir))
            {
                continue;
            }
            agent_read_paths.extend(event.paths.iter().cloned());
            if event
                .paths
                .iter()
                .any(|path| path.starts_with(&context.attachments_dir))
            {
                attachment_paths.extend(event.paths.iter().cloned());
            }
        }
        match event {
            Ok(event)
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) && event.paths.iter().any(|path| {
                    path == &context.settings_path
                        || (path.parent() == Some(context.themes_dir.as_path())
                            && path
                                .extension()
                                .and_then(|extension| extension.to_str())
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("toml")))
                        || is_managed_markdown(path)
                }) =>
            {
                markdown_paths.extend(
                    event
                        .paths
                        .iter()
                        .filter(|path| is_managed_markdown(path))
                        .cloned(),
                );
                content_changed = true;
            }
            Ok(_) => {}
            Err(error) => watcher_error = Some(error),
        }
    }
    let mut events = Vec::new();
    if !agent_read_paths.is_empty() {
        events.push(WorkspaceEvent::AgentReadsDirty {
            paths: agent_read_paths,
        });
    }
    if !attachment_paths.is_empty() {
        events.push(WorkspaceEvent::AttachmentsChanged {
            paths: attachment_paths,
        });
    }
    if content_changed {
        events.push(WorkspaceEvent::ContentChanged {
            markdown_paths: markdown_paths.clone(),
        });
    }
    if let Some(error) = watcher_error {
        events.push(WorkspaceEvent::WatcherFailed(error.to_string()));
    }
    (events, markdown_paths)
}

fn is_managed_markdown(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("mb")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::ModifyKind;

    fn context() -> (WatchContext, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(directory.path()).unwrap();
        (WatchContext::from_storage(&storage), directory)
    }

    fn modify_event(paths: Vec<PathBuf>) -> notify::Result<notify::Event> {
        let mut event = notify::Event::new(EventKind::Modify(ModifyKind::Any));
        event.paths = paths;
        Ok(event)
    }

    fn classify(
        events: Vec<notify::Result<notify::Event>>,
        context: &WatchContext,
    ) -> (Vec<WorkspaceEvent>, Vec<PathBuf>) {
        classify_events(&events, context)
    }

    #[test]
    fn markdown_events_become_content_changes_with_index_paths() {
        let (context, directory) = context();
        let note = directory.path().join("data").join("note.md");
        let (events, markdown_paths) = classify(vec![modify_event(vec![note.clone()])], &context);
        assert_eq!(
            events,
            vec![
                WorkspaceEvent::AgentReadsDirty {
                    paths: vec![note.clone()]
                },
                WorkspaceEvent::ContentChanged {
                    markdown_paths: vec![note.clone()]
                },
            ]
        );
        assert_eq!(markdown_paths, vec![note]);
    }

    #[test]
    fn settings_and_theme_events_change_content_without_index_paths() {
        let (context, directory) = context();
        let settings = directory.path().join("config").join("settings.toml");
        let theme = directory.path().join("themes").join("custom.toml");
        let (events, markdown_paths) = classify(
            vec![
                modify_event(vec![settings.clone()]),
                modify_event(vec![theme.clone()]),
            ],
            &context,
        );
        assert_eq!(
            events,
            vec![
                WorkspaceEvent::AgentReadsDirty {
                    paths: vec![settings.clone(), theme.clone()]
                },
                WorkspaceEvent::ContentChanged {
                    markdown_paths: vec![]
                },
            ]
        );
        assert!(markdown_paths.is_empty());
    }

    #[test]
    fn non_markdown_events_only_dirty_agent_reads() {
        let (context, directory) = context();
        let temp = directory.path().join("editor.tmp");
        let (events, markdown_paths) = classify(vec![modify_event(vec![temp.clone()])], &context);
        assert_eq!(
            events,
            vec![WorkspaceEvent::AgentReadsDirty {
                paths: vec![temp.clone()]
            }]
        );
        assert!(markdown_paths.is_empty());
    }

    #[test]
    fn attachment_events_route_to_attachments_and_agent_reads() {
        let (context, directory) = context();
        let metadata = directory
            .path()
            .join("attachments")
            .join("00000000-0000-4000-8000-000000000001")
            .join("metadata.json");
        let (events, _) = classify(vec![modify_event(vec![metadata.clone()])], &context);
        assert_eq!(
            events,
            vec![
                WorkspaceEvent::AgentReadsDirty {
                    paths: vec![metadata.clone()]
                },
                WorkspaceEvent::AttachmentsChanged {
                    paths: vec![metadata.clone()]
                },
            ]
        );
    }

    #[test]
    fn agent_workspace_events_are_ignored_entirely() {
        let (context, directory) = context();
        let session = directory
            .path()
            .join("workspace")
            .join("main")
            .join("session.json");
        let scratch = directory
            .path()
            .join("workspace")
            .join("main")
            .join("scratch.md");
        let (events, markdown_paths) = classify(
            vec![modify_event(vec![session]), modify_event(vec![scratch])],
            &context,
        );
        assert!(events.is_empty());
        assert!(markdown_paths.is_empty());
    }

    #[test]
    fn mixed_events_mark_every_path_dirty_and_index_markdown() {
        // An event straddling the sandbox and a managed note is not a pure
        // sandbox event: every path dirties agent reads, and the markdown
        // filter is extension-based, so both .md paths feed the index (the
        // indexer itself drops the non-managed sandbox path).
        let (context, directory) = context();
        let scratch = directory
            .path()
            .join("workspace")
            .join("main")
            .join("scratch.md");
        let note = directory.path().join("data").join("note.mb");
        let (events, markdown_paths) = classify(
            vec![modify_event(vec![scratch.clone(), note.clone()])],
            &context,
        );
        assert_eq!(
            events,
            vec![
                WorkspaceEvent::AgentReadsDirty {
                    paths: vec![scratch.clone(), note.clone()]
                },
                WorkspaceEvent::ContentChanged {
                    markdown_paths: vec![scratch.clone(), note.clone()]
                },
            ]
        );
        assert_eq!(markdown_paths, vec![scratch, note]);
    }

    #[test]
    fn watcher_errors_surface_as_events() {
        let (context, _directory) = context();
        let error = notify::Error::new(notify::ErrorKind::WatchNotFound);
        let (events, _) = classify(vec![Err(error)], &context);
        assert!(matches!(
            events.as_slice(),
            [WorkspaceEvent::WatcherFailed(_)]
        ));
    }

    #[test]
    fn watch_workspace_registers_managed_content_only() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::new(fs::canonicalize(directory.path()).unwrap()).unwrap();
        storage.ensure_files().unwrap();

        // One stored attachment in the per-attachment directory layout.
        let attachment_dir = storage
            .attachments_dir
            .join("00000000-0000-4000-8000-000000000001");
        fs::create_dir_all(&attachment_dir).unwrap();
        let metadata_path = attachment_dir.join("metadata.json");
        fs::write(&metadata_path, "{}").unwrap();

        let watcher = WorkspaceWatcher::new(&storage).unwrap();

        // Agent workspace churn stays outside the notify queue because the
        // workspace is unregistered; a settle window therefore remains empty.
        let workspace = storage.workspace_dir.clone();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("session.json"), "{}").unwrap();
        fs::write(workspace.join("scratch.md"), "# scratch").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
        let mut workspace_events = Vec::new();
        while std::time::Instant::now() < deadline {
            for event in watcher.receiver.try_iter().flatten() {
                workspace_events.extend(
                    event
                        .paths
                        .into_iter()
                        .filter(|path| path.starts_with(&workspace)),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            workspace_events.is_empty(),
            "workspace activity must not enter notify queues: {workspace_events:?}"
        );

        // Application-managed content that requires refresh is still watched:
        // a managed note, a theme file, the settings config file, and mutable
        // attachment metadata.
        let daily = storage.daily_dir.join("2026-08-03.md");
        fs::write(&daily, "note").unwrap();
        let theme = storage.themes_dir.join("custom.toml");
        fs::write(&theme, "panel = \"#010203\"\n").unwrap();
        fs::write(&storage.settings_path, "theme = \"custom\"\n").unwrap();
        fs::write(&metadata_path, "{\"display_name\":\"renamed\"}").unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            for event in watcher.receiver.try_iter().flatten() {
                seen.extend(event.paths.iter().cloned());
            }
            let observed = [&daily, &theme, &storage.settings_path, &metadata_path]
                .iter()
                .all(|path| seen.iter().any(|seen| seen == *path));
            if observed {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for expected in [&daily, &theme, &storage.settings_path, &metadata_path] {
            assert!(
                seen.iter().any(|seen| seen == expected),
                "managed content must be observed by the watcher: {} (seen: {seen:?})",
                expected.display()
            );
        }
        assert!(
            seen.iter()
                .all(|path| !path.starts_with(&storage.workspace_dir)),
            "no watcher event may come from the agent workspace: {seen:?}"
        );
    }
}
