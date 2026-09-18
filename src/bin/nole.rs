//! Thin binary entry point; all logic lives in the `nole` library. The web
//! backend and the TUI are peer presentation layers over the same core, so
//! the binary dispatches between them without either depending on the other.

fn main() -> anyhow::Result<()> {
    if nole::web::run_web_if_requested(std::env::args())? {
        return Ok(());
    }
    nole::tui::runtime::run_app()
}
