//! Nole: an Agent-driven terminal knowledge management system.
//!
//! One shared library plus terminal and desktop entry points. `core` holds the domain layer (data
//! model, persistence, indexing, markup, agent, provider) and must stay free
//! of terminal-rendering dependencies; `tui` and `gui` hold the presentation
//! layers and depend only downward on `core`. Build with
//! `default-features = false` to use the core from another client.

pub mod core;
#[cfg(feature = "tui")]
pub mod tui;

#[cfg(feature = "gui")]
pub mod gui;
