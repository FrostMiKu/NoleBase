//! Nole: an Agent-driven terminal knowledge management system.
//!
//! One library plus one thin binary. `core` holds the domain layer (data
//! model, persistence, indexing, markup, agent, provider) and must stay free
//! of terminal-rendering dependencies; `tui` holds the terminal presentation
//! layer and depends only downward on `core`. Build with
//! `default-features = false` to use the core from another client.

pub mod core;
#[cfg(feature = "tui")]
pub mod tui;
