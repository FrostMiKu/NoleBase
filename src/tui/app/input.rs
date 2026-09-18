//! Keyboard, mouse, and paste input routing for the workspace.
//!
//! Splitting by responsibility keeps the `App` method surface readable:
//! global dispatch, per-view key handlers, link activation, mouse routing,
//! and append/undo input plumbing.

mod append;
mod dispatch;
mod links;
mod mouse;
mod view_keys;
