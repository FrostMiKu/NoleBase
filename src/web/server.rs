//! The Axum server: router assembly, the loopback listener, and the
//! background task that drives the workspace watcher the way the TUI event
//! loop does.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, delete, get, post, put};
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

/// Which frontend the router serves for non-API GETs.
pub enum Frontend {
    /// A production build on disk, relative to the server's working
    /// directory. Dev override: a rebuilt frontend shows up without
    /// recompiling Rust.
    Dir(PathBuf),
    /// The production build compiled into this binary
    /// (`cfg(embed_web_ui)`, set by the build script when `web/dist`
    /// exists at compile time).
    #[cfg(embed_web_ui)]
    Embedded,
    /// No frontend; `/` answers with the plain-text API greeting so the
    /// API still starts without a frontend build.
    None,
}

pub fn router(state: web::state::WebState) -> Router {
    router_with_frontend(state, Frontend::None)
}

/// The router with an explicit static frontend directory; `None` keeps the
/// plain-text API greeting. Tests use this to pin the frontend mode.
pub fn router_with_static_dir(state: web::state::WebState, static_dir: Option<PathBuf>) -> Router {
    router_with_frontend(state, static_dir.map_or(Frontend::None, Frontend::Dir))
}

/// The full router: registered API endpoints plus the chosen frontend.
/// `Dir` serves the built React app from disk with an SPA fallback
/// (unmatched non-API GETs return `index.html`); `Embedded` serves the
/// compiled-in UI the same way; `None` keeps the plain-text API greeting.
pub fn router_with_frontend(state: web::state::WebState, frontend: Frontend) -> Router {
    let api = Router::new()
        .route(
            "/api/health",
            get(|| async { web::error::health_response() }),
        )
        .route("/api/workspace", get(web::workspace::overview))
        .route(
            "/api/notes",
            get(web::workspace::notes).post(web::workspace::create_note),
        )
        .route("/api/daily", get(web::workspace::daily))
        .route("/api/daily/{date}", delete(web::workspace::delete_daily))
        .route("/api/daily/today", post(web::workspace::append_to_today))
        .route("/api/daily/{date}/move", post(web::workspace::move_daily))
        .route("/api/todos", get(web::workspace::todos))
        .route(
            "/api/todos/{index}/toggle",
            post(web::workspace::toggle_todo),
        )
        .route("/api/search", get(web::workspace::search))
        .route("/api/tags", get(web::workspace::tags))
        .route("/api/tags/{tag}", get(web::workspace::tag_documents))
        .route("/api/backlinks", get(web::workspace::backlinks))
        .route(
            "/api/document",
            get(web::workspace::read_document)
                .put(web::workspace::write_document)
                .delete(web::workspace::delete_document),
        )
        .route(
            "/api/document/rename",
            post(web::workspace::rename_document),
        )
        .route(
            "/api/document/archive",
            post(web::workspace::archive_document),
        )
        .route(
            "/api/document/restore",
            post(web::workspace::restore_document),
        )
        .route("/api/ws/workspace", get(web::ws::workspace_events))
        .route("/api/ws/agent", get(web::ws::agent_events))
        .route("/api/agent", get(web::agent::status))
        .route("/api/agent/history", get(web::agent::history))
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

    match frontend {
        Frontend::Dir(dir) => api
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))),
        #[cfg(embed_web_ui)]
        Frontend::Embedded => api.fallback(get(web::assets::serve_embedded)),
        Frontend::None => api.route("/", get(|| async { web::error::index_response() })),
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
/// the API on `127.0.0.1:<port>` until the process is interrupted. The
/// frontend comes from [`frontend`]: a `web/dist` build on disk when one
/// exists, else the UI compiled into this binary, else API-only with the
/// plain-text greeting.
pub fn serve(port: u16) -> Result<()> {
    let frontend = frontend();
    let storage = Storage::resolve_root().context("resolving Nole storage")?;
    storage.ensure_files().context("preparing Nole workspace")?;
    let nole = Nole::open(storage.clone()).context("opening Nole application")?;
    // Same restore the TUI performs: the persisted conversation continues
    // the agent's context and the persisted panel seeds the transcript.
    let restored = storage
        .load_agent_session()
        .context("loading the persisted agent session")?
        .unwrap_or_default();
    let state = web::state::WebState::new(nole.workspace().clone(), nole.agent().clone(), restored);

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
        // The session recorder: the web backend's replacement for the TUI
        // event loop's persistence duty, keeping `session.json` current.
        tokio::spawn(session_recorder_loop(
            state.agent.subscribe(),
            state.clone(),
            storage,
        ));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("binding 127.0.0.1:{port}"))?;
        match &frontend {
            Frontend::Dir(dir) => println!(
                "Nole web UI + API listening on http://127.0.0.1:{port} (static: {})",
                dir.display()
            ),
            #[cfg(embed_web_ui)]
            Frontend::Embedded => println!(
                "Nole web UI + API listening on http://127.0.0.1:{port} (embedded UI)"
            ),
            Frontend::None => println!(
                "Nole web API listening on http://127.0.0.1:{port} (no frontend build found; API only)"
            ),
        }
        axum::serve(
            listener,
            router_with_frontend(state, frontend),
        )
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("serving the web API")
    })?;
    Ok(())
}

/// Choose the frontend for `nole web`: an on-disk `web/dist` build
/// (relative to the working directory) wins so a rebuilt frontend shows up
/// without recompiling Rust; otherwise the UI compiled into this binary;
/// otherwise API-only. Absence of either is normal, not an error.
fn frontend() -> Frontend {
    match frontend_dist_dir() {
        Some(dir) => Frontend::Dir(dir),
        #[cfg(embed_web_ui)]
        None => Frontend::Embedded,
        #[cfg(not(embed_web_ui))]
        None => Frontend::None,
    }
}

/// Locate a built frontend at `web/dist` relative to the current working
/// directory.
fn frontend_dist_dir() -> Option<PathBuf> {
    let candidate = PathBuf::from("web").join("dist");
    candidate.is_dir().then_some(candidate)
}

/// Mirror the TUI event loop's session persistence: apply every agent event
/// to the web backend's transcript, and write the session scope back at the
/// same lifecycle points (conversation update, stop, finish). Runs until the
/// process exits.
async fn session_recorder_loop(
    mut events: tokio::sync::broadcast::Receiver<crate::core::agent::AgentEvent>,
    state: web::state::WebState,
    storage: Storage,
) {
    loop {
        match events.recv().await {
            Ok(event) => state.record_agent_event(&event, &storage),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
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
