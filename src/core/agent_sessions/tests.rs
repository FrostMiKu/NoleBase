use super::*;
use crate::core::provider::Message;
use crate::core::storage::Storage;
use std::time::Instant;

fn workspace() -> (tempfile::TempDir, Arc<Workspace>) {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::new(directory.path()).unwrap();
    storage.ensure_files().unwrap();
    let workspace = Arc::new(Workspace::open(storage).unwrap());
    (directory, workspace)
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "Session operation timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn session_notes_keep_independent_names_history_usage_and_unread_state_after_reopening() {
    let (_directory, workspace) = workspace();
    let sessions = AgentSessions::open(workspace.clone()).unwrap();
    let a = sessions.create("Research").unwrap();
    let b = sessions.create("Writing").unwrap();
    let a_id = a.summary().id;
    let b_id = b.summary().id;
    let mut conversation = crate::core::agent_session::AgentConversation::default();
    conversation.messages.push(Message::user("Question A"));
    a.observe(&AgentEvent::ConversationUpdated(conversation));
    a.observe(&AgentEvent::AssistantMessageFinished {
        text: "Answer A".into(),
        final_output: true,
    });
    a.observe(&AgentEvent::Usage(TokenUsage {
        input_tokens: 11,
        output_tokens: 3,
        ..Default::default()
    }));
    a.observe(&AgentEvent::Finished(Ok("Answer A".into())));
    a.rename("Research renamed").unwrap();
    assert!(a.summary().unread);
    assert!(!b.summary().unread);
    assert!(workspace.note_files().unwrap().is_empty());
    assert!(a.rename("  ").is_err());
    assert_eq!(a.summary().title, "Research renamed");
    let reopened = AgentSessions::open(workspace.clone()).unwrap();
    let restored = reopened.get(a_id).unwrap();
    assert_eq!(restored.summary().title, "Research renamed");
    assert!(restored.summary().unread);
    let (history, panel, usage, _, _) = restored.scope().into_parts();
    assert_eq!(history.messages[0].text(), "Question A");
    assert!(panel.iter().any(
        |entry| matches!(entry, AgentPanelEntry::Assistant { text, .. } if text == "Answer A")
    ));
    assert_eq!(usage.input_tokens, 11);
    assert!(reopened.get(b_id).unwrap().history().is_empty());
    restored.mark_read().unwrap();
    assert!(
        !AgentSessions::open(workspace)
            .unwrap()
            .get(a_id)
            .unwrap()
            .summary()
            .unread
    );
    restored.observe(&AgentEvent::Finished(Ok("Next answer".into())));
    assert!(restored.summary().unread);
}

#[test]
fn deleting_a_session_clears_its_resources_and_prevents_late_events_from_recreating_it() {
    let (_directory, workspace) = workspace();
    let sessions = AgentSessions::open(workspace.clone()).unwrap();
    let a = sessions.create("Delete").unwrap();
    let b = sessions.create("Keep").unwrap();
    let a_id = a.summary().id;
    let root = store::directory(&workspace.storage().root, a_id);
    let result = crate::core::agent::SessionResultStore::in_directory(&root).unwrap();
    result.store("large result").unwrap();
    std::fs::write(
        workspace.storage().workspace_dir.join("shared.txt"),
        "shared",
    )
    .unwrap();
    sessions.delete(a_id).unwrap();
    a.observe(&AgentEvent::Finished(Ok("late output".into())));
    assert!(a.start("later".into()).is_err());
    assert!(!root.exists());
    assert!(sessions.get(a_id).is_err());
    assert!(sessions.get(b.summary().id).is_ok());
    assert!(workspace
        .storage()
        .workspace_dir
        .join("shared.txt")
        .exists());
}

#[test]
fn two_sessions_reach_the_provider_concurrently_and_cancel_independently() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    let (_directory, workspace) = workspace();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(&workspace.storage().ai_config_path, format!("api_format = 'completions'\napi_key = 'test'\nmodel = 'test'\nbase_url = 'http://{address}'\n")).unwrap();
    let (arrived, arrivals) = std::sync::mpsc::channel();
    let (release_a, wait_a) = std::sync::mpsc::channel();
    let (release_b, wait_b) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut workers = Vec::new();
        for release in [wait_a, wait_b] {
            let (stream, _) = listener.accept().unwrap();
            let arrived = arrived.clone();
            workers.push(std::thread::spawn(move || {
                stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                let mut reader = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" { break; }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") { length = value.trim().parse().unwrap(); }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                arrived.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(10)).unwrap();
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Complete\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                let _ = write!(reader.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let sessions = AgentSessions::open(workspace).unwrap();
    let a = sessions.create("First").unwrap();
    let b = sessions.create("Second").unwrap();
    assert!(matches!(
        a.start("A".into()).unwrap(),
        AgentSessionStart::Started(_)
    ));
    assert!(matches!(
        b.start("B".into()).unwrap(),
        AgentSessionStart::Started(_)
    ));
    arrivals.recv_timeout(Duration::from_secs(10)).unwrap();
    arrivals.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(a.session.is_running() && b.session.is_running());
    a.session.cancel();
    wait_until(|| !a.session.is_running());
    assert!(b.session.is_running());
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    wait_until(|| !b.session.is_running());
    wait_until(|| b.summary().unread);
    assert!(b.history().iter().any(
        |entry| matches!(entry, AgentPanelEntry::Assistant { text, .. } if text == "Complete")
    ));
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn session_note_files_keep_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (_directory, workspace) = workspace();
    let session = AgentSessions::open(workspace.clone())
        .unwrap()
        .create("Private")
        .unwrap();
    let path =
        store::directory(&workspace.storage().root, session.summary().id).join("session.json");
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn a_session_interrupted_by_shutdown_restores_its_history_and_recovers_once() {
    let (_directory, workspace) = workspace();
    let sessions = AgentSessions::open(workspace.clone()).unwrap();
    let session = sessions.create("Interrupted").unwrap();
    let id = session.summary().id;
    let mut note = store::load(&workspace.storage().root).unwrap().remove(0);
    note.run_active = true;
    store::write(&workspace.storage().root, &note).unwrap();
    let recovered = AgentSessions::open(workspace.clone()).unwrap();
    let restored = recovered.get(id).unwrap();
    assert!(!restored.summary().running);
    assert!(restored.summary().unread);
    assert!(
        matches!(restored.history().last(), Some(AgentPanelEntry::Error(message)) if message.contains("interrupted"))
    );
    let again = AgentSessions::open(workspace).unwrap();
    assert_eq!(again.get(id).unwrap().history(), restored.history());
}
