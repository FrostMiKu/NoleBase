//! The local tool implementations exposed to the model.
//!
//! Each submodule groups tools by theme. The tool structs are re-exported here so
//! the runtime (in [`crate::agent`]) can register them by name; private helpers stay
//! hidden inside their theme module, and only [`util`] is shared between themes.

mod explore;
mod file_ops;
mod interactive;
mod notes_tags;
mod read;
mod skills;
mod util;
mod web;
mod write_policy;

pub use self::{
    explore::*, file_ops::*, interactive::*, notes_tags::*, read::*, skills::*, web::*,
};
