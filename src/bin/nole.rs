//! Thin binary entry point; all logic lives in the `nole` library.

fn main() -> anyhow::Result<()> {
    nole::tui::runtime::run_app()
}
