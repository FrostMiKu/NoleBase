//! The local tool implementations exposed to the model.
//!
//! Each submodule groups tools by theme. The tool structs are re-exported here so
//! the runtime (in [`crate::agent`]) can register them by name; private helpers stay
//! hidden inside their theme module, and only [`util`] and [`workspace_quota`]
//! are shared between themes.

mod attachment_ops;
mod explore;
mod file_ops;
mod interactive;
mod notes_tags;
mod read;
mod review;
mod skills;
mod util;
mod web;
mod workspace_quota;
mod write_policy;

pub use self::{
    attachment_ops::*, explore::*, file_ops::*, interactive::*, notes_tags::*, read::*, review::*,
    skills::*, web::*,
};
