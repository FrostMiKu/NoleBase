//! Modal dialogs, command palette, skill browser, and destructive overlays.
//!
//! The dialog state machine is split across small submodules by responsibility:
//! constructing dialogs, the command palette, the skill browser, generic key
//! handling, and destructive-confirmation overlays.

mod delete;
mod handlers;
mod open;
mod palette;
mod skill;
