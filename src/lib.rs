//! Nole: an Agent-driven terminal knowledge management system.
//!
//! The crate is one library plus one thin binary. `runtime` owns the terminal
//! event loop; everything else is organized into domain modules.

pub mod agent;
pub mod agent_session;
pub mod app;
pub mod attachment;
pub mod attachment_index;
pub mod attachment_usage;
pub mod backend;
pub mod clipboard;
pub mod core;
pub mod document_index;
pub mod embedded_terminal;
pub mod export;
pub mod image_data;
pub mod media;
pub mod notification;
pub mod observable;
pub mod provider;
pub mod runtime;
pub mod skill;
pub mod storage;
pub mod theme;
pub mod tui;
pub mod ui;
pub mod vlist;
pub mod wiki_link_index;
pub mod workspace_index;
