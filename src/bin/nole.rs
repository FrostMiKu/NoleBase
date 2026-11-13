//! Executable argument dispatch and process lifecycle.

use anyhow::{bail, Result};

fn main() -> Result<()> {
    if let Some(code) = nole::core::agent::run_shell_helper_if_requested(std::env::args())? {
        std::process::exit(i32::from(code));
    }
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => println!("nole {}", env!("CARGO_PKG_VERSION")),
        Some("--help" | "-h") => {
            println!("Nole\n\nUsage: nole [--version | --help]\n\nRun nole to open your workspace.")
        }
        Some(argument) => bail!("Unknown argument: {argument}. Run `nole --help` for usage."),
        None => nole::tui::runtime::run_app()?,
    }
    Ok(())
}
