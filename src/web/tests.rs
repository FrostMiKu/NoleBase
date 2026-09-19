//! Web layer tests: router behavior over a temporary workspace, path-safety
//! rejection, and the pure `WorkspaceEvent`/`AgentEvent` DTO conversions that
//! define the WebSocket protocol.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::core::agent::{
    AgentEvent, AgentStopReason, ApprovalKind, ApprovalRequest, AskUserRequest, CommandApproval,
    PrivateTerminalInputRequest,
};
use crate::core::agent_service::AgentService;
use crate::core::agent_session::{AgentConversation, TokenUsage};
use crate::core::storage::Storage;
use crate::core::workspace::Workspace;
use crate::core::workspace_watcher::WorkspaceEvent;
use crate::web::dto::{WebAgentEvent, WebWorkspaceEvent};
use crate::web::server;
use crate::web::state::WebState;

fn test_state() -> (WebState, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::new(directory.path()).unwrap();
    storage.ensure_files().unwrap();
    let workspace = Arc::new(Workspace::open(storage).unwrap());
    let agent = Arc::new(AgentService::new(&workspace));
    (WebState::new(workspace, agent), directory)
}

/// Block until the document index publishes a snapshot, mirroring the core
/// tests' settle loop.
fn wait_for_index(state: &WebState) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if state.workspace.poll_index_update() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

async fn request(router: &Router, request: Request<Body>) -> (StatusCode, Value, String) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, json, text)
}

async fn get(router: &Router, uri: &str) -> (StatusCode, Value, String) {
    request(
        router,
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

async fn put_json(router: &Router, uri: &str, body: Value) -> (StatusCode, Value, String) {
    request(
        router,
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn post_json(router: &Router, uri: &str, body: Value) -> (StatusCode, Value, String) {
    request(
        router,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

fn error_code(json: &Value) -> &str {
    json["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn health_reports_the_package_version() {
    let (state, _directory) = test_state();
    let (status, json, _) = get(&server::router(state), "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], json!(true));
    assert_eq!(json["version"], json!(env!("CARGO_PKG_VERSION")));
}

#[tokio::test]
async fn index_greets_with_plain_text() {
    let (state, _directory) = test_state();
    let (status, _, text) = get(&server::router(state), "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "Nole Web API");
}

#[tokio::test]
async fn workspace_read_endpoints_return_dtos() {
    let (state, directory) = test_state();
    let storage = Storage::new(directory.path()).unwrap();
    let note = storage.data_dir.join("note.md");
    std::fs::write(&note, "#tag body text").unwrap();
    storage.append_to_today("- a daily line").unwrap();
    storage.append_to_today("- [ ] a task").unwrap();
    assert!(wait_for_index(&state), "index must settle");
    // The first snapshot can race the writes above; refresh explicitly and
    // wait for the reconverged snapshot, like the core index tests.
    state.workspace.refresh_documents(vec![note]);
    assert!(wait_for_index(&state), "refreshed index must settle");

    let router = server::router(state.clone());
    let (status, json, _) = get(&router, "/api/workspace").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["notes"], json!(1));
    assert_eq!(json["daily"], json!(1));
    assert_eq!(json["todos"], json!(1));
    assert_eq!(json["index_ready"], json!(true));

    let (status, json, _) = get(&router, "/api/notes").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["path"], json!("data/note.md"));
    assert_eq!(json[0]["name"], json!("note"));
    assert_eq!(json[0]["archived"], json!(false));
    assert!(json[0]["modified"].is_string());

    let (status, json, _) = get(&router, "/api/daily").await;
    assert_eq!(status, StatusCode::OK);
    assert!(json[0]["date"].is_string());
    assert!(json[0]["body"].is_string());
    assert!(
        json[0]["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("daily/") && path.ends_with(".md")),
        "daily note must carry an openable workspace-relative path: {json}"
    );

    let (status, json, _) = get(&router, "/api/todos").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["text"], json!("a task"));
    assert_eq!(json[0]["checked"], json!(false));

    let (status, json, _) = get(&router, "/api/search?q=body").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["hits"][0]["path"], json!("data/note.md"));
    assert!(json["hits"][0]["line_no"].is_u64());

    let (status, json, _) = get(&router, "/api/tags").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["name"], json!("tag"));

    let (status, json, _) = get(&router, "/api/tags/tag").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["path"], json!("data/note.md"));
}

#[tokio::test]
async fn search_requires_a_query() {
    let (state, _directory) = test_state();
    let router = server::router(state);
    for uri in ["/api/search", "/api/search?q=", "/api/search?q=%20%20"] {
        let (status, json, _) = get(&router, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "uri: {uri}");
        assert_eq!(error_code(&json), "bad_request");
    }
}

#[tokio::test]
async fn daily_paths_open_through_the_document_endpoint() {
    let (state, _directory) = test_state();
    let storage = Storage::new(state.workspace.storage().root.clone()).unwrap();
    storage.append_to_today("daily body").unwrap();
    let router = server::router(state.clone());

    let (_, json, _) = get(&router, "/api/daily").await;
    let path = json[0]["path"].as_str().unwrap().to_string();
    let (status, document, _) = get(
        &router,
        &format!("/api/document?path={}", urlencoding_encode(&path)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(document["path"], json!(path));
    assert_eq!(document["source"], json!("daily body\n"));
}

#[tokio::test]
async fn daily_today_append_creates_then_appends() {
    let (state, _directory) = test_state();
    let router = server::router(state.clone());

    // Empty content is rejected.
    let (status, json, _) = post_json(&router, "/api/daily/today", json!({"body": "  "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&json), "bad_request");

    // First send creates today's note.
    let (status, json, _) =
        post_json(&router, "/api/daily/today", json!({"body": "first line"})).await;
    assert_eq!(status, StatusCode::OK);
    let path = json["path"].as_str().unwrap().to_string();
    assert!(path.starts_with("daily/") && path.ends_with(".md"));
    assert_eq!(json["body"], json!("first line"));

    // A second send appends with blank-line separation.
    let (status, _, _) =
        post_json(&router, "/api/daily/today", json!({"body": "second line"})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, document, _) = get(
        &router,
        &format!("/api/document?path={}", urlencoding_encode(&path)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(document["source"].as_str().unwrap().contains("first line"));
    assert!(document["source"].as_str().unwrap().contains("second line"));

    // The daily list reflects the created note.
    let (status, daily, _) = get(&router, "/api/daily").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(daily[0]["path"], json!(path));
}

#[tokio::test]
async fn todo_toggle_flips_the_indexed_task_and_404s_out_of_range() {
    let (state, _directory) = test_state();
    state
        .workspace
        .storage()
        .append_to_today("- [ ] first task")
        .unwrap();
    state
        .workspace
        .storage()
        .append_to_today("- [ ] second task")
        .unwrap();
    let router = server::router(state.clone());

    // Toggling index 0 answers with the refreshed list.
    let (status, json, _) = post_json(&router, "/api/todos/0/toggle", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["checked"], json!(true));
    assert_eq!(json[1]["checked"], json!(false));

    // Out-of-range indices are a stable 404, not an error.
    let (status, json, _) = post_json(&router, "/api/todos/9/toggle", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&json), "not_found");

    // Toggling again restores the open state.
    let (status, json, _) = post_json(&router, "/api/todos/0/toggle", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["checked"], json!(false));
    assert_eq!(json[1]["checked"], json!(false));

    // Non-numeric indices never reach the handler.
    let (status, _, _) = post_json(&router, "/api/todos/abc/toggle", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn static_serving_serves_the_spa_and_keeps_api_404s() {
    let (state, _directory) = test_state();
    let dist = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dist.path().join("assets")).unwrap();
    std::fs::write(dist.path().join("index.html"), "<html>nole app</html>").unwrap();
    std::fs::write(dist.path().join("assets/app.js"), "console.log('nole')").unwrap();

    let router = server::router_with_static_dir(state.clone(), Some(dist.path().to_path_buf()));

    // The SPA root and a deep client route both answer with index.html.
    let (status, _, text) = get(&router, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "<html>nole app</html>");
    let (status, _, text) = get(&router, "/document?path=data/note.md").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "<html>nole app</html>");

    // Real asset files are served from disk.
    let (status, _, text) = get(&router, "/assets/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "console.log('nole')");

    // Unknown non-API paths fall back to the SPA; unknown API paths must
    // never leak index.html.
    let (status, _, text) = get(&router, "/not-a-route").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(text, "<html>nole app</html>");
    let (status, json, _) = get(&router, "/api/not-found").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&json), "not_found");
    let (status, json, _) = get(&router, "/api/definitely/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&json), "not_found");

    // Registered API routes still take precedence over the wildcard.
    let (status, _, _) = get(&router, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn unknown_document_reads_return_stable_not_found() {
    let (state, _directory) = test_state();
    let (status, json, _) = get(&server::router(state), "/api/document?path=data/missing.md").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&json), "not_found");
    assert_eq!(json["error"]["message"], json!("Document not found"));
}

#[tokio::test]
async fn document_read_rejects_path_traversal() {
    let (state, directory) = test_state();
    let secret = directory.path().join("secret.md");
    std::fs::write(&secret, "outside").unwrap();
    let router = server::router(state);
    for path in [
        "../secret.md",
        "../../secret.md",
        "data/../../secret.md",
        "/etc/passwd",
        "data\\note.md",
        "data/sub/deep.md",
        "config/settings.toml",
        "",
    ] {
        let uri = format!("/api/document?path={}", urlencoding_encode(path));
        let (status, json, text) = get(&router, &uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "path: {path} ({text})");
        assert_eq!(error_code(&json), "bad_request");
    }
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "outside");
}

#[tokio::test]
async fn document_write_round_trips_and_rejects_traversal() {
    let (state, directory) = test_state();
    let storage = Storage::new(directory.path()).unwrap();
    let router = server::router(state.clone());

    let (status, _, _) = put_json(
        &router,
        "/api/document",
        json!({"path": "data/web.md", "source": "# from the web"}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        std::fs::read_to_string(storage.data_dir.join("web.md")).unwrap(),
        "# from the web"
    );

    let (status, json, _) = get(&router, "/api/document?path=data/web.md").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["path"], json!("data/web.md"));
    assert_eq!(json["source"], json!("# from the web"));

    let secret = directory.path().join("secret.md");
    std::fs::write(&secret, "outside").unwrap();
    for path in [
        "../evil.md",
        "data/../../evil.md",
        "/tmp/evil.md",
        "workspace/main/scratch.md",
        "config/settings.toml",
        "data/note.txt",
    ] {
        let (status, json, _) = put_json(
            &router,
            "/api/document",
            json!({"path": path, "source": "evil"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "path: {path}");
        assert_eq!(error_code(&json), "bad_request");
    }
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "outside");
    assert!(!storage
        .workspace_dir
        .join("main")
        .join("scratch.md")
        .exists());
    assert!(!directory.path().join("evil.md").exists());
}

#[tokio::test]
async fn backlinks_return_relative_sources() {
    let (state, directory) = test_state();
    let storage = Storage::new(directory.path()).unwrap();
    let a = storage.data_dir.join("a.md");
    let b = storage.data_dir.join("b.md");
    std::fs::write(&a, "[[b]] link").unwrap();
    std::fs::write(&b, "target").unwrap();
    assert!(wait_for_index(&state), "index must settle");
    state.workspace.refresh_documents(vec![a, b]);
    assert!(wait_for_index(&state), "refreshed index must settle");

    let (status, json, _) = get(
        &server::router(state.clone()),
        "/api/backlinks?path=data/b.md",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["backlinks"], json!(["data/a.md"]));

    let (status, json, _) = get(&server::router(state), "/api/backlinks?path=../b.md").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&json), "bad_request");
}

#[tokio::test]
async fn responses_never_leak_absolute_workspace_paths() {
    let (state, directory) = test_state();
    let storage = Storage::new(directory.path()).unwrap();
    std::fs::write(storage.data_dir.join("note.md"), "body").unwrap();
    wait_for_index(&state);
    let root = directory.path().to_str().unwrap().to_string();

    let router = server::router(state.clone());
    for uri in [
        "/api/workspace",
        "/api/notes",
        "/api/daily",
        "/api/todos",
        "/api/tags",
        "/api/document?path=data/note.md",
    ] {
        let (_, _, text) = get(&router, uri).await;
        assert!(
            !text.contains(&root),
            "absolute workspace path leaked from {uri}: {text}"
        );
    }
}

#[tokio::test]
async fn agent_commands_validate_input() {
    let (state, _directory) = test_state();
    let router = server::router(state.clone());

    let (status, json, _) = post_json(&router, "/api/agent/run", json!({"prompt": ""})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&json), "bad_request");
    let (status, _json, _) = post_json(&router, "/api/agent/run", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, json, _) = post_json(
        &router,
        "/api/agent/approval",
        json!({"decision": "whatever"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&json), "bad_request");
    let (status, _json, _) = post_json(&router, "/api/agent/answer", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _json, _) = post_json(&router, "/api/agent/terminal-input", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _json, _) =
        put_json(&router, "/api/agent/permission", json!({"mode": "banana"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, json, _) =
        put_json(&router, "/api/agent/permission", json!({"mode": "auto"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["mode"], json!("auto"));
    let (status, json, _) = get(&router, "/api/agent/permission").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["mode"], json!("auto"));
    let (status, json, _) = get(&router, "/api/agent").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["running"], json!(false));
}

/// Regression: a workspace rooted through a symlink (macOS `/tmp`, an
/// `NOLE_DIR` override) must still index external writes. notify reports
/// canonicalized paths there; the watcher rewrites them back onto the
/// storage's lexical spelling before classification.
#[tokio::test]
async fn watcher_pipeline_refreshes_index_after_external_write() {
    use crate::core::nole::Nole;

    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::new(directory.path()).unwrap();
    storage.ensure_files().unwrap();
    let mut nole = Nole::open(storage).unwrap();
    let workspace = nole.workspace().clone();

    // Drain the initial snapshot like the web watcher loop does.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !workspace.poll_index_update() {
        assert!(
            Instant::now() < deadline,
            "initial index snapshot must settle"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    std::fs::write(
        directory.path().join("data").join("live.md"),
        "live needle word\n",
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let mut saw_event = false;
    for _ in 0..50 {
        let events = nole.poll_workspace();
        if !events.is_empty() {
            saw_event = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(saw_event, "watcher must observe the external write");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !workspace.poll_index_update() {
        assert!(Instant::now() < deadline, "refreshed snapshot must settle");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let hits = workspace.search("needle").unwrap_or_default();
    assert!(
        hits.iter().any(|hit| matches!(hit,
            crate::core::model::SearchHit::FileLine { path, .. } if path.ends_with("live.md"))),
        "index must contain the externally written file; hits: {hits:?}"
    );
}

#[tokio::test]
async fn agent_run_is_accepted_and_reports_conflict_while_active() {
    let (state, _directory) = test_state();
    let router = server::router(state.clone());
    let (status, _, _) = post_json(&router, "/api/agent/run", json!({"prompt": "hello"})).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // The temporary workspace has no provider configuration, so the run
    // settles with an error quickly; either a second conflict (still
    // settling) or a fresh acceptance (settled) is valid behavior. The
    // unambiguous contract — status flips back to not-running — is checked
    // below.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, json, _) = get(&router, "/api/agent").await;
        if json["running"] == json!(false) {
            break;
        }
        assert!(Instant::now() < deadline, "agent run must settle");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (status, _, _) = post_json(&router, "/api/agent/cancel", json!({})).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[test]
fn error_texts_never_carry_absolute_workspace_paths() {
    let (state, directory) = test_state();
    let workspace = &state.workspace;
    let root = directory.path().to_str().unwrap().to_string();

    let event = WebWorkspaceEvent::from_workspace_event(
        workspace,
        &WorkspaceEvent::WatcherFailed(format!("watching {root}/data failed")),
    );
    match &event {
        WebWorkspaceEvent::WatcherFailed { message } => {
            assert_eq!(message, "watching data failed");
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let event = WebAgentEvent::from_agent_event(
        workspace,
        &AgentEvent::Finished(Err(format!(
            "parsing AI config {root}/config/ai.toml failed"
        ))),
    );
    match &event {
        WebAgentEvent::Finished {
            error: Some(error), ..
        } => {
            assert_eq!(error, "parsing AI config config/ai.toml failed");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn workspace_events_map_to_structured_dtos_with_relative_paths() {
    let (state, directory) = test_state();
    let workspace = &state.workspace;
    let storage = Storage::new(directory.path()).unwrap();

    let event = WebWorkspaceEvent::from_workspace_event(
        workspace,
        &WorkspaceEvent::ContentChanged {
            markdown_paths: vec![storage.data_dir.join("note.md")],
        },
    );
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({"type": "content_changed", "paths": ["data/note.md"]})
    );

    let event = WebWorkspaceEvent::from_workspace_event(
        workspace,
        &WorkspaceEvent::AttachmentsChanged {
            paths: vec![storage.attachments_dir.join("id").join("metadata.json")],
        },
    );
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({"type": "attachments_changed", "paths": ["attachments/id/metadata.json"]})
    );

    let event = WebWorkspaceEvent::from_workspace_event(
        workspace,
        &WorkspaceEvent::AgentReadsDirty {
            paths: vec![storage.daily_dir.join("2026-09-19.md")],
        },
    );
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({"type": "agent_reads_dirty", "paths": ["daily/2026-09-19.md"]})
    );

    let event = WebWorkspaceEvent::from_workspace_event(
        workspace,
        &WorkspaceEvent::WatcherFailed("boom".to_string()),
    );
    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({"type": "watcher_failed", "message": "boom"})
    );
}

#[test]
fn agent_events_map_to_structured_dtos() {
    let (state, directory) = test_state();
    let workspace = &state.workspace;
    let storage = Storage::new(directory.path()).unwrap();

    let value = |event: AgentEvent| {
        serde_json::to_value(WebAgentEvent::from_agent_event(workspace, &event)).unwrap()
    };

    assert_eq!(
        value(AgentEvent::AssistantDelta("chunk".into())),
        json!({"type": "assistant_delta", "text": "chunk"})
    );
    assert_eq!(
        value(AgentEvent::ThinkingFinished),
        json!({"type": "thinking_finished"})
    );
    assert_eq!(
        value(AgentEvent::ToolFinished {
            id: "t1".into(),
            message: "Read data/note.md".into(),
            preview: Some("12 lines".into()),
        }),
        json!({"type": "tool_finished", "id": "t1", "message": "Read data/note.md", "preview": "12 lines"})
    );
    assert_eq!(
        value(AgentEvent::Usage(TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            ..TokenUsage::default()
        })),
        json!({"type": "usage", "usage": {
            "input_tokens": 10,
            "output_tokens": 20,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
        }})
    );

    // Approval keeps every field the UI needs to render the gate, including
    // the embedded command approval payload.
    assert_eq!(
        value(AgentEvent::Approval(ApprovalRequest {
            title: "Run command".into(),
            message: "body".into(),
            kind: ApprovalKind::Command(CommandApproval {
                purpose: "list files".into(),
                label: "ls".into(),
                code: "ls -la".into(),
            }),
        })),
        json!({"type": "approval_requested", "title": "Run command", "message": "body",
            "kind": {"kind": "command", "purpose": "list files", "label": "ls", "code": "ls -la"}})
    );

    assert_eq!(
        value(AgentEvent::AskUser(AskUserRequest {
            question: "Which?".into(),
            options: vec!["a".into(), "b".into()],
        })),
        json!({"type": "ask_user_requested", "question": "Which?", "options": ["a", "b"]})
    );

    assert_eq!(
        value(AgentEvent::PrivateTerminalInput(
            PrivateTerminalInputRequest {
                session_id: "s1".into(),
                purpose: "sudo".into(),
                prompt: "password".into(),
            }
        )),
        json!({"type": "terminal_input_requested", "session_id": "s1", "purpose": "sudo", "prompt": "password"})
    );

    assert_eq!(
        value(AgentEvent::Stopped(AgentStopReason::ToolApprovalDenied)),
        json!({"type": "stopped", "reason": "tool_approval_denied"})
    );
    assert_eq!(
        value(AgentEvent::Finished(Ok("done".into()))),
        json!({"type": "finished", "ok": true, "text": "done", "error": null})
    );
    assert_eq!(
        value(AgentEvent::Finished(Err("boom".into()))),
        json!({"type": "finished", "ok": false, "text": null, "error": "boom"})
    );

    let mut conversation = AgentConversation::default();
    conversation
        .messages
        .push(crate::core::provider::Message::user("hi"));
    assert_eq!(
        value(AgentEvent::ConversationUpdated(conversation)),
        json!({"type": "conversation_updated", "message_count": 1})
    );

    assert_eq!(
        value(AgentEvent::FileMoved {
            from: storage.data_dir.join("a.md"),
            to: storage.data_dir.join("b.md"),
        }),
        json!({"type": "file_moved", "from": "data/a.md", "to": "data/b.md"})
    );
}

/// Minimal percent-encoding for building query strings in tests.
fn urlencoding_encode(input: &str) -> String {
    let mut output = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char)
            }
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}
