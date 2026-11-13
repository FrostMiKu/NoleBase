//! Native-client behavior through the public core API, in both build configurations.

use std::time::{Duration, Instant};

use nole::core::agent::AgentEvent;
use nole::core::agent_session::AgentSessionStart;
use nole::core::nole::Nole;
use nole::core::storage::Storage;
use nole::core::workspace_watcher::WorkspaceEvent;

#[test]
fn fresh_workspace_supports_document_edits_and_indexes_external_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    let mut app = Nole::open(Storage::new(&root).unwrap()).unwrap();
    let note = app.workspace().create_note("Client").unwrap();
    let address = app.workspace().relative_path(&note).unwrap();
    app.workspace()
        .write_document(&address, "Original content")
        .unwrap();
    assert_eq!(
        app.workspace().read_document(&address).unwrap(),
        "Original content"
    );

    // Settle the initial index so the update must arrive through the watcher.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.workspace().poll_index_update();
        if app
            .workspace()
            .search("Original")
            .is_some_and(|hits| !hits.is_empty())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "initial document was absent from search"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(&note, "External revision").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut observed_change = false;
    loop {
        observed_change |= app.poll_workspace().iter().any(|event| {
            matches!(event, WorkspaceEvent::ContentChanged { markdown_paths } if markdown_paths.contains(&note))
        });
        app.workspace().poll_index_update();
        if observed_change
            && app
                .workspace()
                .search("External")
                .is_some_and(|hits| hits.iter().any(|hit| matches!(hit, nole::core::model::SearchHit::FileLine { path, .. } if path == &note)))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "external document change did not reach the watcher and search"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        app.workspace().read_document(&address).unwrap(),
        "External revision"
    );
    assert!(app.workspace().search("Original").unwrap().is_empty());
}

#[test]
fn session_subscribers_receive_run_failures_and_history_survives_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    let app = Nole::open(Storage::new(&root).unwrap()).unwrap();
    // Exercise a real worker failure without a provider or network dependency.
    std::fs::write(
        app.workspace().storage().ai_config_path.clone(),
        "[invalid config",
    )
    .unwrap();
    let session = app.sessions().create("Client").unwrap();
    let id = session.summary().id;
    let mut first = session.subscribe();
    let mut second = session.subscribe();
    assert!(matches!(
        session.start("Inspect this note".into()).unwrap(),
        AgentSessionStart::Started(_)
    ));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let receive_failure = |receiver: &mut tokio::sync::broadcast::Receiver<AgentEvent>| {
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match receiver.recv().await.unwrap() {
                        AgentEvent::Finished(Err(message)) => break message,
                        AgentEvent::Finished(Ok(_)) => {
                            panic!("invalid configuration completed successfully")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("run failure did not reach the subscriber")
        })
    };
    let failure = receive_failure(&mut first);
    assert!(!failure.is_empty());
    assert_eq!(receive_failure(&mut second), failure);

    session.rename("Renamed client").unwrap();
    drop(session);
    drop(app);
    let reopened = Nole::open(Storage::new(&root).unwrap()).unwrap();
    let restored = reopened.sessions().get(id).unwrap();
    assert_eq!(restored.summary().title, "Renamed client");
    assert!(restored.history().iter().any(|entry| matches!(entry,
        nole::core::agent_session::AgentPanelEntry::Error(message) if message.contains(&failure))));
}
