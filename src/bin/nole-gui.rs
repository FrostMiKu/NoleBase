//! Native desktop entry point.
fn main() -> anyhow::Result<()> {
    if let Some(code) = nole::core::agent::run_shell_helper_if_requested(std::env::args())? {
        std::process::exit(i32::from(code));
    }
    nole::gui::run()
}
