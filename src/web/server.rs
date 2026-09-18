//! The Axum server: router assembly, the loopback listener, and the
//! background task that drives the workspace watcher the way the TUI event
//! loop does.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::routing::{get, post, put};
use axum::Router;

use crate::core::agent_service::AgentService;
use crate::core::nole::Nole;
use crate::core::storage::Storage;
use crate::core::workspace::Workspace;
use crate::core::workspace_watcher::WorkspaceEvent;
use crate::web;

/// How often the background watcher task drains filesystem events. The TUI
/// polls on its event loop cadence; the web backend has no terminal ticks,
/// so a coarse interval keeps index latency bounded without busy polling.
const WATCHER_POLL_INTERVAL: Duration = Duration::from_millis(200);

pub fn router(state: web::state::WebState) -> Router {
    Router::new()
        .route("/", get(|| async { web::error::index_response() }))
        .route(
            "/api/health",
            get(|| async { web::error::health_response() }),
        )
        .route("/api/workspace", get(web::workspace::overview))
        .route("/api/notes", get(web::workspace::notes))
        .route("/api/daily", get(web::workspace::daily))
        .route("/api/todos", get(web::workspace::todos))
        .route("/api/search", get(web::workspace::search))
        .route("/api/tags", get(web::workspace::tags))
        .route("/api/tags/{tag}", get(web::workspace::tag_documents))
        .route("/api/backlinks", get(web::workspace::backlinks))
        .route("/api/document", get(web::workspace::read_document))
        .route("/api/document", put(web::workspace::write_document))
        .route("/api/ws/workspace", get(web::ws::workspace_events))
        .route("/api/ws/agent", get(web::ws::agent_events))
        .route("/api/agent", get(web::agent::status))
        .route("/api/agent/run", post(web::agent::run))
        .route("/api/agent/cancel", post(web::agent::cancel))
        .route("/api/agent/approval", post(web::agent::approval))
        .route("/api/agent/answer", post(web::agent::answer))
        .route(
            "/api/agent/terminal-input",
            post(web::agent::terminal_input),
        )
        .route("/api/agent/permission", get(web::agent::get_permission))
        .route("/api/agent/permission", put(web::agent::put_permission))
        .with_state(state)
}

/// Run `nole web`: open the application, start the watcher hub, and serve
/// the API on `127.0.0.1:<port>` until the process is interrupted.
pub fn serve(port: u16) -> Result<()> {
    let storage = Storage::resolve_root().context("resolving Nole storage")?;
    storage.ensure_files().context("preparing Nole workspace")?;
    let nole = Nole::open(storage).context("opening Nole application")?;
    let state = web::state::WebState::new(nole.workspace().clone(), nole.agent().clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the web runtime")?;
    runtime.block_on(async move {
        tokio::spawn(watcher_loop(
            nole,
            state.workspace_events.clone(),
            state.workspace.clone(),
            state.agent.clone(),
        ));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("binding 127.0.0.1:{port}"))?;
        println!("Nole web API listening on http://127.0.0.1:{port}");
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .context("serving the web API")
    })?;
    Ok(())
}

/// The web backend's replacement for the TUI event loop: drain the watcher,
/// apply its domain events to the workspace/agent services exactly as the
/// TUI does, publish the web DTOs to WebSocket subscribers, and settle the
/// document index. Runs until the process exits.
async fn watcher_loop(
    mut nole: Nole,
    events: tokio::sync::broadcast::Sender<web::dto::WebWorkspaceEvent>,
    workspace: Arc<Workspace>,
    agent: Arc<AgentService>,
) {
    let mut interval = tokio::time::interval(WATCHER_POLL_INTERVAL);
    loop {
        interval.tick().await;
        for event in nole.poll_workspace() {
            // Mirror the TUI's response: dirty cached agent reads, then the
            // UI-facing reload (which the web client does itself from the
            // broadcast event).
            if let WorkspaceEvent::AgentReadsDirty { paths } = &event {
                if let Err(error) = agent.mark_reads_dirty(paths) {
                    eprintln!("[nole web] agent read-state error: {error:#}");
                }
            }
            let web_event = web::dto::WebWorkspaceEvent::from_workspace_event(&workspace, &event);
            let _ = events.send(web_event);
        }
        workspace.poll_index_update();
    }
}
