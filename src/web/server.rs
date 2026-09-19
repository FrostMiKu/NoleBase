//! The Axum server: router assembly, the loopback listener, and the
//! background task that drives the workspace watcher the way the TUI event
//! loop does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use serde_json::json;
use tower_http::services::{ServeDir, ServeFile};

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
    router_with_static_dir(state, None)
}

/// The full router with an optional static frontend directory. `Some(dir)`
/// serves the built React app with an SPA fallback (unmatched non-API GETs
/// return `index.html`); `None` keeps the plain-text API greeting so the
/// API still starts without a frontend build.
pub fn router_with_static_dir(state: web::state::WebState, static_dir: Option<PathBuf>) -> Router {
    let api = Router::new()
        .route(
            "/api/health",
            get(|| async { web::error::health_response() }),
        )
        .route("/api/workspace", get(web::workspace::overview))
        .route("/api/notes", get(web::workspace::notes))
        .route("/api/daily", get(web::workspace::daily))
        .route("/api/daily/today", post(web::workspace::append_to_today))
        .route("/api/todos", get(web::workspace::todos))
        .route(
            "/api/todos/{index}/toggle",
            post(web::workspace::toggle_todo),
        )
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
        .route("/api/agent/jobs", get(web::agent::jobs))
        .route("/api/agent/approval", post(web::agent::approval))
        .route("/api/agent/answer", post(web::agent::answer))
        .route(
            "/api/agent/terminal-input",
            post(web::agent::terminal_input),
        )
        .route("/api/agent/permission", get(web::agent::get_permission))
        .route("/api/agent/permission", put(web::agent::put_permission))
        // Registered API endpoints above take precedence; anything else
        // under /api must answer with the API error model, never the SPA
        // fallback's index.html.
        .route("/api/{*rest}", any(api_not_found))
        .with_state(state);

    match static_dir {
        Some(dir) => api
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))),
        None => api.route("/", get(|| async { web::error::index_response() })),
    }
}

/// `GET /api/unknown-path`: a stable API 404 so SPA fallback never masks a
/// mistyped API address.
async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": {"code": "not_found", "message": "Unknown API endpoint"}})),
    )
}

/// Run `nole web`: open the application, start the watcher hub, and serve
/// the API on `127.0.0.1:<port>` until the process is interrupted. When a
/// production frontend build exists at `web/dist` (relative to the working
/// directory), it is served as the React UI; without one the API still
/// starts, answering `/` with the plain-text greeting.
pub fn serve(port: u16) -> Result<()> {
    let static_dir = frontend_dist_dir();
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
        match &static_dir {
            Some(dir) => println!(
                "Nole web UI + API listening on http://127.0.0.1:{port} (static: {})",
                dir.display()
            ),
            None => {
                println!("Nole web API listening on http://127.0.0.1:{port} (no web/dist build found; API only)")
            }
        }
        axum::serve(
            listener,
            router_with_static_dir(state, static_dir),
        )
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving the web API")
    })?;
    Ok(())
}

/// Locate the built frontend at `web/dist` relative to the current working
/// directory. Absence is normal (API-only mode), not an error.
fn frontend_dist_dir() -> Option<PathBuf> {
    let candidate = PathBuf::from("web").join("dist");
    candidate.is_dir().then_some(candidate)
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
