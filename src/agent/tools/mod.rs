//! The local tool implementations exposed to the model.
//!
//! Each submodule groups tools by theme. The tool structs are re-exported here so
//! the runtime (in [`crate::agent`]) can register them by name; private helpers stay
//! hidden inside their theme module, and only [`util`] and [`workspace_quota`]
//! are shared between themes.

mod attachment_ops;
mod calculator;
mod explore;
mod file_edit;
mod file_ops;
mod file_patch;
mod grep;
mod interactive;
mod notes_tags;
mod read;
mod review;
mod skills;
mod util;
mod web;
mod wiki_links;
mod workspace_quota;
mod write_policy;

pub use self::{
    attachment_ops::*, calculator::*, explore::*, file_ops::*, file_patch::*, grep::*,
    interactive::*, notes_tags::*, read::*, review::*, skills::*, web::*, wiki_links::*,
};
pub(crate) use write_policy::REPAIR_REQUIRED_MARKER;
