//! The web presentation layer: a local, loopback-only HTTP + WebSocket API
//! over the same core services (`Workspace`, `AgentService`, `Nole`) that the
//! TUI consumes. This layer must never depend on `crate::tui`, and the core
//! must never depend on this layer.
//!
//! Boundary rules:
//!
//! - HTTP handlers stay thin: validate input, call one core service, map the
//!   result into a DTO from [`dto`].
//! - Web clients address documents by workspace-relative paths; the core
//!   resolver owns traversal rejection.
//! - Errors cross the boundary only through the stable JSON model in
//!   [`error`].

pub mod agent;
#[cfg(embed_web_ui)]
pub mod assets;
pub mod dto;
pub mod error;
pub mod server;
pub mod state;
pub mod workspace;
pub mod ws;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result};

/// The default loopback port for `nole web`.
pub const DEFAULT_WEB_PORT: u16 = 38121;

/// Handle the `web` subcommand: consume `web [--port <n>]` arguments and run
/// the local web backend to completion. Returns `Ok(false)` when the
/// arguments do not request the web backend, so the caller can fall through
/// to the TUI unchanged.
pub fn run_web_if_requested<I>(mut args: I) -> Result<bool>
where
    I: Iterator<Item = String>,
{
    args.next(); // program name
    if args.next().as_deref() != Some("web") {
        return Ok(false);
    }
    let mut port = DEFAULT_WEB_PORT;
    let mut rest = args;
    while let Some(argument) = rest.next() {
        let value = if let Some(value) = argument.strip_prefix("--port=") {
            Some(value.to_string())
        } else if argument == "--port" {
            rest.next()
        } else {
            None
        };
        match value {
            Some(value) => {
                port = value
                    .parse()
                    .with_context(|| format!("invalid web port {value:?}"))?;
            }
            None => anyhow::bail!("unknown web argument {argument:?} (expected --port <n>)"),
        }
    }
    server::serve(port).context("running Nole web backend")?;
    Ok(true)
}
