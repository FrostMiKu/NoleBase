//! The local tool implementations exposed to the model.
//!
//! Each submodule groups tools by theme. The tool structs are re-exported here so
//! the runtime (in [`crate::agent`]) can register them by name; private helpers stay
//! hidden inside their theme module, and only [`util`] is shared between themes.

mod file_ops;
mod interactive;
mod notes_tags;
mod read;
mod util;
mod web;

pub use self::{file_ops::*, interactive::*, notes_tags::*, read::*, web::*};
