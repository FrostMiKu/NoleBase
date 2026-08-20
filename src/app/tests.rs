use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};

use crate::attachment::AttachmentStore;

use super::*;
use std::fs;

fn install_agent_observable(app: &mut App) -> tokio::sync::broadcast::Sender<AgentEvent> {
    let (sender, events) = tokio::sync::broadcast::channel(AGENT_STREAM_BUFFER);
    app.active_agent = Some(Observable {
        output: Box::pin(std::future::pending()),
        events,
        cancel: tokio_util::sync::CancellationToken::new(),
    });
    sender
}

fn install_agent_terminal_snapshot(app: &mut App) {
    app.agent_terminal
        .set_monitor_snapshot_for_test(crate::agent::AgentTerminalSnapshot {
            title: "ssh build-host".to_string(),
            status: crate::agent::AgentTerminalStatus::Running,
            terminal: crate::embedded_terminal::TerminalSnapshot::from_bytes(24, 80, b"ready"),
        });
}

fn make_app() -> (App, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::new(directory.path()).unwrap();
    storage.ensure_files().unwrap();
    (App::new(storage).unwrap(), directory)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The default export destination under the user's home directory, spelled
/// with the platform's path separator (`~/name` on Unix, `~\name` on
/// Windows).
fn home_dest(file_name: &str) -> String {
    Path::new("~")
        .join(file_name)
        .to_string_lossy()
        .into_owned()
}

fn add_daily_note(app: &mut App, body: &str) {
    app.storage.append_to_today(body).unwrap();
    app.reload();
    app.selected = app.daily_notes.len() - 1;
    app.focus = Focus::Center;
}

fn refresh_test_index(app: &mut App) {
    app.apply_workspace_index(WorkspaceIndex::build(&app.storage));
}

fn wait_for_export(app: &mut App) {
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while app.export_in_progress && std::time::Instant::now() < deadline {
        app.poll_export();
        std::thread::sleep(Duration::from_millis(5));
    }
    app.poll_export();
    assert!(!app.export_in_progress, "export worker did not finish");
}

#[test]
fn starts_with_daily_center_focused() {
    let (app, _directory) = make_app();
    assert_eq!(app.focus, Focus::Center);
    assert_eq!(app.center_view, CenterView::Daily);
    assert_eq!(app.files_context, FilesContext::Browse);
    assert_eq!(app.overlay, None);
    assert_eq!(app.permission_mode, PermissionMode::Approve);
}

#[test]
fn command_dialog_supports_single_multi_and_free_text_modes() {
    let (mut app, _directory) = make_app();
    app.open_dialog(DialogState::new(
        "Format",
        "Choose a format",
        DialogMode::SingleSelect,
        DialogPurpose::Custom,
        vec![DialogOption::new("Markdown"), DialogOption::new("MBDown")],
    ));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.take_dialog_result(),
        Some(DialogResult::Selected("MBDown".to_string()))
    );

    app.open_dialog(DialogState::new(
        "Targets",
        "Select targets",
        DialogMode::MultiSelect,
        DialogPurpose::Custom,
        vec![DialogOption::new("daily"), DialogOption::new("archives")],
    ));
    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.take_dialog_result(),
        Some(DialogResult::SelectedMany(vec![
            "daily".to_string(),
            "archives".to_string()
        ]))
    );

    app.open_dialog(DialogState::new(
        "Name",
        "Choose or type a name",
        DialogMode::SelectOrInput,
        DialogPurpose::Custom,
        vec![DialogOption::new("Existing")],
    ));
    app.handle_key(key(KeyCode::Char('n')));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.take_dialog_result(),
        Some(DialogResult::Text("n".to_string()))
    );
}

#[test]
fn command_palette_names_mouse_support_action_for_the_next_state() {
    let (mut app, _directory) = make_app();
    let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);

    app.handle_key(ctrl_p);
    app.handle_paste("mouse support");
    let disable = app
        .dialog
        .as_ref()
        .and_then(DialogState::selected_option)
        .unwrap();
    assert_eq!(disable.label, "Interface: Disable mouse support");
    assert!(disable.hint.as_deref().unwrap().contains("select and copy"));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Command::SetMouseCapture(false))
    );
    assert!(!app.mouse_captured);

    app.handle_key(ctrl_p);
    app.handle_paste("mouse support");
    let enable = app
        .dialog
        .as_ref()
        .and_then(DialogState::selected_option)
        .unwrap();
    assert_eq!(enable.label, "Interface: Enable mouse support");
    assert_eq!(
        enable.hint.as_deref(),
        Some("Restore mouse clicking and scrolling")
    );
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Some(Command::SetMouseCapture(true))
    );
    assert!(app.mouse_captured);
}

#[test]
fn command_palette_filters_and_clears_the_agent_session() {
    let (mut app, _directory) = make_app();
    app.agent_conversation = AgentConversation::seeded_for_test();
    app.agent_panel.push(Arc::new(AgentPanelEntry::Prompt {
        text: "Previous prompt".to_string(),
        muted: false,
    }));
    app.persist_agent_session().unwrap();
    assert!(app.storage.agent_session_path.exists());

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert_eq!(app.overlay, Some(Overlay::Dialog));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::CommandPalette)
    );
    assert_eq!(
        app.command_matches.len(),
        APP_COMMANDS
            .iter()
            .filter(|command| app.command_available(command.id))
            .count()
    );

    app.handle_paste("clear");
    assert_eq!(
        app.command_matches.first(),
        Some(&AppCommand::ClearAgentSession)
    );
    assert_eq!(
        app.dialog
            .as_ref()
            .and_then(DialogState::selected_option)
            .map(|option| option.label.as_str()),
        Some("Agent: Clear session")
    );
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.overlay, None);
    assert!(!app.agent_conversation.clear());
    assert!(app.agent_panel.is_empty());
    assert!(!app.storage.agent_session_path.exists());
    assert_eq!(app.status, "Agent session cleared");
}

#[test]
fn app_restores_the_single_agent_session() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::new(directory.path()).unwrap();
    storage.ensure_files().unwrap();
    let conversation = AgentConversation::seeded_for_test();
    let panel = vec![Arc::new(AgentPanelEntry::Assistant {
        text: "Persisted answer".to_string(),
        streaming: false,
        final_output: true,
    })];
    storage
        .write_agent_session(&AgentSession::from_parts(
            &conversation,
            &panel
                .iter()
                .map(|entry| entry.as_ref().clone())
                .collect::<Vec<_>>(),
            TokenUsage {
                input_tokens: 10,
                output_tokens: 4,
                cache_creation_input_tokens: 2,
                cache_read_input_tokens: 3,
            },
            4,
            Duration::from_secs(2),
        ))
        .unwrap();

    let mut app = App::new(storage).unwrap();

    assert!(app.agent_conversation.clear());
    assert_eq!(app.agent_panel, panel);
    assert_eq!(app.agent_scroll, 0);
    assert!(app.agent_follow_tail);
    assert_eq!(app.agent_usage.input_tokens, 10);
    assert_eq!(app.agent_timed_output_tokens, 4);
    assert_eq!(app.agent_response_duration, Duration::from_secs(2));
}

#[test]
fn conversation_update_overwrites_the_saved_agent_session() {
    let (mut app, _directory) = make_app();
    app.agent_panel.push(Arc::new(AgentPanelEntry::Assistant {
        text: "Completed answer".to_string(),
        streaming: false,
        final_output: true,
    }));
    let sender = install_agent_observable(&mut app);

    sender
        .send(AgentEvent::ConversationUpdated(
            AgentConversation::seeded_for_test(),
        ))
        .unwrap();
    app.poll_agent();

    let (mut conversation, panel, _, _, _) = app
        .storage
        .load_agent_session()
        .unwrap()
        .unwrap()
        .into_parts();
    assert!(conversation.clear());
    assert_eq!(
        panel,
        app.agent_panel
            .iter()
            .map(|entry| entry.as_ref().clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn failed_session_delete_keeps_the_in_memory_session() {
    let (mut app, _directory) = make_app();
    app.agent_conversation = AgentConversation::seeded_for_test();
    install_agent_terminal_snapshot(&mut app);
    fs::create_dir(&app.storage.agent_session_path).unwrap();

    app.clear_agent_session();

    assert!(app.agent_conversation.clear());
    assert!(!app.agent_terminal.is_active());
    assert!(app.status.starts_with("Agent session clear error:"));
    assert!(app.notifications.visible().is_some());
}

#[test]
fn command_palette_interrupts_the_running_agent() {
    let (mut app, _directory) = make_app();
    let cancelled = Arc::new(AtomicBool::new(false));
    app.ai_cancel = Some(cancelled.clone());
    app.ai_running = true;

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    app.handle_paste("interrupt");
    assert_eq!(
        app.command_matches.first(),
        Some(&AppCommand::InterruptAgent)
    );
    app.handle_key(key(KeyCode::Enter));

    assert!(cancelled.load(Ordering::Relaxed));
    assert!(!app.ai_running);
    assert_eq!(app.status, "Agent task cancelled");
}

#[test]
fn command_palette_creates_and_opens_a_regular_note() {
    let (mut app, _directory) = make_app();
    fs::write(&app.storage.template_path, "# From template\n").unwrap();

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    app.handle_paste("note new");
    assert_eq!(app.command_matches.first(), Some(&AppCommand::NewNote));
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.files_context, FilesContext::NewTarget);
    assert_eq!(app.overlay, Some(Overlay::Dialog));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::NewFile)
    );

    app.handle_paste("Scratch");
    app.handle_key(key(KeyCode::Enter));

    let path = app.storage.data_dir.join("Scratch.md");
    assert!(path.exists());
    assert_eq!(fs::read_to_string(&path).unwrap(), "# Scratch\n\n");
    assert_eq!(app.files_context, FilesContext::Browse);
    assert_eq!(app.focus, Focus::Center);
    assert!(matches!(
        app.document.as_ref().map(|document| &document.kind),
        Some(DocumentKind::File(opened)) if opened == &path
    ));
    assert_eq!(
        app.document
            .as_ref()
            .map(|document| document.source.as_str()),
        Some("# Scratch\n\n")
    );
}

#[test]
fn command_palette_creates_a_note_from_the_template_only_when_requested() {
    let (mut app, _directory) = make_app();
    fs::write(&app.storage.template_path, "# From template\n").unwrap();

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    app.handle_paste("new from template");
    assert_eq!(
        app.command_matches.first(),
        Some(&AppCommand::NewNoteFromTemplate)
    );
    app.handle_key(key(KeyCode::Enter));
    app.handle_paste("Templated");
    app.handle_key(key(KeyCode::Enter));

    let path = app.storage.data_dir.join("Templated.md");
    assert_eq!(fs::read_to_string(&path).unwrap(), "# From template\n");
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(path))
    );
}

#[test]
fn command_palette_adds_contextual_note_and_agent_output_commands() {
    let (mut app, _directory) = make_app();
    let current = app.storage.data_dir.join("Current.md");
    let other = app.storage.data_dir.join("Other.md");
    fs::write(&current, "# Current\n").unwrap();
    fs::write(&other, "# Other\n").unwrap();
    app.reload_files();
    app.open_file_document(&current, DocumentReturn::Daily);
    app.selected_file = Some(other);
    app.agent_panel.push(Arc::new(AgentPanelEntry::Assistant {
        text: "Final response".to_string(),
        streaming: false,
        final_output: true,
    }));

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));

    assert!(app.command_matches.contains(&AppCommand::EditCurrentNote));
    assert!(app.command_matches.contains(&AppCommand::ExportCurrentFile));
    assert!(app.command_matches.contains(&AppCommand::RenameCurrentNote));
    assert!(app.command_matches.contains(&AppCommand::DeleteCurrentNote));
    assert!(app
        .command_matches
        .contains(&AppCommand::ArchiveCurrentNote));
    assert!(!app
        .command_matches
        .contains(&AppCommand::RestoreCurrentNote));

    app.handle_paste("rename");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.pending_file.as_ref(), Some(&current));
    assert_eq!(app.files_context, FilesContext::Rename);
    assert_eq!(app.overlay, Some(Overlay::Dialog));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::RenameFile)
    );
}

#[test]
fn current_file_export_selects_format_cancels_cleanly_and_publishes() {
    let (mut app, _directory) = make_app();
    let output = tempfile::tempdir().unwrap();
    let current = app.storage.data_dir.join("Export me.md");
    fs::write(&current, b"# Export me\n\nExact bytes.\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);

    app.execute_app_command(AppCommand::ExportCurrentFile);
    let dialog = app.dialog.as_ref().unwrap();
    assert_eq!(dialog.purpose, DialogPurpose::ExportFormat);
    assert_eq!(dialog.selected, 0);
    assert_eq!(
        dialog
            .options
            .iter()
            .map(|option| (option.label.as_str(), option.hint.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("Original", Some("Exact source bytes")),
            ("HTML", Some("Safe standalone .html")),
        ]
    );
    app.handle_key(key(KeyCode::Esc));
    assert!(app.pending_export_source.is_none());
    assert!(app.pending_export_format.is_none());

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.pending_export_format, Some(ExportFormat::Html));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::ExportDestination)
    );
    assert_eq!(app.dialog.as_ref().unwrap().message, "Destination path  ");
    assert_eq!(
        app.dialog.as_ref().unwrap().input,
        home_dest("Export me.html")
    );
    assert_eq!(
        app.dialog.as_ref().unwrap().cursor,
        app.dialog.as_ref().unwrap().input.chars().count()
    );
    app.dialog.as_mut().unwrap().input = output
        .path()
        .join("wrong.txt")
        .to_string_lossy()
        .into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
    let cursor = app.dialog.as_ref().unwrap().cursor;
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.dialog.as_ref().unwrap().cursor, cursor - 1);
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.dialog.as_ref().unwrap().cursor, cursor);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::ExportDestination)
    );
    assert!(app.status.contains("must end in .html"));
    assert!(app.dialog.as_ref().unwrap().input.ends_with("wrong.txt"));
    app.handle_key(key(KeyCode::Esc));
    assert!(app.pending_export_source.is_none());
    assert!(app.pending_export_format.is_none());
    assert!(app.pending_export_destination.is_none());

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.dialog.as_ref().unwrap().input,
        home_dest("Export me.md")
    );
    let destination = output.path().join("exact.md");
    app.dialog.as_mut().unwrap().input = destination.to_string_lossy().into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.export_in_progress);
    assert!(app.dialog.is_none());
    assert!(app.status.starts_with("Exporting as Original to "));
    wait_for_export(&mut app);
    assert_eq!(fs::read(&destination).unwrap(), fs::read(&current).unwrap());
    assert!(app.status.starts_with(&format!(
        "Exported {} bytes as Original to ",
        fs::metadata(&current).unwrap().len()
    )));
    assert!(app.notifications.visible().is_some());
    assert!(app.dialog.is_none());
}

#[test]
fn export_format_switch_changes_default_destination_extension() {
    let (mut app, _directory) = make_app();
    let current = app.storage.data_dir.join("Quarterly Report.mb");
    fs::write(&current, b"# Quarterly\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);

    for (index, (expected, format)) in [
        (home_dest("Quarterly Report.mb"), ExportFormat::Original),
        (home_dest("Quarterly Report.html"), ExportFormat::Html),
    ]
    .into_iter()
    .enumerate()
    {
        app.execute_app_command(AppCommand::ExportCurrentFile);
        for _ in 0..index {
            app.handle_key(key(KeyCode::Down));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.pending_export_format, Some(format));
        assert_eq!(
            app.dialog.as_ref().unwrap().input,
            expected,
            "default destination for {}",
            format.label()
        );
        app.handle_key(key(KeyCode::Esc));
        assert!(app.pending_export_source.is_none());
        assert!(app.pending_export_format.is_none());
    }
}

#[test]
fn export_destination_default_follows_configured_export_directory() {
    let (mut app, _directory) = make_app();
    let current = app.storage.data_dir.join("Doc.md");
    fs::write(&current, b"# Doc\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.dialog.as_ref().unwrap().input, home_dest("Doc.md"));

    // A changed setting is picked up by the next export immediately.
    app.handle_key(key(KeyCode::Esc));
    let export_dir = tempfile::tempdir().unwrap();
    let export_dir_text = export_dir.path().display().to_string();
    fs::write(
        &app.storage.settings_path,
        format!("theme = \"default\"\nexport_directory = '{export_dir_text}'\n"),
    )
    .unwrap();
    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.dialog.as_ref().unwrap().input,
        Path::new(&export_dir_text)
            .join("Doc.md")
            .to_string_lossy()
            .as_ref()
    );
    app.handle_key(key(KeyCode::Esc));
}

#[test]
fn export_destination_falls_back_to_file_name_when_export_directory_invalid() {
    let (mut app, _directory) = make_app();
    let current = app.storage.data_dir.join("Doc.md");
    fs::write(&current, b"# Doc\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);

    fs::write(
        &app.storage.settings_path,
        "theme = \"default\"\nexport_directory = \"   \"\n",
    )
    .unwrap();
    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.dialog.as_ref().unwrap().input, "Doc.md");
    assert!(app.status.contains("export_directory"));
    app.handle_key(key(KeyCode::Esc));
}

#[test]
fn daily_note_export_is_available_and_publishes_the_daily_file() {
    let (mut app, _directory) = make_app();
    let output = tempfile::tempdir().unwrap();
    let note = app
        .storage
        .append_daily("2026-08-05", "Daily body\n")
        .unwrap();
    app.reload();
    app.open_daily_document(note.date, DocumentReturn::Daily);
    assert_eq!(
        app.current_export_path(),
        Some(app.storage.daily_file_path("2026-08-05").unwrap())
    );
    assert!(app.command_available(AppCommand::ExportCurrentFile));

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.dialog.as_ref().unwrap().input,
        home_dest("2026-08-05.html")
    );
    let destination = output.path().join("daily.html");
    app.dialog.as_mut().unwrap().input = destination.to_string_lossy().into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.export_in_progress);
    wait_for_export(&mut app);
    let html = fs::read_to_string(&destination).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("Daily body"));
    assert!(app.status.starts_with("Exported "));
    assert!(app.dialog.is_none());
}

#[test]
fn skill_document_can_be_exported() {
    let (mut app, _directory) = make_app();
    let output = tempfile::tempdir().unwrap();
    let skill_path = app.storage.skills_dir.join("guide.md");
    fs::create_dir_all(&app.storage.skills_dir).unwrap();
    fs::write(&skill_path, "# Guide\n\nStep one.\n").unwrap();
    app.show_document(
        DocumentKind::Skill(skill_path.clone()),
        "Skill guide".to_string(),
        "# Guide\n\nStep one.\n".to_string(),
        DocumentReturn::Skills,
    );
    app.center_view = CenterView::Document;
    app.focus = Focus::Center;
    assert!(app.command_available(AppCommand::ExportCurrentFile));

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.dialog.as_ref().unwrap().input, home_dest("guide.html"));
    let destination = output.path().join("guide.html");
    app.dialog.as_mut().unwrap().input = destination.to_string_lossy().into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.export_in_progress);
    wait_for_export(&mut app);
    let html = fs::read_to_string(&destination).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("Step one."));
}

#[test]
fn html_export_surfaces_renderer_diagnostics_in_success_status() {
    let (mut app, _directory) = make_app();
    let output = tempfile::tempdir().unwrap();
    let current = app.storage.data_dir.join("Broken image.md");
    fs::write(&current, "# Broken image\n\n![missing](missing.png)\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    let destination = output.path().join("broken.html");
    app.dialog.as_mut().unwrap().input = destination.to_string_lossy().into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.export_in_progress);
    wait_for_export(&mut app);
    let html = fs::read_to_string(&destination).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(app.status.starts_with("Exported "));
    assert!(app
        .status
        .contains("· warning: image 'missing.png' could not be embedded"));
}

#[test]
fn failed_background_export_restores_destination_for_direct_retry() {
    let (mut app, _directory) = make_app();
    let output = tempfile::tempdir().unwrap();
    let current = app.storage.data_dir.join("Retry me.md");
    fs::write(&current, "# Retry me\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Enter));
    let destination = output.path().join("retry.md");
    app.dialog.as_mut().unwrap().input = destination.to_string_lossy().into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();

    // Simulate a background failure instead of spawning the worker: the
    // submitted source, format, and destination stay in pending state.
    let (sender, receiver) = mpsc::channel();
    sender
        .send(ExportJobResult {
            format: ExportFormat::Original,
            outcome: Err("simulated publish failure".to_string()),
        })
        .unwrap();
    drop(sender);
    app.pending_export_destination = Some(app.dialog.as_ref().unwrap().input.clone());
    app.export_job = Some(receiver);
    app.export_job_format = Some(ExportFormat::Original);
    app.export_in_progress = true;
    app.overlay = None;
    app.dialog = None;

    app.poll_export();
    assert!(!app.export_in_progress);
    assert!(app.status.contains("simulated publish failure"));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::ExportDestination)
    );
    assert_eq!(
        app.dialog.as_ref().unwrap().input,
        destination.to_string_lossy().as_ref()
    );
    assert!(app.pending_export_source.is_some());
    assert_eq!(app.pending_export_format, Some(ExportFormat::Original));

    // A direct retry with the restored destination succeeds.
    app.handle_key(key(KeyCode::Enter));
    assert!(app.export_in_progress);
    wait_for_export(&mut app);
    assert_eq!(fs::read(&destination).unwrap(), fs::read(&current).unwrap());
    assert!(app.status.starts_with("Exported "));
    assert!(app.pending_export_destination.is_none());
}

#[test]
fn existing_export_destination_offers_confirmation_and_cancel_restores_input() {
    let (mut app, _directory) = make_app();
    let output = tempfile::tempdir().unwrap();
    let current = app.storage.data_dir.join("Overwrite me.md");
    fs::write(&current, b"# New body\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);
    let destination = output.path().join("target.md");
    fs::write(&destination, "old content").unwrap();

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Enter));
    app.dialog.as_mut().unwrap().input = destination.to_string_lossy().into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
    app.handle_key(key(KeyCode::Enter));

    // No worker starts; an explicit confirmation is requested instead, and
    // the submitted destination is kept for the cancel path.
    assert!(!app.export_in_progress);
    let dialog = app.dialog.as_ref().unwrap();
    assert_eq!(dialog.purpose, DialogPurpose::ExportOverwrite);
    assert_eq!(dialog.mode, DialogMode::Confirm);
    assert!(dialog.message.contains("already exists"));
    assert_eq!(
        app.pending_export_destination.as_deref(),
        Some(destination.to_str().unwrap())
    );
    assert_eq!(app.pending_export_format, Some(ExportFormat::Original));

    // Cancelling returns to the destination input with the same path, keeps
    // the format, and leaves the existing file untouched.
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::ExportDestination)
    );
    assert_eq!(
        app.dialog.as_ref().unwrap().input,
        destination.to_string_lossy().as_ref()
    );
    assert_eq!(fs::read_to_string(&destination).unwrap(), "old content");
    assert!(!app.export_in_progress);
    assert_eq!(app.pending_export_format, Some(ExportFormat::Original));

    // The restored input is still usable: a fresh path exports without any
    // confirmation and the original destination stays untouched.
    app.dialog.as_mut().unwrap().input = output
        .path()
        .join("fresh.md")
        .to_string_lossy()
        .into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.export_in_progress);
    assert_eq!(app.dialog.as_ref().map(|dialog| dialog.purpose), None);
    wait_for_export(&mut app);
    assert_eq!(
        fs::read(output.path().join("fresh.md")).unwrap(),
        fs::read(&current).unwrap()
    );
    assert_eq!(fs::read_to_string(&destination).unwrap(), "old content");
}

#[test]
fn confirmed_overwrite_replaces_the_existing_destination() {
    let (mut app, _directory) = make_app();
    let output = tempfile::tempdir().unwrap();
    let current = app.storage.data_dir.join("Confirmed.md");
    fs::write(&current, b"# Replacement body\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);
    let destination = output.path().join("target.md");
    fs::write(&destination, "old content").unwrap();

    app.execute_app_command(AppCommand::ExportCurrentFile);
    app.handle_key(key(KeyCode::Enter));
    app.dialog.as_mut().unwrap().input = destination.to_string_lossy().into_owned();
    app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::ExportOverwrite)
    );

    // Confirming starts the original background task, which replaces the
    // destination atomically.
    app.handle_key(key(KeyCode::Enter));
    assert!(app.export_in_progress);
    assert!(app.dialog.is_none());
    assert!(app.status.starts_with("Exporting as Original to "));
    wait_for_export(&mut app);
    assert_eq!(fs::read(&destination).unwrap(), fs::read(&current).unwrap());
    assert!(app.status.starts_with("Exported "));
    assert!(app.pending_export_destination.is_none());
    assert!(app.dialog.is_none());
}

#[cfg(unix)]
#[test]
fn existing_directory_or_symlink_destination_never_offers_overwrite() {
    use std::os::unix::fs::symlink;
    let (mut app, _directory) = make_app();
    let output = tempfile::tempdir().unwrap();
    let current = app.storage.data_dir.join("Kind check.md");
    fs::write(&current, "# Body\n").unwrap();
    app.open_file_document(&current, DocumentReturn::Daily);
    let directory = output.path().join("dir");
    fs::create_dir(&directory).unwrap();
    let target = output.path().join("real.md");
    fs::write(&target, "precious").unwrap();
    let link = output.path().join("link.md");
    symlink(&target, &link).unwrap();

    for bad in [directory.clone(), link.clone()] {
        app.execute_app_command(AppCommand::ExportCurrentFile);
        app.handle_key(key(KeyCode::Enter));
        app.dialog.as_mut().unwrap().input = bad.to_string_lossy().into_owned();
        app.dialog.as_mut().unwrap().cursor = app.dialog.as_ref().unwrap().input.chars().count();
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            app.dialog.as_ref().map(|dialog| dialog.purpose),
            Some(DialogPurpose::ExportDestination),
            "no overwrite confirmation for {bad:?}"
        );
        assert!(!app.export_in_progress);
        assert!(app.status.contains("already exists"));
        app.handle_key(key(KeyCode::Esc));
    }
    assert_eq!(fs::read_to_string(&target).unwrap(), "precious");
}

#[test]
fn failed_export_retry_survives_an_unrelated_dialog() {
    let (mut app, _directory) = make_app();
    let source = app.storage.data_dir.join("Retry after help.md");
    fs::write(&source, "content").unwrap();
    app.pending_export_source = Some(source);
    app.pending_export_format = Some(ExportFormat::Original);
    app.pending_export_destination = Some("retry-after-help.md".to_string());
    let (sender, receiver) = mpsc::channel();
    sender
        .send(ExportJobResult {
            format: ExportFormat::Original,
            outcome: Err("simulated failure behind help".to_string()),
        })
        .unwrap();
    app.export_job = Some(receiver);
    app.export_job_format = Some(ExportFormat::Original);
    app.export_in_progress = true;
    app.open_dialog(DialogState::new(
        "Help",
        String::new(),
        DialogMode::Informational,
        DialogPurpose::Help,
        Vec::new(),
    ));

    app.poll_export();
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::Help)
    );
    assert!(app.pending_export_source.is_some());
    app.close_dialog();
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::ExportDestination)
    );
    assert_eq!(app.dialog.as_ref().unwrap().input, "retry-after-help.md");
}

#[test]
fn archived_note_gets_restore_instead_of_archive_command() {
    let (mut app, _directory) = make_app();
    let note = app.storage.data_dir.join("Archived.md");
    fs::write(&note, "archive me\n").unwrap();
    let archived = app.storage.archive_note(&note).unwrap();
    app.reload_files();
    app.open_file_document(&archived, DocumentReturn::Daily);

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));

    assert!(app
        .command_matches
        .contains(&AppCommand::RestoreCurrentNote));
    assert!(!app
        .command_matches
        .contains(&AppCommand::ArchiveCurrentNote));

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    app.handle_key(key(KeyCode::Char('u')));

    let restored = app.storage.data_dir.join("Archived.md");
    assert!(!archived.exists());
    assert!(restored.exists());
    assert_eq!(app.status, "Note restored");
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(restored))
    );
}

#[test]
fn opening_a_note_keeps_the_file_tree_selection_in_sync() {
    let (mut app, _directory) = make_app();
    let first = app.storage.data_dir.join("First.md");
    let second = app.storage.data_dir.join("Second.md");
    fs::write(&first, "first\n").unwrap();
    fs::write(&second, "second\n").unwrap();
    app.reload_files();
    app.selected_file = Some(first);
    app.notes_expanded = false;

    app.open_file_document(&second, DocumentReturn::Search);

    assert_eq!(app.selected_file.as_ref(), Some(&second));
    assert!(app.notes_expanded);
    assert!(matches!(
        app.visible_file_rows().get(app.file_row),
        Some(FileListRow::File(index)) if app.note_files[*index].path == second
    ));
}

#[test]
fn right_from_the_file_tree_returns_to_the_open_document_without_opening_selection() {
    let (mut app, _directory) = make_app();
    let first = app.storage.data_dir.join("First.md");
    let second = app.storage.data_dir.join("Second.md");
    fs::write(&first, "first\n").unwrap();
    fs::write(&second, "second\n").unwrap();
    app.reload_files();
    app.open_file_document(&first, DocumentReturn::Daily);
    app.open_files();
    let second_row = app
        .visible_file_rows()
        .iter()
        .position(
            |row| matches!(row, FileListRow::File(index) if app.note_files[*index].path == second),
        )
        .unwrap();
    app.select_file_row(second_row);
    assert_eq!(app.selected_file.as_ref(), Some(&second));
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(first.clone()))
    );

    app.handle_key(key(KeyCode::Right));

    assert_eq!(app.focus, Focus::Center);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(first.clone()))
    );
    assert_eq!(app.selected_file.as_ref(), Some(&first));
}

#[test]
fn right_from_an_open_document_focuses_workspace_views_without_changing_page() {
    let (mut app, _directory) = make_app();
    let note = app.storage.data_dir.join("Open.md");
    fs::write(&note, "open document\n").unwrap();
    app.open_file_document(&note, DocumentReturn::Daily);

    app.handle_key(key(KeyCode::Right));

    assert_eq!(app.focus, Focus::Views);
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(note))
    );
}

#[test]
fn right_from_the_file_tree_returns_to_daily_without_opening_a_note() {
    let (mut app, _directory) = make_app();
    let note = app.storage.data_dir.join("Selected.md");
    fs::write(&note, "selected note\n").unwrap();
    app.reload_files();
    app.center_view = CenterView::Daily;
    app.document = None;
    app.open_files();
    let note_row = app
        .visible_file_rows()
        .iter()
        .position(
            |row| matches!(row, FileListRow::File(index) if app.note_files[*index].path == note),
        )
        .unwrap();
    app.select_file_row(note_row);

    app.handle_key(key(KeyCode::Right));

    assert_eq!(app.focus, Focus::Center);
    assert_eq!(app.center_view, CenterView::Daily);
    assert!(app.document.is_none());
    assert_eq!(app.selected_file.as_ref(), Some(&note));
}

#[test]
fn editable_support_files_open_through_the_editor_pipeline() {
    let (mut app, _directory) = make_app();
    let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);

    for (query, expected) in [
        ("ai settings", app.storage.ai_config_path.clone()),
        ("agent instructions", app.storage.agents_path.clone()),
        ("agent memory", app.storage.memory_path.clone()),
        ("template edit", app.storage.template_path.clone()),
    ] {
        app.handle_key(ctrl_p);
        app.handle_paste(query);
        let command = app.handle_key(key(KeyCode::Enter));
        assert_eq!(command, Some(Command::Edit(expected)));
        assert_eq!(app.overlay, None);
    }
}

#[test]
fn command_palette_switches_theme_and_persists_the_selection() {
    let (mut app, _directory) = make_app();
    let custom =
        crate::theme::DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#010203\"");
    fs::write(app.storage.themes_dir.join("custom.toml"), custom).unwrap();

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    app.handle_paste("theme switch");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), None);
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::ThemePicker)
    );

    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.overlay, None);
    assert_eq!(app.theme_selection, "custom");
    assert_eq!(app.active_theme, "custom");
    assert_eq!(app.storage.load_theme_selection().unwrap(), "custom");
    assert_eq!(app.theme.surface_panel, ratatui::style::Color::Rgb(1, 2, 3));
}

#[test]
fn command_palette_and_hash_shortcut_open_centered_tag_browser() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "daily #rust and #rust\nnot #rustlang");
    fs::write(app.storage.data_dir.join("Project.md"), "note #rust\n").unwrap();
    refresh_test_index(&mut app);

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    app.handle_paste("tags browse");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.overlay, None);
    assert_eq!(app.center_view, CenterView::Tags);
    assert_eq!(app.focus, Focus::Center);
    let rust = app
        .tag_results
        .iter()
        .find(|tag| tag.name == "rust")
        .unwrap();
    assert_eq!((rust.documents, rust.mentions), (2, 3));

    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.center_view, CenterView::Tags);
    assert_eq!(app.active_tag.as_deref(), Some("rust"));
    assert_eq!(app.tag_notes.len(), 2, "one card per distinct note");
    assert!(app
        .tag_notes
        .iter()
        .any(|note| note.body.contains("daily #rust")));
    assert!(app.tag_notes.iter().all(|note| note
        .path
        .file_stem()
        .is_some_and(|stem| stem.to_string_lossy() == note.title)));
    assert!(app
        .tag_notes
        .iter()
        .any(|note| note.body.contains("note #rust")));
    assert!(app
        .tag_notes
        .iter()
        .all(|note| !note.body.contains("#rustlang") || note.body.contains("daily #rust")));

    app.tag_note_index = 1;
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.center_view, CenterView::Document);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.center_view, CenterView::Tags);
    assert_eq!(app.active_tag.as_deref(), Some("rust"));
    assert_eq!(app.tag_note_index, 1);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.active_tag, None);

    app.center_view = CenterView::Daily;
    app.handle_key(key(KeyCode::Char('#')));
    assert_eq!(app.center_view, CenterView::Tags);
    app.handle_paste("lang");
    assert_eq!(app.tag_query, "lang");
    assert_eq!(app.tag_results.len(), 1);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.center_view, CenterView::Daily);

    app.center_view = CenterView::Document;
    app.handle_key(key(KeyCode::Char('#')));
    assert_eq!(app.center_view, CenterView::Tags);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.center_view, CenterView::Document);
}

#[test]
fn command_palette_renames_an_exact_tag_across_the_workspace() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "daily #old and `#old`");
    let note = app.storage.data_dir.join("Project.md");
    fs::write(&note, "note #old and #oldlang\n").unwrap();
    refresh_test_index(&mut app);

    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    app.handle_paste("tags rename");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::TagRenameSource)
    );
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.pending_tag_rename.as_deref(), Some("old"));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::TagRenameTarget)
    );
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.mode),
        Some(DialogMode::SingleLine)
    );
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.message.as_str()),
        Some("New tag  #")
    );
    app.handle_paste("new/\ntag");
    assert_eq!(app.dialog.as_ref().unwrap().input, "new/tag");
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.overlay, None);
    assert!(app.status.contains("2 documents (2 mentions)"));
    assert_eq!(
        fs::read_to_string(note).unwrap(),
        "note #new/tag and #oldlang\n"
    );
    let daily = app
        .storage
        .daily_file_path(&app.daily_notes[0].date.to_string())
        .unwrap();
    assert_eq!(
        fs::read_to_string(daily).unwrap(),
        "daily #new/tag and `#old`\n"
    );
    assert_eq!(
        app.workspace_index
            .with_index(|index| index.exact_tag_hits("new/tag", None).len()),
        Some(2)
    );
}

#[test]
fn ctrl_p_toggles_the_command_palette_without_replacing_other_dialogs() {
    let (mut app, _directory) = make_app();
    let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    app.handle_key(ctrl_p);
    assert_eq!(app.overlay, Some(Overlay::Dialog));
    app.handle_key(ctrl_p);
    assert_eq!(app.overlay, None);

    app.open_help();
    app.handle_key(ctrl_p);
    assert_eq!(app.overlay, Some(Overlay::Help));
}

#[test]
fn tab_cycles_permission_mode_approve_auto_yolo_without_changing_focus() {
    let (mut app, _directory) = make_app();
    assert_eq!(app.focus, Focus::Center);
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.permission_mode, PermissionMode::Auto);
    assert_eq!(app.focus, Focus::Center);
    assert_eq!(
        app.permission_mode_atomic.load(Ordering::Relaxed),
        PermissionMode::Auto.code()
    );
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.permission_mode, PermissionMode::Yolo);
    assert_eq!(
        app.permission_mode_atomic.load(Ordering::Relaxed),
        PermissionMode::Yolo.code()
    );
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.permission_mode, PermissionMode::Approve);
    assert_eq!(
        app.permission_mode_atomic.load(Ordering::Relaxed),
        PermissionMode::Approve.code()
    );
}

#[test]
fn ai_action_opens_an_optional_prompt_overlay() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "card body");
    let date = app.selected_date().unwrap();
    app.dispatch_action(date, Action::Ai);
    assert_eq!(app.overlay, Some(Overlay::AiPrompt));
    assert_eq!(app.ai_source_date, Some(date));
    app.handle_paste("custom prompt");
    assert_eq!(app.ai_prompt_input, "custom prompt");
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, None);
}

#[test]
fn daily_ai_custom_prompt_includes_the_daily_file_path() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "card body that should not become the prompt");
    let date = app.selected_date().unwrap();
    let path = app.storage.daily_file_path(&date.to_string()).unwrap();
    let display_path = path
        .strip_prefix(&app.storage.root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.ai_running = true;

    app.dispatch_action(date, Action::Ai);
    app.handle_paste("Extract the action items");
    app.handle_key(key(KeyCode::Enter));

    let prompts = app.agent_input_buffer.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains(&display_path));
    assert!(prompts[0].contains("Extract the action items"));
    assert!(!prompts[0].contains("card body that should not become the prompt"));
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Prompt { text, muted: true })
            if text == "Extract the action items"
    ));
}

#[test]
fn empty_daily_ai_prompt_requests_in_place_markdown_formatting() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "unformatted card body");
    let date = app.selected_date().unwrap();
    let path = app.storage.daily_file_path(&date.to_string()).unwrap();
    let display_path = path
        .strip_prefix(&app.storage.root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    app.ai_running = true;

    app.dispatch_action(date, Action::Ai);
    app.handle_key(key(KeyCode::Enter));

    let prompts = app.agent_input_buffer.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains(&display_path));
    assert!(prompts[0].contains(FORMAT_DAILY_NOTE_PROMPT));
    assert!(!prompts[0].contains("unformatted card body"));
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Prompt { text, muted: true })
            if text == &format!("Format {display_path}")
    ));
}

#[test]
fn approval_overlay_sends_the_user_decision() {
    let (mut app, _directory) = make_app();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    app.ai_approval_sender = Some(sender);
    app.approval_request = Some(ApprovalRequest {
        title: "Update note".to_string(),
        message: "--- old\n+++ new\n-old\n+new\n".to_string(),
        kind: ApprovalKind::Diff,
    });
    app.set_overlay(Overlay::Approval);
    app.handle_key(key(KeyCode::Char('y')));
    assert_eq!(receiver.try_recv().unwrap(), ApprovalDecision::Approve);
    assert_eq!(app.overlay, None);
    assert!(app.approval_request.is_none());

    install_agent_terminal_snapshot(&mut app);
    app.approval_request = Some(ApprovalRequest {
        title: "Send terminal input".to_string(),
        message: String::new(),
        kind: ApprovalKind::Confirm,
    });
    app.set_overlay(Overlay::Approval);
    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(receiver.try_recv().unwrap(), ApprovalDecision::Deny);
    assert!(!app.agent_terminal.is_active());
}

#[test]
fn ask_user_overlay_accepts_options_and_custom_text() {
    let (mut app, _directory) = make_app();
    let event_sender = install_agent_observable(&mut app);
    let (answer_sender, mut answer_receiver) = tokio::sync::mpsc::unbounded_channel();
    app.ai_user_sender = Some(answer_sender);
    app.agent_panel.push(Arc::new(AgentPanelEntry::Tool {
        text: "Calling Ask...\nChoose a format".to_string(),
        active: true,
        preview: None,
    }));
    event_sender
        .send(AgentEvent::AskUser(AskUserRequest {
            kind: AskUserKind::Tool,
            question: "Choose a format".to_string(),
            options: vec!["Markdown".to_string(), "MBDown".to_string()],
        }))
        .unwrap();
    app.poll_agent();
    assert_eq!(app.overlay, Some(Overlay::AskUser));

    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        answer_receiver.try_recv().unwrap(),
        AskUserResponse::Answer("MBDown".to_string())
    );
    assert_eq!(app.overlay, None);
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Tool { text, active: true, .. })
            if text == "Calling Ask...\nChoose a format\nMBDown"
    ));

    event_sender
        .send(AgentEvent::ToolFinished {
            id: "ask-user".to_string(),
            message: "Completed Ask.\nChoose a format".to_string(),
            preview: None,
        })
        .unwrap();
    app.poll_agent();
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Tool { text, active: false, .. })
            if text == "Completed Ask.\nChoose a format\nMBDown"
    ));

    event_sender
        .send(AgentEvent::AskUser(AskUserRequest {
            kind: AskUserKind::Tool,
            question: "Anything else?".to_string(),
            options: vec!["No".to_string()],
        }))
        .unwrap();
    app.poll_agent();
    app.handle_key(key(KeyCode::Char('Y')));
    app.handle_paste("es, use colors");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        answer_receiver.try_recv().unwrap(),
        AskUserResponse::Answer("Yes, use colors".to_string())
    );
}

#[test]
fn round_limit_dialog_submits_continue_and_escape_submits_stop() {
    let (mut app, _directory) = make_app();
    let event_sender = install_agent_observable(&mut app);
    let (answer_sender, mut answer_receiver) = tokio::sync::mpsc::unbounded_channel();
    app.ai_user_sender = Some(answer_sender);
    let request = AskUserRequest {
        kind: AskUserKind::RoundLimit,
        question: "Continue for another segment?".to_string(),
        options: vec!["Continue".to_string(), "Stop".to_string()],
    };

    event_sender
        .send(AgentEvent::AskUser(request.clone()))
        .unwrap();
    app.poll_agent();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        answer_receiver.try_recv().unwrap(),
        AskUserResponse::Answer("Continue".to_string())
    );
    assert_eq!(app.status, "Agent continuing");

    event_sender.send(AgentEvent::AskUser(request)).unwrap();
    app.poll_agent();
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(
        answer_receiver.try_recv().unwrap(),
        AskUserResponse::Answer("Stop".to_string())
    );
    assert_eq!(app.status, "Agent stopping at the request-round limit");
}

#[test]
fn agent_panel_appends_streaming_activity_and_final_reply() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);
    app.ai_running = true;
    app.agent_panel = vec![
        Arc::new(AgentPanelEntry::Prompt {
            text: "First follow-up".to_string(),
            muted: true,
        }),
        Arc::new(AgentPanelEntry::Prompt {
            text: "Second follow-up".to_string(),
            muted: true,
        }),
    ];
    sender.send(AgentEvent::BufferedInputConsumed(1)).unwrap();
    sender
        .send(AgentEvent::AssistantDelta("I need to inspect ".to_string()))
        .unwrap();
    sender
        .send(AgentEvent::AssistantDelta("the source first.".to_string()))
        .unwrap();
    sender
        .send(AgentEvent::ToolStarted {
            id: "read".to_string(),
            message: "Calling Read File...".to_string(),
        })
        .unwrap();
    sender
        .send(AgentEvent::Round {
            current: 2,
            limit: 25,
        })
        .unwrap();
    sender
        .send(AgentEvent::Usage(TokenUsage {
            input_tokens: 1_000,
            output_tokens: 200,
            cache_creation_input_tokens: 300,
            cache_read_input_tokens: 700,
        }))
        .unwrap();
    sender
        .send(AgentEvent::ContextWindow {
            tokens: 1_700,
            capacity: 200_000,
        })
        .unwrap();
    sender
        .send(AgentEvent::ResponseTiming {
            output_tokens: 200,
            elapsed: Duration::from_secs(2),
        })
        .unwrap();
    app.poll_agent();
    assert_eq!(app.status, "Calling Read File...");
    assert!(matches!(
        app.agent_panel[2].as_ref(),
        AgentPanelEntry::Assistant { text, streaming: true, .. }
            if text == "I need to inspect the source first."
    ));
    assert!(matches!(
        app.agent_panel[3].as_ref(),
        AgentPanelEntry::Tool { text, active: true, .. } if text == "Calling Read File..."
    ));
    assert_eq!(app.agent_round, 2);
    assert_eq!(app.agent_round_limit, 25);
    assert_eq!(app.agent_usage.total_input(), 2_000);
    assert_eq!(app.agent_context_window, 1_700);
    assert_eq!(app.agent_context_capacity, 200_000);
    assert_eq!(app.agent_timed_output_tokens, 200);
    assert_eq!(app.agent_response_duration, Duration::from_secs(2));
    assert!(matches!(
        app.agent_panel[0].as_ref(),
        AgentPanelEntry::Prompt { muted: false, .. }
    ));
    assert!(matches!(
        app.agent_panel[1].as_ref(),
        AgentPanelEntry::Prompt { muted: true, .. }
    ));

    sender
        .send(AgentEvent::ToolFinished {
            id: "read".to_string(),
            message: "Completed Read File.".to_string(),
            preview: Some("first line of the note".to_string()),
        })
        .unwrap();
    app.poll_agent();
    assert!(matches!(
        app.agent_panel[3].as_ref(),
        AgentPanelEntry::Tool {
            text,
            active: false,
            preview,
        } if text == "Completed Read File."
            && preview.as_deref() == Some("first line of the note")
    ));

    sender
        .send(AgentEvent::AssistantMessageFinished {
            text: "I need to inspect the source first.".to_string(),
            final_output: false,
        })
        .unwrap();
    sender
        .send(AgentEvent::AssistantDelta("final reply".to_string()))
        .unwrap();
    sender
        .send(AgentEvent::AssistantMessageFinished {
            text: "final reply".to_string(),
            final_output: true,
        })
        .unwrap();
    sender
        .send(AgentEvent::Finished(Ok("final reply".to_string())))
        .unwrap();
    app.poll_agent();
    assert_eq!(app.agent_panel.len(), 5);
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Assistant { text, streaming: false, final_output: true })
            if text == "final reply"
    ));
    assert_eq!(app.status, "Agent finished");
    assert_eq!(
        app.notifications.visible().as_deref(),
        Some("Agent finished")
    );
    assert_eq!(app.notifications.take_bells(), 1);
}

#[test]
fn thinking_events_build_and_finish_thinking_entries() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);
    app.ai_running = true;
    sender
        .send(AgentEvent::ThinkingDelta("Let me read".to_string()))
        .unwrap();
    sender
        .send(AgentEvent::ThinkingDelta(" the file.".to_string()))
        .unwrap();
    app.poll_agent();
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Thinking { text, streaming: true })
            if text == "Let me read the file."
    ));
    sender.send(AgentEvent::ThinkingFinished).unwrap();
    app.poll_agent();
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Thinking {
            streaming: false,
            ..
        })
    ));
}

#[test]
fn cancelling_agent_finishes_streaming_thinking_entry() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);
    app.ai_running = true;
    install_agent_terminal_snapshot(&mut app);
    sender
        .send(AgentEvent::ThinkingDelta("Still reasoning".to_string()))
        .unwrap();
    app.poll_agent();
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Thinking {
            streaming: true,
            ..
        })
    ));

    app.cancel_agent();

    assert!(!app.ai_running);
    assert!(!app.agent_terminal.is_active());
    assert!(app.agent_panel.iter().all(|entry| !matches!(
        entry.as_ref(),
        AgentPanelEntry::Thinking {
            streaming: true,
            ..
        }
    )));
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Error(text)) if text == "Cancelled"
    ));
}

#[test]
fn ordinary_text_stays_an_assistant_message() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);
    app.ai_running = true;
    sender
        .send(AgentEvent::AssistantDelta("I need to inspect".to_string()))
        .unwrap();
    app.poll_agent();
    sender
        .send(AgentEvent::AssistantMessageFinished {
            text: "I need to inspect".to_string(),
            final_output: false,
        })
        .unwrap();
    app.poll_agent();
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Assistant {
            text,
            streaming: false,
            final_output: false,
        }) if text == "I need to inspect"
    ));
}

#[test]
fn manual_agent_scroll_disables_tail_follow() {
    let (mut app, _directory) = make_app();
    app.agent_follow_tail = true;
    app.agent_scroll = 20;

    app.scroll_agent_by(-5);

    assert!(!app.agent_follow_tail);
    assert_eq!(app.agent_scroll, 15);
}

#[test]
fn streaming_events_preserve_manual_agent_scroll() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);
    app.ai_running = true;
    app.agent_follow_tail = false;
    app.agent_scroll = 7;
    sender
        .send(AgentEvent::ThinkingDelta("still working".to_string()))
        .unwrap();
    sender
        .send(AgentEvent::AssistantDelta("response".to_string()))
        .unwrap();

    app.poll_agent();

    assert!(!app.agent_follow_tail);
    assert_eq!(app.agent_scroll, 7);
}

#[test]
fn concurrent_tool_events_finish_the_matching_timeline_entries() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);
    sender
        .send(AgentEvent::ToolStarted {
            id: "a".to_string(),
            message: "Fetching Web...\nhttps://a.example".to_string(),
        })
        .unwrap();
    sender
        .send(AgentEvent::ToolStarted {
            id: "b".to_string(),
            message: "Fetching Web...\nhttps://b.example".to_string(),
        })
        .unwrap();
    sender
        .send(AgentEvent::ToolFinished {
            id: "a".to_string(),
            message: "Completed Web Fetch.\nhttps://a.example".to_string(),
            preview: None,
        })
        .unwrap();
    app.poll_agent();

    assert!(matches!(
        app.agent_panel[0].as_ref(),
        AgentPanelEntry::Tool { text, active: false, .. }
            if text.contains("https://a.example")
    ));
    assert!(matches!(
        app.agent_panel[1].as_ref(),
        AgentPanelEntry::Tool { text, active: true, .. }
            if text.contains("https://b.example")
    ));
}

#[test]
fn agent_retry_events_clear_partial_output_and_accumulate_metrics() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);

    sender
        .send(AgentEvent::AssistantDelta("partial".to_string()))
        .unwrap();
    sender
        .send(AgentEvent::ThinkingDelta("partial thought".to_string()))
        .unwrap();
    sender.send(AgentEvent::Retry).unwrap();
    sender.send(AgentEvent::Retry).unwrap();
    app.poll_agent();

    assert_eq!(app.agent_retry_count, 2);
    assert!(app.agent_panel.iter().all(|entry| !matches!(
        entry.as_ref(),
        AgentPanelEntry::Assistant {
            streaming: true,
            ..
        } | AgentPanelEntry::Thinking {
            streaming: true,
            ..
        }
    )));
}

#[test]
fn agent_terminal_outcomes_send_distinct_notifications() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);
    app.ai_running = true;

    sender
        .send(AgentEvent::Stopped(AgentStopReason::RequestRoundLimit))
        .unwrap();
    app.poll_agent();

    assert_eq!(app.status, "Agent paused at the request-round limit");
    assert_eq!(
        app.notifications.visible().as_deref(),
        Some("Agent stopped at the request-round limit")
    );
    assert_eq!(app.notifications.take_bells(), 1);

    let sender = install_agent_observable(&mut app);
    app.ai_running = true;
    sender
        .send(AgentEvent::Stopped(AgentStopReason::ToolApprovalDenied))
        .unwrap();
    app.poll_agent();

    assert_eq!(app.status, "Agent stopped after tool approval was denied");
    assert_eq!(
        app.notifications.visible().as_deref(),
        Some("Agent stopped after tool approval was denied")
    );
    assert_eq!(app.notifications.take_bells(), 1);

    let sender = install_agent_observable(&mut app);
    app.ai_running = true;
    sender
        .send(AgentEvent::Finished(Err("network unavailable".to_string())))
        .unwrap();
    app.poll_agent();

    assert_eq!(app.status, "AI error: network unavailable");
    assert_eq!(
        app.notifications.visible().as_deref(),
        Some("AI error: network unavailable")
    );
    assert_eq!(app.notifications.take_bells(), 1);
}

#[test]
fn application_errors_notify_but_agent_tool_failures_do_not() {
    let (mut app, _directory) = make_app();
    app.set_error("Open error: application unavailable");
    assert_eq!(app.status, "Open error: application unavailable");
    assert_eq!(
        app.notifications.visible().as_deref(),
        Some("Open error: application unavailable")
    );
    assert_eq!(app.notifications.take_bells(), 1);

    let sender = install_agent_observable(&mut app);
    sender
        .send(AgentEvent::ToolStarted {
            id: "failed-read".to_string(),
            message: "Calling Read File...".to_string(),
        })
        .unwrap();
    sender
        .send(AgentEvent::ToolFinished {
            id: "failed-read".to_string(),
            message: "Failed Read File: file not found".to_string(),
            preview: None,
        })
        .unwrap();
    app.poll_agent();

    assert_eq!(app.status, "Failed Read File: file not found");
    assert_eq!(
        app.notifications.visible().as_deref(),
        Some("Open error: application unavailable")
    );
    assert_eq!(app.notifications.take_bells(), 0);
}

#[test]
fn next_round_replaces_a_failed_tool_status_with_working_state() {
    let (mut app, _directory) = make_app();
    let sender = install_agent_observable(&mut app);
    app.ai_running = true;
    sender
        .send(AgentEvent::ToolStarted {
            id: "bad-write".to_string(),
            message: "Calling Write...".to_string(),
        })
        .unwrap();
    sender
        .send(AgentEvent::ToolFinished {
            id: "bad-write".to_string(),
            message: "Failed Write: MBDown validation failed".to_string(),
            preview: None,
        })
        .unwrap();
    sender
        .send(AgentEvent::Round {
            current: 2,
            limit: 25,
        })
        .unwrap();

    app.poll_agent();

    assert!(app.ai_running);
    assert_eq!(app.agent_round, 2);
    assert_eq!(app.status, "AI is working...");
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Tool {
            text,
            active: false,
            ..
        }) if text == "Failed Write: MBDown validation failed"
    ));
}

#[test]
fn agent_open_file_event_displays_the_note_in_the_tui() {
    let (mut app, _directory) = make_app();
    let note = app.storage.data_dir.join("Agent View.md");
    fs::write(&note, "# Opened by Agent\n").unwrap();
    let note = fs::canonicalize(note).unwrap();
    let sender = install_agent_observable(&mut app);

    sender.send(AgentEvent::OpenFile(note.clone())).unwrap();
    app.poll_agent();

    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(app.focus, Focus::Center);
    assert!(matches!(
        app.document.as_ref().map(|document| &document.kind),
        Some(DocumentKind::File(path)) if path == &note
    ));
    assert_eq!(app.status, format!("Agent opened {}", note.display()));
}

#[test]
fn c_cancels_a_running_agent_only_from_the_agent_panel() {
    let (mut app, _directory) = make_app();
    let observable_cancel = tokio_util::sync::CancellationToken::new();
    let (_sender, events) = tokio::sync::broadcast::channel(1);
    app.active_agent = Some(Observable {
        output: Box::pin(std::future::pending()),
        events,
        cancel: observable_cancel.clone(),
    });
    let cancelled = Arc::new(AtomicBool::new(false));
    app.ai_cancel = Some(cancelled.clone());
    app.ai_running = true;
    app.agent_panel.push(Arc::new(AgentPanelEntry::Tool {
        text: "Fetching Web...".to_string(),
        active: true,
        preview: None,
    }));
    app.focus = Focus::Center;

    app.handle_key(key(KeyCode::Char('c')));
    assert!(app.ai_running);
    assert!(!cancelled.load(Ordering::Relaxed));
    assert!(!observable_cancel.is_cancelled());

    app.focus = Focus::Agent;
    app.handle_key(key(KeyCode::Char('c')));
    assert!(!app.ai_running);
    assert!(cancelled.load(Ordering::Relaxed));
    assert!(observable_cancel.is_cancelled());
    assert!(
        matches!(app.agent_panel.last().map(|entry| entry.as_ref()), Some(AgentPanelEntry::Error(text)) if text == "Cancelled")
    );
    assert!(matches!(
        app.agent_panel[0].as_ref(),
        AgentPanelEntry::Tool { active: false, .. }
    ));
    assert!(app.ai_approval_sender.is_some());
    assert!(app.ai_user_sender.is_some());
    assert_eq!(app.status, "Agent task cancelled");
    assert_eq!(
        app.notifications.visible().as_deref(),
        Some("Agent task cancelled")
    );
    assert_eq!(app.notifications.take_bells(), 1);
}

#[test]
fn uppercase_c_cancels_work_and_clears_the_agent_session() {
    let (mut app, _directory) = make_app();
    let cancelled = Arc::new(AtomicBool::new(false));
    app.ai_cancel = Some(cancelled.clone());
    app.ai_running = true;
    app.agent_panel = vec![
        Arc::new(AgentPanelEntry::Prompt {
            text: "Current prompt".to_string(),
            muted: false,
        }),
        Arc::new(AgentPanelEntry::Tool {
            text: "Searching Web...".to_string(),
            active: true,
            preview: None,
        }),
        Arc::new(AgentPanelEntry::Assistant {
            text: "Looking for sources.".to_string(),
            streaming: true,
            final_output: false,
        }),
    ];
    app.agent_conversation = AgentConversation::seeded_for_test();
    app.focus = Focus::Agent;

    app.handle_key(key(KeyCode::Char('C')));

    assert!(cancelled.load(Ordering::Relaxed));
    assert!(!app.ai_running);
    assert!(!app.agent_conversation.clear());
    assert!(app.agent_panel.is_empty());
    assert_eq!(app.status, "Agent session cleared");
}

#[test]
fn recording_from_a_document_appends_silently_and_pins_scroll_to_end() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Article.md");
    fs::write(&path, "# Article\n\nInspiration\n").unwrap();
    app.open_file_document(&path, DocumentReturn::Daily);
    let document = app.document.as_mut().unwrap();
    document.scroll = 1;
    document.target_line = Some(1);
    app.status = "Old status".to_string();
    app.handle_key(key(KeyCode::Char('i')));
    assert_eq!(app.focus, Focus::Compose);
    app.handle_paste("new idea");
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(path.clone()))
    );
    assert_eq!(app.document.as_ref().unwrap().scroll, u16::MAX);
    assert_eq!(app.document.as_ref().unwrap().target_line, None);
    assert!(app.daily_notes.is_empty());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "# Article\n\nInspiration\n\nnew idea\n"
    );
    assert_eq!(
        app.document.as_ref().unwrap().source,
        "# Article\n\nInspiration\n\nnew idea\n"
    );
    assert!(app.notifications.visible().is_none());
    assert!(app.status.is_empty());
}

#[test]
fn ctrl_u_recalls_an_article_append_into_compose() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Article.md");
    fs::write(&path, "# Article\n").unwrap();
    app.open_file_document(&path, DocumentReturn::Daily);
    app.handle_key(key(KeyCode::Char('i')));
    app.handle_paste("  mistaken prompt \n");
    app.handle_key(key(KeyCode::Enter));

    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

    assert_eq!(app.input, "  mistaken prompt \n");
    assert_eq!(app.input_cursor, app.input.chars().count());
    assert_eq!(fs::read_to_string(&path).unwrap(), "# Article\n");
    assert_eq!(app.document.as_ref().unwrap().source, "# Article\n");
    assert_eq!(app.status, "Recalled last append");
}

#[test]
fn ctrl_u_recalls_the_first_daily_append_and_removes_its_file() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Compose;
    app.handle_paste("send this to Agent");
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.daily_notes.len(), 1);

    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

    assert_eq!(app.input, "send this to Agent");
    assert_eq!(app.input_cursor, app.input.chars().count());
    assert!(app.daily_notes.is_empty());
    assert_eq!(app.status, "Recalled last append");
}

#[test]
fn ctrl_u_refuses_to_truncate_a_note_changed_after_append() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Article.md");
    fs::write(&path, "# Article\n").unwrap();
    app.open_file_document(&path, DocumentReturn::Daily);
    app.handle_key(key(KeyCode::Char('i')));
    app.handle_paste("mistaken prompt");
    app.handle_key(key(KeyCode::Enter));
    fs::write(&path, "# Article\n\nmistaken prompt\n\nexternal edit\n").unwrap();

    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

    assert!(app.input.is_empty());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "# Article\n\nmistaken prompt\n\nexternal edit\n"
    );
    assert!(app.status.starts_with("Recall error:"));
}

#[test]
fn recording_from_a_daily_preview_appends_to_that_date() {
    let (mut app, _directory) = make_app();
    app.storage.append_daily("2026-07-26", "first").unwrap();
    app.reload();
    app.open_daily_document(
        NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        DocumentReturn::Daily,
    );
    app.handle_key(key(KeyCode::Char('i')));
    app.handle_paste("second");
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.storage.read_daily_by_date("2026-07-26").unwrap().body,
        "first\n\nsecond"
    );
    assert_eq!(app.document.as_ref().unwrap().source, "first\n\nsecond");
    assert_eq!(
        app.notifications.visible().as_deref(),
        Some("Appended to Daily 2026-07-26")
    );
}

#[test]
fn reload_keeps_the_selected_daily_note_by_date() {
    let (mut app, _directory) = make_app();
    let first = app.storage.append_daily("2026-07-26", "first").unwrap();
    let second = app.storage.append_daily("2026-07-27", "second").unwrap();
    app.reload();
    app.selected = app
        .daily_notes
        .iter()
        .position(|note| note.date == second.date)
        .unwrap();

    app.storage.remove_daily(&first.date.to_string()).unwrap();
    app.reload();

    assert_eq!(app.selected_date(), Some(second.date));
}

#[test]
fn workspace_reload_applies_theme_changes_and_invalidates_render_caches() {
    let (mut app, _directory) = make_app();
    app.daily_vlist.width = 80;
    app.agent_vlist.width = 40;
    let custom =
        crate::theme::DEFAULT_THEME_TOML.replace("panel = \"#181825\"", "panel = \"#010203\"");
    fs::write(app.storage.themes_dir.join("custom.toml"), custom).unwrap();
    app.storage.write_theme_selection("custom").unwrap();

    app.reload_workspace();

    assert_eq!(app.theme.surface_panel, ratatui::style::Color::Rgb(1, 2, 3));
    assert_eq!(app.daily_vlist.width, 0);
    assert_eq!(app.agent_vlist.width, 0);
}

#[test]
fn compose_paste_normalizes_newlines_at_character_cursor() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Compose;
    app.input = "ab".to_string();
    app.input_cursor = 1;
    app.handle_paste("X\r\nY\rZ");
    assert_eq!(app.input, "aX\nY\nZb");
    assert_eq!(app.input_cursor, 6);
}

#[test]
fn compose_agent_prompt_includes_the_path_for_the_current_content_view() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "Daily context");
    let date = app.selected_date().unwrap();
    app.input = "Summarize the key point".to_string();

    let prompt = app.compose_agent_prompt().unwrap();
    assert!(prompt.contains(&format!("currently viewing daily note: daily/{date}.md")));
    assert!(prompt.ends_with("Summarize the key point"));

    let path = app.storage.data_dir.join("Reference.md");
    fs::write(&path, "# Reference\n").unwrap();
    app.open_file_document(&path, DocumentReturn::Daily);

    let prompt = app.compose_agent_prompt().unwrap();
    assert!(prompt.contains("currently viewing note: data/Reference.md"));
    assert!(prompt.ends_with("Summarize the key point"));

    app.document = Some(Document {
        kind: DocumentKind::Daily(date),
        title: "Daily".to_string(),
        source: String::new(),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    });
    let prompt = app.compose_agent_prompt().unwrap();
    assert!(prompt.contains(&format!("currently viewing daily note: daily/{date}.md")));

    app.center_view = CenterView::Chat;
    assert_eq!(
        app.compose_agent_prompt().as_deref(),
        Some("Summarize the key point")
    );
}

#[test]
fn ctrl_enter_sends_compose_to_agent_without_creating_a_chat_card() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Compose;
    let daily_count = app.daily_notes.len();
    app.agent_usage.input_tokens = 1_234;
    app.agent_timed_output_tokens = 400;
    app.agent_response_duration = Duration::from_secs(2);
    app.input = "Direct Agent prompt".to_string();
    app.input_cursor = app.input.chars().count();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    assert!(app.ai_running);
    assert!(matches!(
        app.agent_panel.last().map(|entry| entry.as_ref()),
        Some(AgentPanelEntry::Prompt { text, muted: false }) if text == "Direct Agent prompt"
    ));
    assert!(app.input.is_empty());
    assert_eq!(app.input_cursor, 0);
    assert_eq!(app.daily_notes.len(), daily_count);
    assert_eq!(app.agent_usage.input_tokens, 1_234);
    assert_eq!(app.agent_timed_output_tokens, 400);
    assert_eq!(app.agent_response_duration, Duration::from_secs(2));
}

#[test]
fn ctrl_enter_buffers_and_clears_compose_while_agent_is_busy() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Compose;
    app.ai_running = true;
    app.agent_panel.push(Arc::new(AgentPanelEntry::Prompt {
        text: "Initial prompt".to_string(),
        muted: false,
    }));
    app.input = "Additional prompt".to_string();
    app.input_cursor = app.input.chars().count();

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    app.input = "One more detail".to_string();
    app.input_cursor = app.input.chars().count();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));

    assert!(app.input.is_empty());
    assert_eq!(app.input_cursor, 0);
    assert_eq!(
        app.agent_panel
            .iter()
            .map(|entry| entry.as_ref().clone())
            .collect::<Vec<_>>(),
        vec![
            AgentPanelEntry::Prompt {
                text: "Initial prompt".to_string(),
                muted: false,
            },
            AgentPanelEntry::Prompt {
                text: "Additional prompt".to_string(),
                muted: true,
            },
            AgentPanelEntry::Prompt {
                text: "One more detail".to_string(),
                muted: true,
            }
        ]
    );
    assert_eq!(
        *app.agent_input_buffer.lock().unwrap(),
        ["Additional prompt", "One more detail"]
    );
    assert_eq!(app.status, "Prompt buffered for Agent");
}

#[test]
fn f_focuses_files_and_t_opens_the_todo_page() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    app.handle_key(key(KeyCode::Char('f')));
    assert_eq!(app.focus, Focus::Files);
    assert_eq!(app.center_view, CenterView::Daily);
    app.handle_key(key(KeyCode::Char('t')));
    assert_eq!(app.focus, Focus::Center);
    assert_eq!(app.center_view, CenterView::Todo);
}

#[test]
fn todo_filter_accepts_keyboard_and_paste_then_toggles_the_visible_task() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "- [ ] write docs\n- [ ] fix release");
    app.open_todo();

    for character in ['f', 'i', 'x'] {
        app.handle_key(key(KeyCode::Char(character)));
    }
    assert_eq!(app.focus, Focus::Center, "filter characters stay in Todo");
    assert_eq!(app.todo_query, "fix");
    assert_eq!(app.visible_todo_indices(), vec![1]);
    assert_eq!(app.todo_index, 1);

    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Backspace));
    app.handle_key(key(KeyCode::Char('i')));
    app.handle_key(key(KeyCode::End));
    app.handle_paste(" rele\nase");
    assert_eq!(app.todo_query, "fix release");
    assert_eq!(app.todo_cursor, app.todo_query.chars().count());

    app.handle_key(key(KeyCode::Enter));
    assert!(app.todo_items[1].checked);
    assert!(!app.todo_items[0].checked);
    assert_eq!(app.todo_query, "fix release");
}

#[test]
fn todo_filter_with_no_matches_does_not_toggle_an_unrelated_task() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "- [ ] write docs");
    app.open_todo();
    app.handle_paste("missing");

    assert!(app.visible_todo_indices().is_empty());
    app.handle_key(key(KeyCode::Enter));

    assert!(!app.todo_items[0].checked);
}

#[test]
fn workspace_view_registry_drives_sidebar_selection() {
    let (mut app, _directory) = make_app();
    assert_eq!(
        WorkspaceView::ALL
            .iter()
            .map(|view| (view.label, view.description, view.center_view))
            .collect::<Vec<_>>(),
        [
            ("Agent", "AI conversation", CenterView::Chat),
            ("TODO", "Tasks", CenterView::Todo),
            ("Search", "Find notes", CenterView::Search),
            ("Tag", "Browse tags", CenterView::Tags),
            ("Attachment", "Browse attachments", CenterView::Attachments),
            ("Daily", "Daily notes", CenterView::Daily),
        ]
    );

    app.focus = Focus::Views;
    app.workspace_view_index = 0;
    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.focus, Focus::Center);
    assert_eq!(app.center_view, CenterView::Chat);
}

#[test]
fn arrows_move_focus_across_the_workspace() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "selected card");
    app.focus = Focus::Center;

    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.focus, Focus::Files);
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.focus, Focus::Center);
    assert_eq!(app.center_view, CenterView::Daily);

    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.focus, Focus::Views);
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.focus, Focus::Center);

    app.focus = Focus::Views;
    app.workspace_view_index = 0;
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.focus, Focus::Agent);
}

#[test]
fn file_tree_keeps_both_groups_and_expands_archives_on_demand() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Note.md"), "note").unwrap();
    fs::write(app.storage.archives_dir.join("Old.md"), "old").unwrap();
    app.reload_files();

    let rows = app.visible_file_rows();
    assert!(rows.contains(&FileListRow::Group(FileGroup::Notes)));
    assert!(rows.contains(&FileListRow::Group(FileGroup::Archives)));
    assert!(rows.iter().any(|row| matches!(
        row,
        FileListRow::File(index) if !app.note_files[*index].archived
    )));
    assert!(!rows.iter().any(|row| matches!(
        row,
        FileListRow::File(index) if app.note_files[*index].archived
    )));

    app.archives_expanded = true;
    assert!(app.visible_file_rows().iter().any(|row| matches!(
        row,
        FileListRow::File(index) if app.note_files[*index].archived
    )));
}

#[test]
fn file_search_includes_archives_but_move_targets_do_not() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Active.md"), "active").unwrap();
    fs::write(app.storage.archives_dir.join("Archived.md"), "old").unwrap();
    app.reload_files();
    app.files_context = FilesContext::Search;
    app.file_query = "arch".to_string();
    assert!(app.visible_file_rows().iter().any(|row| matches!(
        row,
        FileListRow::File(index) if app.note_files[*index].archived
    )));

    app.files_context = FilesContext::MoveTarget;
    app.file_query.clear();
    assert!(app.visible_file_rows().iter().all(|row| matches!(
        row,
        FileListRow::File(index) if !app.note_files[*index].archived
    )));
}

#[test]
fn views_and_agent_form_a_navigable_right_sidebar() {
    let (mut app, _directory) = make_app();
    app.todo_items = vec![TodoItem {
        checked: false,
        text: "only task".to_string(),
    }];
    app.todo_index = 0;
    app.agent_panel.push(Arc::new(AgentPanelEntry::Assistant {
        text: "final reply".to_string(),
        streaming: false,
        final_output: true,
    }));
    app.focus = Focus::Views;
    app.workspace_view_index = 0;

    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.focus, Focus::Agent);
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.focus, Focus::Center);
}

#[test]
fn enter_on_agent_panel_does_not_move_output_to_daily() {
    let (mut app, _directory) = make_app();
    let original_count = app.daily_notes.len();
    app.agent_panel = vec![
        Arc::new(AgentPanelEntry::Prompt {
            text: "User prompt".to_string(),
            muted: false,
        }),
        Arc::new(AgentPanelEntry::Assistant {
            text: "Agent final reply".to_string(),
            streaming: false,
            final_output: true,
        }),
    ];
    app.focus = Focus::Agent;

    app.handle_key(key(KeyCode::Enter));

    assert_eq!(app.daily_notes.len(), original_count);
    assert_eq!(app.agent_panel.len(), 2);
    assert_eq!(app.focus, Focus::Agent);
}

#[test]
fn files_search_uses_note_files_without_duplicate_lists() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Work.md"), "work").unwrap();
    fs::write(app.storage.data_dir.join("Personal.md"), "personal").unwrap();
    app.open_files();
    app.handle_key(key(KeyCode::Char('/')));
    assert_eq!(app.files_context, FilesContext::Search);
    app.handle_key(key(KeyCode::Char('w')));
    app.handle_key(key(KeyCode::Char('k')));
    let visible = app
        .visible_file_rows()
        .into_iter()
        .filter_map(|row| match row {
            FileListRow::File(index) => Some(index),
            FileListRow::Group(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(visible.len(), 1);
    assert_eq!(
        app.note_files[visible[0]]
            .path
            .file_stem()
            .and_then(|stem| stem.to_str()),
        Some("Work")
    );
}

#[test]
fn file_enter_opens_center_document_and_escape_returns_to_daily() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Project.md");
    fs::write(&path, "# Project\n").unwrap();
    app.open_files();
    app.selected_file = Some(path.clone());
    app.file_index = app
        .note_files
        .iter()
        .position(|file| file.path == path)
        .unwrap();
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.focus, Focus::Center);
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(path))
    );
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.center_view, CenterView::Daily);
    assert_eq!(app.focus, Focus::Center);
}

#[test]
fn file_edit_returns_terminal_command() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Project.md");
    fs::write(&path, "# Project\n").unwrap();
    app.open_files();
    app.file_index = app
        .note_files
        .iter()
        .position(|file| file.path == path)
        .unwrap();
    app.sync_selected_file();
    assert_eq!(
        app.handle_key(key(KeyCode::Char('e'))),
        Some(Command::Edit(path))
    );
}

#[test]
fn document_edit_returns_terminal_command() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Project.md");
    fs::write(&path, "# Project\n").unwrap();
    app.open_file_document(&path, DocumentReturn::Daily);

    assert_eq!(
        app.handle_key(key(KeyCode::Char('e'))),
        Some(Command::Edit(path))
    );
    assert_eq!(app.center_view, CenterView::Document);
}

#[test]
fn document_render_cache_survives_scroll_and_invalidates_on_content_or_width() {
    let mut document = Document {
        kind: DocumentKind::File(PathBuf::from("cached.md")),
        title: "Cached".to_string(),
        source: "```rust\nfn main() {}\n```".repeat(100),
        scroll: 0,
        target_line: None,
        return_to: DocumentReturn::Daily,
        render_cache: None,
    };

    assert!(document.ensure_rendered(80, crate::theme::Theme::default()));
    assert!(!document.ensure_rendered(80, crate::theme::Theme::default()));
    document.scroll = 20;
    assert!(!document.ensure_rendered(80, crate::theme::Theme::default()));

    assert!(document.ensure_rendered(100, crate::theme::Theme::default()));
    assert!(!document.ensure_rendered(100, crate::theme::Theme::default()));
    document.replace_source("updated".to_string());
    assert!(document.render_cache.is_none());
    assert!(document.ensure_rendered(100, crate::theme::Theme::default()));
}

#[test]
fn reopening_a_document_restores_its_app_level_render_cache() {
    let (mut app, _directory) = make_app();
    let first = app.storage.data_dir.join("First.md");
    let second = app.storage.data_dir.join("Second.md");
    fs::write(&first, "```rust\nfn main() {}\n```".repeat(100)).unwrap();
    fs::write(&second, "# Second").unwrap();

    app.open_file_document(&first, DocumentReturn::Daily);
    assert!(app
        .document
        .as_mut()
        .unwrap()
        .ensure_rendered(80, crate::theme::Theme::default()));
    app.open_file_document(&second, DocumentReturn::Daily);
    assert_eq!(app.document_render_lru.entries.len(), 1);

    app.open_file_document(&first, DocumentReturn::Daily);
    let document = app.document.as_mut().unwrap();
    assert!(document.render_cache.is_some());
    assert!(!document.ensure_rendered(80, crate::theme::Theme::default()));
}

#[test]
fn reopening_a_changed_document_rejects_the_stale_render_cache() {
    let (mut app, _directory) = make_app();
    let first = app.storage.data_dir.join("First.md");
    let second = app.storage.data_dir.join("Second.md");
    fs::write(&first, "old source").unwrap();
    fs::write(&second, "second source").unwrap();

    app.open_file_document(&first, DocumentReturn::Daily);
    app.document
        .as_mut()
        .unwrap()
        .ensure_rendered(80, crate::theme::Theme::default());
    app.open_file_document(&second, DocumentReturn::Daily);
    fs::write(&first, "new source").unwrap();

    app.open_file_document(&first, DocumentReturn::Daily);
    assert!(app.document.as_ref().unwrap().render_cache.is_none());
    assert_eq!(app.document.as_ref().unwrap().source, "new source");
}

#[test]
fn inactive_document_cache_follows_a_file_rename() {
    let (mut app, _directory) = make_app();
    let from = app.storage.data_dir.join("Before.md");
    let to = app.storage.data_dir.join("After.md");
    let other = app.storage.data_dir.join("Other.md");
    fs::write(&from, "cached source").unwrap();
    fs::write(&other, "other source").unwrap();

    app.open_file_document(&from, DocumentReturn::Daily);
    app.document
        .as_mut()
        .unwrap()
        .ensure_rendered(80, crate::theme::Theme::default());
    app.open_file_document(&other, DocumentReturn::Daily);
    fs::rename(&from, &to).unwrap();
    assert!(!app.retarget_open_document(&from, &to));

    app.open_file_document(&to, DocumentReturn::Daily);
    assert!(!app
        .document
        .as_mut()
        .unwrap()
        .ensure_rendered(80, crate::theme::Theme::default()));
}

#[test]
fn document_render_lru_evicts_the_oldest_entries() {
    let (mut app, _directory) = make_app();
    let paths = (0..DOCUMENT_CACHE_CAPACITY + 3)
        .map(|index| {
            let path = app.storage.data_dir.join(format!("Note{index}.md"));
            fs::write(&path, format!("note {index}")).unwrap();
            path
        })
        .collect::<Vec<_>>();

    for path in &paths {
        app.open_file_document(path, DocumentReturn::Daily);
        app.document
            .as_mut()
            .unwrap()
            .ensure_rendered(80, crate::theme::Theme::default());
    }

    assert_eq!(
        app.document_render_lru.entries.len(),
        DOCUMENT_CACHE_CAPACITY
    );
    assert!(app.document_render_lru.entries.iter().all(|entry| {
        entry.kind != DocumentKind::File(paths[0].clone())
            && entry.kind != DocumentKind::File(paths[1].clone())
    }));
}

#[test]
fn file_document_archives_and_restores_date_named_articles_as_regular_notes() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("2026-08-02.md");
    fs::write(&path, "# Project\n").unwrap();
    app.reload_files();
    app.open_file_document(&path, DocumentReturn::Daily);

    app.handle_key(key(KeyCode::Char('r')));
    assert_eq!(app.overlay, Some(Overlay::Dialog));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::RenameFile)
    );
    assert_eq!(app.pending_file.as_ref(), Some(&path));
    app.handle_key(key(KeyCode::Esc));

    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteFile));
    assert_eq!(app.pending_file.as_ref(), Some(&path));
    app.handle_key(key(KeyCode::Esc));

    app.handle_key(key(KeyCode::Char('a')));
    let archived = app.storage.archives_dir.join("2026-08-02.md");
    assert!(!path.exists());
    assert!(archived.exists());
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document
            .as_ref()
            .and_then(|document| match &document.kind {
                DocumentKind::File(path) => Some(path),
                _ => None,
            }),
        Some(&archived)
    );
    assert_eq!(app.status, "Note archived");

    app.handle_key(key(KeyCode::Char('u')));
    assert!(path.exists());
    assert!(!archived.exists());
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document
            .as_ref()
            .and_then(|document| match &document.kind {
                DocumentKind::File(path) => Some(path),
                _ => None,
            }),
        Some(&path)
    );
    assert_eq!(app.status, "Note restored");
}

#[test]
fn document_search_reuses_search_view_and_jumps_to_source_line() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Project.md");
    fs::write(&path, "# Project\nfirst needle\nother\nsecond NEEDLE\n").unwrap();
    fs::write(
        app.storage.data_dir.join("Other.md"),
        "needle outside document\n",
    )
    .unwrap();
    app.open_file_document(&path, DocumentReturn::Daily);

    app.handle_key(key(KeyCode::Char('/')));
    assert_eq!(app.center_view, CenterView::DocumentSearch);
    app.handle_paste("needle");
    assert_eq!(app.search_results.len(), 2);
    assert!(matches!(
        app.search_results[0],
        SearchHit::DocumentLine { line_no: 2, .. }
    ));

    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(app.document.as_ref().unwrap().target_line, Some(4));
}

#[test]
fn single_line_search_inputs_edit_at_the_shared_character_cursor() {
    let (mut app, _directory) = make_app();

    app.open_search();
    app.handle_paste("文档ab");
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Backspace));
    app.handle_key(key(KeyCode::Char('新')));
    app.handle_key(key(KeyCode::Delete));
    assert_eq!(app.search_query, "文档新");
    assert_eq!(app.search_cursor, 3);

    app.open_todo();
    app.handle_paste("文档ab");
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Backspace));
    app.handle_key(key(KeyCode::Char('新')));
    app.handle_key(key(KeyCode::Delete));
    assert_eq!(app.todo_query, "文档新");
    assert_eq!(app.todo_cursor, 3);

    app.focus = Focus::Files;
    app.files_context = FilesContext::Search;
    app.handle_paste("文档ab");
    app.handle_key(key(KeyCode::Home));
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Char('新')));
    assert_eq!(app.file_query, "文新档ab");
    assert_eq!(app.file_query_cursor, 2);

    app.open_command_palette();
    app.handle_paste("文档ab");
    app.handle_key(key(KeyCode::Home));
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Char('新')));
    let dialog = app.dialog.as_ref().unwrap();
    assert_eq!(dialog.input, "文新档ab");
    assert_eq!(dialog.cursor, 2);
}

#[test]
fn escape_from_document_search_returns_to_document() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Project.md");
    fs::write(&path, "# Project\n").unwrap();
    app.open_file_document(&path, DocumentReturn::Daily);
    app.handle_key(key(KeyCode::Char('/')));

    app.handle_key(key(KeyCode::Esc));

    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(path))
    );
}

#[test]
fn daily_shortcuts_exclude_archive_and_keep_move_and_new_contexts() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "file this");
    let daily_path = app
        .storage
        .daily_file_path(&app.selected_date().unwrap().to_string())
        .unwrap();
    app.handle_key(key(KeyCode::Char('a')));
    assert!(daily_path.is_file());
    assert!(app
        .storage
        .archives_dir
        .read_dir()
        .unwrap()
        .next()
        .is_none());
    app.handle_key(key(KeyCode::Char('m')));
    assert_eq!(app.focus, Focus::Files);
    assert_eq!(app.files_context, FilesContext::MoveTarget);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.files_context, FilesContext::Browse);
    assert_eq!(app.focus, Focus::Center);

    app.handle_key(key(KeyCode::Char('n')));
    assert_eq!(app.focus, Focus::Files);
    assert_eq!(app.files_context, FilesContext::NewTarget);
}

#[test]
fn file_rename_is_a_context_and_delete_is_an_overlay() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Old.md"), "old").unwrap();
    app.open_files();
    app.file_index = app
        .note_files
        .iter()
        .position(|file| file.path.ends_with("Old.md"))
        .unwrap();
    app.sync_selected_file();
    app.handle_key(key(KeyCode::Char('r')));
    assert_eq!(app.files_context, FilesContext::Rename);
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteFile));
    assert_eq!(app.focus, Focus::Files);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, None);
    assert_eq!(app.focus, Focus::Files);
}

#[test]
fn enter_confirms_file_deletion_dialog() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("DeleteMe.md");
    fs::write(&path, "delete me").unwrap();
    app.open_files();
    app.file_index = app
        .note_files
        .iter()
        .position(|file| file.path == path)
        .unwrap();
    app.sync_selected_file();

    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteFile));
    app.handle_key(key(KeyCode::Enter));

    assert!(!path.exists());
    assert_eq!(app.overlay, None);
    assert_eq!(app.status, "Deleted DeleteMe.md");
}

#[test]
fn renaming_the_open_document_retargets_it_before_workspace_reload() {
    let (mut app, _directory) = make_app();
    let from = app.storage.data_dir.join("Old.md");
    fs::write(&from, "# Old\n\nBody\n").unwrap();
    app.reload_files();
    app.file_index = app
        .note_files
        .iter()
        .position(|file| file.path == from)
        .unwrap();
    app.sync_selected_file();
    app.open_file_document(&from, DocumentReturn::Daily);
    app.focus = Focus::Files;

    app.handle_key(key(KeyCode::Char('r')));
    if let Some(dialog) = app.dialog.as_mut() {
        dialog.input = "Renamed".to_string();
        dialog.cursor = dialog.input.chars().count();
    }
    app.handle_key(key(KeyCode::Enter));

    let to = app.storage.data_dir.join("Renamed.md");
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(app.selected_file.as_ref(), Some(&to));
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(to.clone()))
    );
    assert_eq!(
        app.document
            .as_ref()
            .map(|document| document.title.as_str()),
        Some("Renamed.md")
    );

    app.reload_workspace();
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document
            .as_ref()
            .map(|document| document.source.as_str()),
        Some("# Old\n\nBody\n")
    );
    assert!(!app.status.starts_with("Reload error:"));
}

#[test]
fn agent_move_event_keeps_the_open_document_across_watcher_reload() {
    let (mut app, _directory) = make_app();
    let from = app.storage.data_dir.join("Old.md");
    fs::write(&from, "# Old\n\nBody\n").unwrap();
    app.open_file_document(&from, DocumentReturn::Daily);
    let destination_dir = app.storage.data_dir.join("moved");
    fs::create_dir(&destination_dir).unwrap();
    let to = destination_dir.join("Renamed.md");
    fs::rename(&from, &to).unwrap();

    let sender = install_agent_observable(&mut app);
    app.ai_running = true;

    // The filesystem watcher can run before the Agent event is polled.
    app.reload_workspace();
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(from))
    );

    sender
        .send(AgentEvent::FileMoved {
            from: PathBuf::from("data/Old.md"),
            to: PathBuf::from("data/moved/Renamed.md"),
        })
        .unwrap();
    sender
        .send(AgentEvent::Finished(Ok("Moved the file".to_string())))
        .unwrap();
    app.poll_agent();

    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(to.clone()))
    );
    assert_eq!(
        app.document
            .as_ref()
            .map(|document| document.source.as_str()),
        Some("# Old\n\nBody\n")
    );
    assert_eq!(
        app.document
            .as_ref()
            .map(|document| document.title.as_str()),
        Some("Renamed.md")
    );
    assert!(!app.status.starts_with("Reload error:"));
}

#[test]
fn search_result_daily_edit_returns_physical_file_command() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "needle");
    refresh_test_index(&mut app);
    app.handle_key(key(KeyCode::Char('/')));
    assert_eq!(app.center_view, CenterView::Search);
    app.handle_paste("needle");
    assert_eq!(app.search_results.len(), 1);
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| document.return_to),
        Some(DocumentReturn::Search)
    );
    let expected = app
        .storage
        .daily_file_path(&app.daily_notes[0].date.to_string())
        .unwrap();
    assert_eq!(
        app.handle_key(key(KeyCode::Char('e'))),
        Some(Command::Edit(expected))
    );
    assert_eq!(app.center_view, CenterView::Document);
}

#[test]
fn file_search_result_keeps_its_source_line_as_a_document_anchor() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Project.md");
    fs::write(&path, "# Project\n\nintro\n\nunique needle\n").unwrap();
    app.reload_files();
    refresh_test_index(&mut app);
    app.open_search();
    app.handle_paste("unique needle");
    assert_eq!(app.search_results.len(), 1);
    app.handle_key(key(KeyCode::Enter));
    let document = app.document.as_ref().expect("opened document");
    assert_eq!(document.kind, DocumentKind::File(path));
    assert_eq!(document.target_line, Some(5));
    assert_eq!(document.return_to, DocumentReturn::Search);
}

#[test]
fn full_text_search_orders_daily_active_and_archived_results() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "shared needle in Daily");
    let active = app.storage.data_dir.join("Active.md");
    let archived = app.storage.archives_dir.join("Archived.md");
    fs::write(&active, "shared needle in active note\n").unwrap();
    fs::write(&archived, "shared needle in archived note\n").unwrap();
    refresh_test_index(&mut app);

    app.open_search();
    app.handle_paste("shared needle");

    assert_eq!(app.search_results.len(), 3);
    let daily = app
        .storage
        .daily_file_path(&app.daily_notes[0].date.to_string())
        .unwrap();
    assert!(matches!(
        &app.search_results[0],
        SearchHit::FileLine { path, .. } if path == &daily
    ));
    assert!(matches!(
        &app.search_results[1],
        SearchHit::FileLine { path, .. } if path == &active
    ));
    assert!(matches!(
        &app.search_results[2],
        SearchHit::FileLine { path, .. } if path == &archived
    ));

    let daily_date = app.daily_notes[0].date;
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(
        app.document.as_ref(),
        Some(Document {
            kind: DocumentKind::Daily(date),
            target_line: Some(1),
            ..
        }) if *date == daily_date
    ));
}

#[test]
fn daily_edit_returns_its_physical_file() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "before");
    let expected = app
        .storage
        .daily_file_path(&app.daily_notes[0].date.to_string())
        .unwrap();
    assert_eq!(
        app.handle_key(key(KeyCode::Char('e'))),
        Some(Command::Edit(expected))
    );
}

#[test]
fn workspace_reload_refreshes_an_open_daily_note_from_disk() {
    let (mut app, _directory) = make_app();
    let note = app.storage.append_daily("2026-07-26", "before").unwrap();
    app.reload();
    app.open_daily_document(note.date, DocumentReturn::Daily);
    let path = app.storage.daily_file_path(&note.date.to_string()).unwrap();
    fs::write(path, "after\n").unwrap();

    app.reload_workspace();

    assert_eq!(app.document.as_ref().unwrap().source, "after");
    assert_eq!(app.daily_notes[0].body, "after");
}

#[test]
fn help_overlay_restores_underlying_state() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Views;
    app.open_help();
    app.handle_key(key(KeyCode::Down));
    assert_eq!(app.help_scroll, 1);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, None);
    assert_eq!(app.focus, Focus::Views);
    assert_eq!(app.center_view, CenterView::Daily);
}

#[test]
fn wheel_routes_by_layout_coordinates_not_focus() {
    let (mut app, _directory) = make_app();
    app.todo_items = vec![
        TodoItem {
            checked: false,
            text: "one".to_string(),
        },
        TodoItem {
            checked: false,
            text: "two".to_string(),
        },
    ];
    app.layout.center = Some(Rect::new(20, 0, 60, 20));
    app.focus = Focus::Center;
    app.center_view = CenterView::Todo;
    app.scroll = 4;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 30,
        row: 4,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.todo_index, 1);
    assert_eq!(app.scroll, 4);
    app.center_view = CenterView::Daily;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 30,
        row: 4,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.scroll, 3);
}

#[test]
fn todo_navigation_follows_grouped_display_order() {
    let (mut app, _directory) = make_app();
    app.todo_items = vec![
        TodoItem {
            checked: true,
            text: "done first in file".to_string(),
        },
        TodoItem {
            checked: false,
            text: "open second in file".to_string(),
        },
        TodoItem {
            checked: false,
            text: "open third in file".to_string(),
        },
    ];
    assert_eq!(app.visible_todo_indices(), vec![1, 2, 0]);
    app.todo_index = 1;
    app.move_todo_selection(1);
    assert_eq!(app.todo_index, 2);
    app.move_todo_selection(1);
    assert_eq!(app.todo_index, 0);

    app.todo_query = "third".to_string();
    app.ensure_visible_todo_selection();
    assert_eq!(app.visible_todo_indices(), vec![2]);
    assert_eq!(app.todo_index, 2);
}

#[test]
fn non_left_mouse_buttons_are_ignored() {
    let (mut app, _directory) = make_app();
    app.layout.files = Some(Rect::new(0, 0, 20, 20));
    app.focus = Focus::Center;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right),
        column: 2,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.focus, Focus::Center);
}

#[test]
fn link_clicks_open_external_targets_or_internal_wiki_notes() {
    let (mut app, _directory) = make_app();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::External("https://example.test".to_string()),
        area: Rect::new(4, 3, 7, 1),
    });
    let command = app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 3,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        command,
        Some(Command::OpenLink("https://example.test".to_string()))
    );

    let path = app.storage.data_dir.join("Project.md");
    fs::write(&path, "linked note").unwrap();
    app.reload_files();
    app.link_hitboxes.clear();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::WikiLink("Project".to_string()),
        area: Rect::new(4, 3, 7, 1),
    });
    assert!(app
        .handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })
        .is_none());
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(path))
    );
}

#[test]
fn local_file_clicks_open_existing_files_from_any_location() {
    let (mut app, _directory) = make_app();
    let attachment = app.storage.data_dir.join("report.pdf");
    fs::write(&attachment, b"report").unwrap();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::LocalFile(attachment.clone()),
        area: Rect::new(4, 3, 7, 1),
    });
    assert_eq!(
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }),
        Some(Command::OpenPath(fs::canonicalize(&attachment).unwrap()))
    );

    app.link_hitboxes.clear();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::LocalFile(app.storage.data_dir.join("missing.pdf")),
        area: Rect::new(4, 3, 7, 1),
    });
    assert!(app
        .handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })
        .is_none());
    assert!(app.status.starts_with("File error:"));

    let outside = tempfile::NamedTempFile::new().unwrap();
    app.link_hitboxes.clear();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::LocalFile(outside.path().to_path_buf()),
        area: Rect::new(4, 3, 7, 1),
    });
    assert_eq!(
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }),
        Some(Command::OpenPath(fs::canonicalize(outside.path()).unwrap()))
    );
}

#[test]
fn attachment_link_clicks_open_the_real_managed_file() {
    let (mut app, _directory) = make_app();
    let store = AttachmentStore::new(app.storage.attachments_dir.clone());
    store.ensure_layout().unwrap();
    let metadata = store
        .import_bytes(b"attachment payload", Some("report.pdf"))
        .unwrap();
    let uri = metadata.uri().to_string();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::Attachment(uri),
        area: Rect::new(4, 3, 7, 1),
    });
    let command = app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 3,
        modifiers: KeyModifiers::NONE,
    });
    let Some(Command::OpenPath(path)) = command else {
        panic!("expected an OpenPath command");
    };
    // The opened file is the store's real application-managed content file;
    // nothing is copied to the workspace and the custom URI never reaches a
    // web opener.
    assert_eq!(fs::read(&path).unwrap(), b"attachment payload");
    assert!(path.starts_with(app.storage.attachments_dir));
    assert_eq!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("pdf")
    );
}

#[test]
fn malformed_attachment_links_error_without_opening() {
    let (mut app, _directory) = make_app();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::Attachment("nole://attachment/not-a-uuid".to_string()),
        area: Rect::new(4, 3, 7, 1),
    });
    assert!(app
        .handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })
        .is_none());
    assert!(app.status.starts_with("Attachment error:"));
}

#[test]
fn wikilink_searches_daily_notes_and_chooses_between_all_matching_locations() {
    let (mut app, _directory) = make_app();
    app.storage
        .append_daily("2026-08-02", "daily version")
        .unwrap();
    let data = app.storage.data_dir.join("2026-08-02.md");
    let archived = app.storage.archives_dir.join("2026-08-02.md");
    fs::write(&data, "data version").unwrap();
    fs::write(&archived, "archived version").unwrap();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::WikiLink("2026-08-02".to_string()),
        area: Rect::new(1, 1, 8, 1),
    });

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(app.overlay, Some(Overlay::WikiLinkChoice));
    assert_eq!(app.wiki_link_candidates.len(), 3);
    assert_eq!(
        app.wiki_link_candidates
            .iter()
            .map(|candidate| candidate.location)
            .collect::<Vec<_>>(),
        vec![
            WikiLinkLocation::Daily,
            WikiLinkLocation::Notes,
            WikiLinkLocation::Archives,
        ]
    );
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.overlay, None);
    assert_eq!(app.document.as_ref().unwrap().source, "daily version\n");

    app.document = None;
    app.center_view = CenterView::Daily;
    app.link_hitboxes.clear();
    app.link_hitboxes.push(LinkHitbox {
        target: LinkTarget::WikiLink("New Note".to_string()),
        area: Rect::new(1, 1, 8, 1),
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    let created = app.storage.data_dir.join("New Note.md");
    assert!(created.is_file());
    assert_eq!(
        app.document.as_ref().unwrap().kind,
        DocumentKind::File(created)
    );
}

#[test]
fn wiki_index_publishes_backlinks_for_the_open_note() {
    let (mut app, _directory) = make_app();
    let target = app.storage.data_dir.join("Target.md");
    let source = app.storage.data_dir.join("Source.md");
    fs::write(&target, "body").unwrap();
    fs::write(&source, "see [[Target]]\n").unwrap();
    app.open_file_document(&target, DocumentReturn::Daily);
    assert!(app.document_backlinks.is_empty(), "index not published yet");

    app.apply_wiki_link_index(crate::wiki_link_index::WikiLinkIndex::build(&app.storage));
    assert_eq!(app.document_backlinks, vec![source.clone()]);

    // Reopening the note re-derives backlinks from the stored index.
    app.close_document();
    app.open_file_document(&target, DocumentReturn::Daily);
    assert_eq!(app.document_backlinks, vec![source]);
}

#[test]
fn clicking_a_backlink_opens_the_referencing_note() {
    let (mut app, _directory) = make_app();
    let target = app.storage.data_dir.join("Target.md");
    let source = app.storage.data_dir.join("Source.md");
    fs::write(&target, "body").unwrap();
    fs::write(&source, "see [[Target]]\n").unwrap();
    app.open_file_document(&target, DocumentReturn::Daily);
    app.apply_wiki_link_index(crate::wiki_link_index::WikiLinkIndex::build(&app.storage));
    assert_eq!(app.document_backlinks, vec![source.clone()]);

    app.backlink_hitboxes.push(BacklinkHitbox {
        path: source.clone(),
        area: Rect::new(3, 2, 10, 1),
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 4,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(source))
    );
}

#[test]
fn clicking_a_backlink_keeps_the_open_documents_return_context() {
    let (mut app, _directory) = make_app();
    let target = app.storage.data_dir.join("Target.md");
    let source = app.storage.data_dir.join("Source.md");
    fs::write(&target, "body").unwrap();
    fs::write(&source, "see [[Target]]\n").unwrap();
    app.open_file_document(&target, DocumentReturn::Search);
    app.apply_wiki_link_index(crate::wiki_link_index::WikiLinkIndex::build(&app.storage));
    assert_eq!(app.document_backlinks, vec![source.clone()]);

    app.backlink_hitboxes.push(BacklinkHitbox {
        path: source.clone(),
        area: Rect::new(3, 2, 10, 1),
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 4,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(source))
    );
    // Navigation inherits the replaced document's return context instead of
    // falling back to Daily.
    assert_eq!(
        app.document.as_ref().map(|document| document.return_to),
        Some(DocumentReturn::Search)
    );
}

#[test]
fn base_escape_and_q_both_quit() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Center;
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Some(Command::Quit));
    assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Some(Command::Quit));
}

#[test]
fn clicking_a_file_opens_it_in_center() {
    let (mut app, _directory) = make_app();
    let path = app.storage.data_dir.join("Clicked.md");
    fs::write(&path, "# Clicked\n").unwrap();
    app.open_files();
    app.file_hitboxes.push(FileHitbox {
        path: path.clone(),
        area: Rect::new(1, 1, 10, 2),
    });
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 2,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.center_view, CenterView::Document);
    assert_eq!(app.focus, Focus::Center);
    assert_eq!(
        app.document.as_ref().map(|document| &document.kind),
        Some(&DocumentKind::File(path))
    );
}

#[test]
fn move_targets_list_managed_data_notes() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Work.md"), "# Work\n").unwrap();
    add_daily_note(&mut app, "file this");
    app.handle_key(key(KeyCode::Char('m')));
    let names: Vec<String> = app
        .visible_file_rows()
        .into_iter()
        .filter_map(|row| match row {
            FileListRow::File(index) => app.note_files[index]
                .path
                .file_stem()
                .and_then(|name| name.to_str())
                .map(str::to_string),
            FileListRow::Group(_) => None,
        })
        .collect();
    assert_eq!(names, vec!["Work"]);
}

#[test]
fn rename_error_keeps_modal_context_for_retry() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("Old.md"), "old").unwrap();
    fs::write(app.storage.data_dir.join("Taken.md"), "taken").unwrap();
    app.open_files();
    app.file_index = app
        .note_files
        .iter()
        .position(|file| file.path.ends_with("Old.md"))
        .unwrap();
    app.sync_selected_file();
    app.handle_key(key(KeyCode::Char('r')));
    if let Some(dialog) = app.dialog.as_mut() {
        dialog.input = "Taken".to_string();
        dialog.cursor = dialog.input.chars().count();
    }
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.files_context, FilesContext::Rename);
    assert!(app.pending_file.is_some());
    assert!(app.status.starts_with("Error:"));
}

#[cfg(unix)]
#[test]
fn terminal_toggle_retains_one_session_and_shell_exit_discards_it() {
    let (mut app, _directory) = make_app();
    let toggle = KeyEvent::new(KeyCode::Char('`'), KeyModifiers::CONTROL);
    app.handle_key(toggle);
    assert_eq!(app.overlay, Some(Overlay::Terminal));
    let process_id = app.terminal_process_id();
    assert!(process_id.is_some());

    app.handle_key(toggle);
    assert_eq!(app.overlay, None);
    assert_eq!(app.terminal_process_id(), process_id);

    app.open_help();
    app.handle_key(toggle);
    assert_eq!(app.overlay, Some(Overlay::Terminal));
    assert_eq!(app.terminal_process_id(), process_id);
    app.handle_key(toggle);
    assert_eq!(app.overlay, Some(Overlay::Help));
    assert_eq!(app.terminal_process_id(), process_id);
    app.handle_key(key(KeyCode::Esc));

    app.handle_key(toggle);
    assert_eq!(app.overlay, Some(Overlay::Terminal));

    for character in "exit".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.handle_key(key(KeyCode::Enter));
    // Shell exit can be slow on shared CI runners; wait generously.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while app.terminal_process_id().is_some() && std::time::Instant::now() < deadline {
        app.poll_terminal();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.terminal_process_id(), None);
    assert_eq!(app.overlay, None);
}

#[test]
fn command_palette_includes_workspace_terminal() {
    let (mut app, _directory) = make_app();
    app.open_command_palette();
    assert!(app.command_matches.contains(&AppCommand::OpenTerminal));
    assert!(app
        .dialog
        .as_ref()
        .unwrap()
        .options
        .iter()
        .any(|option| option.label == "Terminal: Open"));
}

#[test]
fn file_name_modal_inputs_edit_at_the_character_cursor() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Files;
    app.files_context = FilesContext::NewTarget;
    app.new_file_input = "文件".to_string();
    app.new_file_cursor = 2;
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Char('新')));
    assert_eq!(app.new_file_input, "文新件");
    assert_eq!(app.new_file_cursor, 2);
    app.handle_key(key(KeyCode::Backspace));
    app.handle_key(key(KeyCode::Delete));
    assert_eq!(app.new_file_input, "文");
    assert_eq!(app.new_file_cursor, 1);

    app.files_context = FilesContext::Rename;
    app.rename_input = "Report".to_string();
    app.rename_cursor = app.rename_input.chars().count();
    app.handle_key(key(KeyCode::Home));
    app.handle_paste("New-");
    app.handle_key(key(KeyCode::End));
    app.handle_key(key(KeyCode::Char('2')));
    assert_eq!(app.rename_input, "New-Report2");
    assert_eq!(app.rename_cursor, app.rename_input.chars().count());
}

#[test]
fn delete_overlay_and_undo_keep_business_behavior() {
    let (mut app, _directory) = make_app();
    add_daily_note(&mut app, "remove me");
    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.overlay, Some(Overlay::ConfirmDeleteDaily));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.daily_notes.is_empty());
    app.handle_key(key(KeyCode::Char('u')));
    assert_eq!(app.daily_notes.len(), 1);
    assert_eq!(app.daily_notes[0].body, "remove me");
}

fn import_attachment(app: &mut App, name: &str, bytes: &[u8]) -> AttachmentId {
    app.attachment_store
        .import_bytes(bytes, Some(name))
        .unwrap()
        .id
}

fn refresh_attachment_refs(app: &mut App) {
    app.apply_attachment_index(
        0,
        crate::attachment_index::AttachmentReferenceIndex::build(&app.storage),
    );
}

#[test]
fn app_index_publish_reaches_the_agent_delete_handles_shared_state() {
    let (mut app, _directory) = make_app();
    // The Agent worker's delete_attachment tool observes the same shared
    // usage state the app publishes to: it starts unready and becomes ready
    // the moment the app publishes its first attachment index. This fails if
    // App::new stores a separate handle instead of the one it cloned into
    // AgentWorker::spawn.
    let worker_usage = app.agent_worker.attachment_usage().clone();
    assert!(!worker_usage.is_ready());
    refresh_attachment_refs(&mut app);
    assert!(
        worker_usage.is_ready(),
        "apply_attachment_index must publish to the handle AgentWorker::spawn received"
    );
}

#[test]
fn app_wiki_index_publish_reaches_the_agent_wiki_tools_shared_state() {
    let (mut app, _directory) = make_app();
    fs::write(app.storage.data_dir.join("A.md"), "[[B]]\n").unwrap();
    fs::write(app.storage.data_dir.join("B.md"), "body\n").unwrap();
    // The Agent worker's wiki-link tools observe the same shared index the
    // app publishes to: it starts empty and sees the note the moment the app
    // publishes its first wiki index. This fails if App::new stores a
    // separate handle instead of the one it cloned into AgentWorker::spawn.
    let worker_wiki = app.agent_worker.wiki_links().clone();
    assert!(worker_wiki
        .with_index(|index| index.backlinks(&app.storage.data_dir.join("B.md")).len())
        .is_none());
    app.apply_wiki_link_index(crate::wiki_link_index::WikiLinkIndex::build(&app.storage));
    assert_eq!(
        worker_wiki
            .with_index(|index| index.backlinks(&app.storage.data_dir.join("B.md")).len())
            .unwrap(),
        1,
        "apply_wiki_link_index must publish to the handle AgentWorker::spawn received"
    );
}

#[test]
fn attachments_view_lists_name_kind_size_and_distinct_locations() {
    let (mut app, _directory) = make_app();
    let id = import_attachment(&mut app, "report.pdf", b"%PDF-1.4x");
    let uri = crate::attachment::AttachmentUri::from_id(id).to_string();
    fs::write(
        app.storage.data_dir.join("Note.md"),
        format!("[report]({uri}) twice [report]({uri})\n"),
    )
    .unwrap();
    fs::write(
        app.storage.daily_dir.join("2026-07-28.md"),
        format!("[a]({uri})\n"),
    )
    .unwrap();
    refresh_attachment_refs(&mut app);

    app.open_attachments();
    assert_eq!(app.center_view, CenterView::Attachments);
    assert_eq!(app.focus, Focus::Center);
    assert_eq!(app.attachment_entries.len(), 1);
    let entry = &app.attachment_entries[0];
    assert_eq!(entry.id, id);
    assert_eq!(entry.name, "report.pdf");
    assert_eq!(entry.kind, "pdf");
    assert_eq!(entry.size, 9);
    assert_eq!(
        entry.locations, 2,
        "distinct notes, not occurrences, across two files"
    );
}

#[test]
fn referenced_attachment_delete_is_refused_and_reports_locations() {
    let (mut app, _directory) = make_app();
    let id = import_attachment(&mut app, "report.pdf", b"pdf-bytes");
    let uri = crate::attachment::AttachmentUri::from_id(id).to_string();
    fs::write(
        app.storage.data_dir.join("Note.md"),
        format!("[a]({uri})\n"),
    )
    .unwrap();
    refresh_attachment_refs(&mut app);

    app.open_attachments();
    app.request_delete_attachment();

    // Refused up front: no confirm dialog, attachment stays, locations shown.
    assert_eq!(app.overlay, None);
    assert_eq!(app.dialog, None);
    assert!(app.attachment_store.lookup(id).unwrap().is_some());
    assert!(app.status.contains("referenced"), "{}", app.status);
    assert!(app.status.contains("Note.md"), "{}", app.status);
}

#[test]
fn unreferenced_attachment_confirm_moves_to_trash() {
    let (mut app, _directory) = make_app();
    let id = import_attachment(&mut app, "report.pdf", b"pdf-bytes");
    refresh_attachment_refs(&mut app);

    app.open_attachments();
    assert_eq!(app.attachment_entries.len(), 1);
    app.request_delete_attachment();
    assert_eq!(app.overlay, Some(Overlay::Dialog));
    assert_eq!(
        app.dialog.as_ref().map(|dialog| dialog.purpose),
        Some(DialogPurpose::DeleteAttachment)
    );

    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.overlay, None);
    assert!(app.attachment_store.lookup(id).unwrap().is_none());
    assert!(app.attachment_entries.is_empty());
    assert!(app.status.contains("trash"), "{}", app.status);
}

#[test]
fn cancel_keeps_unreferenced_attachment() {
    let (mut app, _directory) = make_app();
    let id = import_attachment(&mut app, "report.pdf", b"pdf-bytes");
    refresh_attachment_refs(&mut app);

    app.open_attachments();
    app.request_delete_attachment();
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay, None);
    assert!(app.attachment_store.lookup(id).unwrap().is_some());
}

#[test]
fn opening_attachment_opens_the_real_managed_file() {
    let (mut app, _directory) = make_app();
    let id = import_attachment(&mut app, "report.pdf", b"pdf-bytes");
    refresh_attachment_refs(&mut app);

    app.open_attachments();
    let command = app.open_attachment_at(0);
    let Some(Command::OpenPath(path)) = command else {
        panic!("expected OpenPath command, got {command:?}");
    };
    // The opened path is the store's real application-managed content file:
    // the attachment bytes live there, no workspace/cache copy is written.
    assert_eq!(fs::read(&path).unwrap(), b"pdf-bytes");
    assert!(path.starts_with(app.storage.attachments_dir));
    assert_eq!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("pdf")
    );
    assert_eq!(
        fs::canonicalize(&path).unwrap(),
        fs::canonicalize(app.attachment_store.open(id).unwrap()).unwrap()
    );
}

#[test]
fn paste_clipboard_command_is_available_only_in_compose_documents() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Compose;
    for view in [CenterView::Daily, CenterView::Chat, CenterView::Document] {
        app.center_view = view;
        assert!(app.command_available(AppCommand::PasteClipboardAsAttachment));
    }
    app.center_view = CenterView::Todo;
    assert!(!app.command_available(AppCommand::PasteClipboardAsAttachment));
    app.center_view = CenterView::Daily;
    app.focus = Focus::Center;
    assert!(!app.command_available(AppCommand::PasteClipboardAsAttachment));
    app.focus = Focus::Compose;
    app.overlay = Some(Overlay::Terminal);
    assert!(!app.command_available(AppCommand::PasteClipboardAsAttachment));
}

#[test]
fn paste_clipboard_references_insert_at_compose_cursor_in_order() {
    let (mut app, _directory) = make_app();
    app.focus = Focus::Compose;
    app.input = "left right".to_string();
    app.input_cursor = 5;
    let first = app
        .attachment_store
        .import_bytes(b"a", Some("a.txt"))
        .unwrap();
    let second = app
        .attachment_store
        .import_bytes(b"b", Some("b.txt"))
        .unwrap();
    let references = format!(
        "{}\n{}",
        crate::attachment::markdown_embed(&first),
        crate::attachment::markdown_embed(&second)
    );
    let expected = format!("left {references}right");
    app.insert_attachment_references(&[first, second]);
    assert_eq!(app.input, expected);
    assert_eq!(app.input_cursor, 5 + references.chars().count());
    assert!(app
        .status
        .starts_with("Imported 2 attachment(s): a.txt, b.txt"));
}

#[test]
fn paste_clipboard_shortcut_is_ignored_outside_compose_and_by_terminal_overlay() {
    let (mut app, _directory) = make_app();
    app.input = "unchanged".to_string();
    app.input_cursor = app.input.chars().count();
    let shortcut = KeyEvent::new(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    app.handle_key(shortcut);
    assert_eq!(app.input, "unchanged");
    assert_eq!(
        app.attachment_store
            .list(&crate::attachment::AttachmentQuery::default())
            .unwrap()
            .total,
        0
    );

    app.focus = Focus::Compose;
    app.overlay = Some(Overlay::Terminal);
    app.handle_key(shortcut);
    assert_eq!(app.input, "unchanged");
    assert_eq!(
        app.attachment_store
            .list(&crate::attachment::AttachmentQuery::default())
            .unwrap()
            .total,
        0
    );
}
