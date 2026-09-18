//! Nole: an Agent-driven terminal knowledge management system.
//!
//! One library plus one thin binary. `core` holds the domain layer (data
//! model, persistence, indexing, markup, agent, provider) and must stay free
//! of terminal-rendering dependencies; `tui` holds the terminal presentation
//! layer and depends only downward on `core`.

pub mod core;
pub mod tui;
pub mod web;
