//! Terminal presentation layer: app state, rendering, themes, hitboxes, and
//! the terminal runtime. Depends on [`crate::core`] only downward.

pub mod app;
pub mod backend;
pub mod clipboard;
pub mod markdown;
pub mod media;
pub mod model;
pub mod notification;
pub mod runtime;
pub mod terminal_keys;
pub mod theme;
pub mod ui;
pub mod vlist;
